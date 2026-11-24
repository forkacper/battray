//! The tray: a hidden window that owns the tray icons and their context menu, and
//! applies settings changes coming from the menu or the settings window.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, AppendMenuW, CreatePopupMenu, CreateWindowExW,
    DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, DBT_DEVTYP_DEVICEINTERFACE,
    DEV_BROADCAST_DEVICEINTERFACE_W, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyIcon,
    DestroyMenu, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, HICON, HMENU,
    IsDialogMessageW, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterDeviceNotificationW, RegisterWindowMessageW,
    SM_CXSMICON, SW_SHOWNORMAL, SetForegroundWindow, SetMenuDefaultItem, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE,
    WM_CONTEXTMENU, WM_DESTROY, WM_DEVICECHANGE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_LBUTTONUP,
    WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};

use crate::config::{self, Config, INTERVAL_CHOICES, TrayMode};
use crate::devices::ChargeState;
use crate::hid;
use crate::icon::{self, Rgb};
use crate::log;
use crate::util::{copy_wide, wide};
use crate::window::{self, View};
use crate::worker::{self, Command, DeviceState, Notify, Shared};

const CLASS: &str = "battray";

const WM_TRAY: u32 = WM_APP + 1;
const WM_UPDATED: u32 = WM_APP + 2;
const WM_DIAGNOSTICS: u32 = WM_APP + 3;
/// Sent by a second instance to open the settings window of the running one.
const WM_SHOW_WINDOW: u32 = WM_APP + 4;

/// Icon shown in the single-icon modes, and when there is nothing else to show.
const MAIN_UID: u32 = 1;
const FIRST_DEVICE_UID: u32 = 100;

const ID_NOOP: u32 = 1;
const ID_REFRESH: u32 = 2;
const ID_MODE_LOWEST: u32 = 3;
const ID_MODE_PER_DEVICE: u32 = 4;
const ID_NOTIFY: u32 = 5;
const ID_AUTOSTART: u32 = 6;
const ID_DIAGNOSTICS: u32 = 7;
const ID_OPEN_FOLDER: u32 = 8;
const ID_EXIT: u32 = 9;
const ID_OPEN: u32 = 10;
const ID_INTERVAL_BASE: u32 = 100;
const ID_SINGLE_BASE: u32 = 200;

/// Hysteresis so a level hovering around the threshold does not notify repeatedly.
const LOW_RESET_MARGIN: u8 = 5;

/// "TaskbarCreated" is broadcast when Explorer restarts; icons must be added again.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// What a tray icon currently shows, kept to skip redundant redraws.
#[derive(PartialEq)]
struct Look {
    text: String,
    color: Rgb,
    tip: String,
}

struct ShownIcon {
    look: Look,
    hicon: HICON,
}

struct App {
    hwnd: HWND,
    config: Config,
    config_path: PathBuf,
    data_dir: PathBuf,
    shared: Shared,
    worker: Sender<Command>,
    devices: Vec<DeviceState>,
    polled: bool,
    icons: HashMap<u32, ShownIcon>,
    device_uids: HashMap<String, u32>,
    next_uid: u32,
    low_notified: HashSet<String>,
}

/// A settings change or action, from the tray menu or the settings window.
pub enum Change {
    Mode(TrayMode),
    Interval(u64),
    NotifyLow(bool),
    Threshold(u8),
    Autostart(bool),
    Refresh,
    Diagnostics,
    OpenFolder,
    Exit,
}

/// Applies a change from the settings window.
pub fn apply(change: Change) {
    with_app(|app| app.apply(change));
}

/// Asks an already running instance to open its settings window.
pub fn activate_running_instance() {
    let class = wide(CLASS);
    // SAFETY: `class` is null-terminated; a null title matches any window of the class.
    let hwnd = unsafe { FindWindowW(class.as_ptr(), ptr::null()) };
    if hwnd.is_null() {
        return;
    }
    // SAFETY: lets the running instance take the foreground, which this process,
    // started by the user, is allowed to grant. PostMessageW tolerates any handle.
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
        PostMessageW(hwnd, WM_SHOW_WINDOW, 0, 0);
    }
}

/// Runs the UI until the user exits. Returns the process exit code.
pub fn run(config: Config, data_dir: PathBuf, open_window: bool) -> i32 {
    let class = wide(CLASS);
    // SAFETY: a null module name returns this executable's handle.
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    // SAFETY: `wc` is fully initialised and `class` outlives the window.
    if unsafe { RegisterClassW(&wc) } == 0 {
        log!("RegisterClassW failed: {}", std::io::Error::last_os_error());
        return 1;
    }
    // A hidden top-level window (not message-only) so it receives the TaskbarCreated broadcast.
    // SAFETY: the class was registered above; all pointers are valid or null.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        )
    };
    if hwnd.is_null() {
        log!(
            "CreateWindowExW failed: {}",
            std::io::Error::last_os_error()
        );
        return 1;
    }
    let taskbar_msg = wide("TaskbarCreated");
    // SAFETY: `taskbar_msg` is null-terminated.
    TASKBAR_CREATED.store(
        unsafe { RegisterWindowMessageW(taskbar_msg.as_ptr()) },
        Ordering::Relaxed,
    );
    register_device_notifications(hwnd);

    let shared: Shared = Arc::new(Mutex::new(Vec::new()));
    let notify = Notify {
        hwnd: hwnd as usize,
        updated_msg: WM_UPDATED,
        diagnostics_msg: WM_DIAGNOSTICS,
    };
    let worker = worker::spawn(
        notify,
        Arc::clone(&shared),
        Duration::from_secs(config.interval_secs),
        data_dir.clone(),
    );
    let mut app = App {
        hwnd,
        config,
        config_path: data_dir.join("config.ini"),
        data_dir,
        shared,
        worker,
        devices: Vec::new(),
        polled: false,
        icons: HashMap::new(),
        device_uids: HashMap::new(),
        next_uid: FIRST_DEVICE_UID,
        low_notified: HashSet::new(),
    };
    app.refresh_icons();
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    config::upgrade_autostart();
    if open_window {
        show_window();
    }

    let mut msg = MSG::default();
    // SAFETY: standard message loop on this thread's queue; `msg` is a valid MSG.
    while unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) } > 0 {
        // Tab and arrow key navigation between the settings window's controls.
        if let Some(dialog) = window::handle()
            // SAFETY: `dialog` is our window and `msg` was filled in by GetMessageW.
            && unsafe { IsDialogMessageW(dialog, &msg) } != 0
        {
            continue;
        }
        // SAFETY: `msg` was filled in by GetMessageW.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    0
}

fn register_device_notifications(hwnd: HWND) {
    let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
        dbcc_size: std::mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
        dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE,
        dbcc_reserved: 0,
        dbcc_classguid: hid::hid_guid(),
        dbcc_name: [0],
    };
    // SAFETY: `filter` is a valid DEV_BROADCAST_DEVICEINTERFACE_W and `hwnd` our window.
    let handle = unsafe {
        RegisterDeviceNotificationW(
            hwnd,
            (&raw const filter).cast(),
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
    };
    if handle.is_null() {
        log!(
            "RegisterDeviceNotificationW failed: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// Runs `f` on the app state. Does nothing before start-up finishes or if the state
/// is already borrowed further up the stack (re-entrant window messages).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            // Legacy (pre-version-4) callback: the mouse message is in the low word of lparam.
            match (lparam as u32) & 0xFFFF {
                WM_LBUTTONUP => show_window(),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_SHOW_WINDOW => {
            show_window();
            0
        }
        WM_UPDATED => {
            with_app(App::on_update);
            0
        }
        WM_DIAGNOSTICS => {
            with_app(|app| open_path(app.hwnd, &app.data_dir.join("diagnostics.txt")));
            0
        }
        WM_DEVICECHANGE => {
            if matches!(wparam as u32, DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE) {
                with_app(|app| app.send(Command::DevicesChanged));
            }
            1
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            // The small icon size may have changed: force a redraw of every icon.
            with_app(|app| {
                for shown in app.icons.values_mut() {
                    shown.look.text.clear();
                }
                app.refresh_icons();
            });
            // SAFETY: forwarding the original message arguments.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DESTROY => {
            with_app(App::remove_all_icons);
            // SAFETY: called on the thread that owns the message loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            with_app(|app| {
                app.forget_icons();
                app.refresh_icons();
            });
            0
        }
        // SAFETY: forwarding the original message arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn show_menu(hwnd: HWND) {
    // The app state must not stay borrowed while TrackPopupMenu runs its modal loop,
    // because that loop dispatches messages (e.g. WM_UPDATED) back into wndproc.
    let Some((menu, single_ids)) = with_app(App::build_menu) else {
        return;
    };
    let mut pt = POINT::default();
    // SAFETY: `pt` is writable; `hwnd` is our window and `menu` a valid popup menu.
    let cmd = unsafe {
        GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            hwnd,
            ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        cmd
    };
    match cmd as u32 {
        ID_OPEN => show_window(),
        cmd => {
            with_app(|app| app.on_command(cmd, &single_ids));
        }
    }
}

fn show_window() {
    with_app(|app| window::show(&app.view()));
}

impl App {
    fn send(&self, cmd: Command) {
        if self.worker.send(cmd).is_err() {
            log!("poller thread is gone");
        }
    }

    fn save_config(&self) {
        if let Err(e) = self.config.save(&self.config_path) {
            log!("cannot save {}: {e}", self.config_path.display());
        }
    }

    fn on_update(&mut self) {
        self.devices = self
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.polled = true;
        self.refresh_icons();
        self.check_low_battery();
        self.sync_window();
    }

    fn view(&self) -> View<'_> {
        View {
            devices: &self.devices,
            polled: self.polled,
            config: &self.config,
            autostart: config::autostart_enabled(),
        }
    }

    fn sync_window(&self) {
        if window::is_visible() {
            window::update(&self.view());
        }
    }

    /// Brings the tray icons in line with the current devices and display mode.
    fn refresh_icons(&mut self) {
        let tip_all = self.summary_tip();
        let threshold = self.config.low_threshold;
        let desired: Vec<(u32, Look)> = match &self.config.mode {
            TrayMode::Lowest => vec![(MAIN_UID, look(self.lowest(), threshold, tip_all))],
            TrayMode::Single(id) => {
                let device = self.devices.iter().find(|d| &d.id == id);
                vec![(MAIN_UID, look(device, threshold, tip_all))]
            }
            TrayMode::PerDevice if self.devices.is_empty() => {
                vec![(MAIN_UID, look(None, threshold, tip_all))]
            }
            TrayMode::PerDevice => {
                let devices = self.devices.clone();
                devices
                    .iter()
                    .map(|d| {
                        let tip = format!("{}\nbattray", device_line(d));
                        (self.uid_for(&d.id), look(Some(d), threshold, tip))
                    })
                    .collect()
            }
        };
        let stale: Vec<u32> = self
            .icons
            .keys()
            .filter(|uid| !desired.iter().any(|(d, _)| d == *uid))
            .copied()
            .collect();
        for uid in stale {
            self.remove_icon(uid);
        }
        for (uid, look) in desired {
            self.show_icon(uid, look);
        }
    }

    fn uid_for(&mut self, id: &str) -> u32 {
        if let Some(&uid) = self.device_uids.get(id) {
            return uid;
        }
        let uid = self.next_uid;
        self.next_uid += 1;
        self.device_uids.insert(id.to_owned(), uid);
        uid
    }

    /// The device with the lowest known level, or any device if none has a level yet.
    fn lowest(&self) -> Option<&DeviceState> {
        self.devices
            .iter()
            .filter(|d| d.battery.is_some())
            .min_by_key(|d| d.battery.map_or(u8::MAX, |b| b.percent))
            .or_else(|| self.devices.first())
    }

    fn summary_tip(&self) -> String {
        let mut tip = String::from("battray");
        if !self.polled {
            tip.push_str("\nSearching for devices…");
        } else if self.devices.is_empty() {
            tip.push_str("\nNo supported devices found");
        }
        for d in &self.devices {
            tip.push('\n');
            tip.push_str(&device_line(d));
        }
        tip
    }

    fn show_icon(&mut self, uid: u32, look: Look) {
        if self.icons.get(&uid).is_some_and(|shown| shown.look == look) {
            return;
        }
        // SAFETY: plain metric queries.
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) };
        let Some(hicon) = icon::render(&look.text, look.color, size) else {
            log!("cannot render tray icon");
            return;
        };
        let mut nid = self.nid(uid);
        nid.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = hicon;
        copy_wide(&mut nid.szTip, &look.tip);
        let existed = self.icons.contains_key(&uid);
        // SAFETY: `nid` is fully initialised and refers to our window.
        let ok = unsafe {
            (existed && Shell_NotifyIconW(NIM_MODIFY, &nid) != 0)
                || Shell_NotifyIconW(NIM_ADD, &nid) != 0
        };
        if !ok {
            // Explorer may not be up yet; TaskbarCreated will trigger another attempt.
            log!("Shell_NotifyIconW failed for icon {uid}");
        }
        if let Some(old) = self.icons.insert(uid, ShownIcon { look, hicon }) {
            // SAFETY: the old icon was created by us and is no longer used by the shell.
            unsafe { DestroyIcon(old.hicon) };
        }
    }

    fn remove_icon(&mut self, uid: u32) {
        let nid = self.nid(uid);
        // SAFETY: `nid` identifies one of our icons.
        unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
        if let Some(old) = self.icons.remove(&uid) {
            // SAFETY: the icon was created by us and the shell no longer shows it.
            unsafe { DestroyIcon(old.hicon) };
        }
    }

    fn remove_all_icons(&mut self) {
        let uids: Vec<u32> = self.icons.keys().copied().collect();
        for uid in uids {
            self.remove_icon(uid);
        }
    }

    /// Drops our record of shown icons after Explorer restarted and lost them.
    fn forget_icons(&mut self) {
        for (_, shown) in self.icons.drain() {
            // SAFETY: the icon was created by us; the restarted shell does not know it.
            unsafe { DestroyIcon(shown.hicon) };
        }
    }

    fn nid(&self, uid: u32) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: uid,
            ..Default::default()
        }
    }

    fn check_low_battery(&mut self) {
        if !self.config.notify_low {
            return;
        }
        let threshold = self.config.low_threshold;
        let mut alerts = Vec::new();
        for d in self.devices.iter().filter(|d| d.fresh) {
            let Some(b) = d.battery else {
                continue;
            };
            let discharging = matches!(b.charging, ChargeState::Discharging | ChargeState::Unknown);
            if discharging && b.percent <= threshold {
                if self.low_notified.insert(d.id.clone()) {
                    alerts.push((d.id.clone(), format!("{} is at {}%", d.name, b.percent)));
                }
            } else if !discharging || b.percent > threshold.saturating_add(LOW_RESET_MARGIN) {
                self.low_notified.remove(&d.id);
            }
        }
        for (id, text) in alerts {
            let uid = self
                .device_uids
                .get(&id)
                .copied()
                .filter(|uid| self.icons.contains_key(uid))
                .unwrap_or(MAIN_UID);
            self.balloon(uid, "Low battery", &text);
        }
    }

    fn balloon(&self, uid: u32, title: &str, text: &str) {
        let mut nid = self.nid(uid);
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_WARNING;
        copy_wide(&mut nid.szInfoTitle, title);
        copy_wide(&mut nid.szInfo, text);
        // SAFETY: `nid` is fully initialised and refers to one of our icons.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    /// Builds the context menu. Also returns the device IDs behind the "Only: …" items.
    fn build_menu(&mut self) -> (HMENU, Vec<String>) {
        // SAFETY: creating empty menus has no preconditions.
        let (menu, display, interval) =
            unsafe { (CreatePopupMenu(), CreatePopupMenu(), CreatePopupMenu()) };

        append(menu, MF_STRING, ID_OPEN, "Open battray");
        // SAFETY: `menu` is valid; the default item is shown in bold.
        unsafe { SetMenuDefaultItem(menu, ID_OPEN, 0) };
        separator(menu);

        if self.devices.is_empty() {
            let text = if self.polled {
                "No supported devices found"
            } else {
                "Searching for devices…"
            };
            append(menu, MF_GRAYED, ID_NOOP, text);
        }
        for d in &self.devices {
            append(menu, MF_STRING, ID_NOOP, &device_line(d));
        }
        separator(menu);
        append(menu, MF_STRING, ID_REFRESH, "Refresh now");

        let mode = &self.config.mode;
        append(
            display,
            checked(*mode == TrayMode::Lowest),
            ID_MODE_LOWEST,
            "Lowest battery",
        );
        append(
            display,
            checked(*mode == TrayMode::PerDevice),
            ID_MODE_PER_DEVICE,
            "Each device separately",
        );
        let single_ids: Vec<String> = self.devices.iter().map(|d| d.id.clone()).collect();
        if !self.devices.is_empty() {
            separator(display);
        }
        for (i, d) in self.devices.iter().enumerate() {
            let on = matches!(mode, TrayMode::Single(id) if *id == d.id);
            append(
                display,
                checked(on),
                ID_SINGLE_BASE + i as u32,
                &format!("Only {}", d.name),
            );
        }
        if let TrayMode::Single(id) = mode
            && !self.devices.iter().any(|d| &d.id == id)
        {
            append(
                display,
                MF_CHECKED | MF_GRAYED,
                ID_NOOP,
                "Only the selected device (not connected)",
            );
        }
        append_popup(menu, display, "Tray display");

        for (i, secs) in INTERVAL_CHOICES.iter().enumerate() {
            let on = self.config.interval_secs == *secs;
            let label = config::interval_label(*secs);
            append(interval, checked(on), ID_INTERVAL_BASE + i as u32, &label);
        }
        append_popup(menu, interval, "Refresh interval");

        let notify_label = format!("Low battery alerts (at {}%)", self.config.low_threshold);
        append(
            menu,
            checked(self.config.notify_low),
            ID_NOTIFY,
            &notify_label,
        );
        append(
            menu,
            checked(config::autostart_enabled()),
            ID_AUTOSTART,
            "Start with Windows",
        );
        separator(menu);
        append(menu, MF_STRING, ID_DIAGNOSTICS, "Save diagnostics report");
        append(menu, MF_STRING, ID_OPEN_FOLDER, "Open data folder");
        separator(menu);
        append(menu, MF_STRING, ID_EXIT, "Exit");
        (menu, single_ids)
    }

    fn on_command(&mut self, cmd: u32, single_ids: &[String]) {
        let change = match cmd {
            ID_REFRESH => Change::Refresh,
            ID_MODE_LOWEST => Change::Mode(TrayMode::Lowest),
            ID_MODE_PER_DEVICE => Change::Mode(TrayMode::PerDevice),
            ID_NOTIFY => Change::NotifyLow(!self.config.notify_low),
            ID_AUTOSTART => Change::Autostart(!config::autostart_enabled()),
            ID_DIAGNOSTICS => Change::Diagnostics,
            ID_OPEN_FOLDER => Change::OpenFolder,
            ID_EXIT => Change::Exit,
            c if (ID_INTERVAL_BASE..ID_SINGLE_BASE).contains(&c) => {
                match INTERVAL_CHOICES.get((c - ID_INTERVAL_BASE) as usize) {
                    Some(&secs) => Change::Interval(secs),
                    None => return,
                }
            }
            c if c >= ID_SINGLE_BASE => match single_ids.get((c - ID_SINGLE_BASE) as usize) {
                Some(id) => Change::Mode(TrayMode::Single(id.clone())),
                None => return,
            },
            _ => return,
        };
        self.apply(change);
    }

    fn apply(&mut self, change: Change) {
        match change {
            Change::Mode(mode) => {
                self.config.mode = mode;
                self.save_config();
                self.refresh_icons();
            }
            Change::Interval(secs) => {
                self.config.interval_secs = secs;
                self.save_config();
                self.send(Command::SetInterval(Duration::from_secs(secs)));
            }
            Change::NotifyLow(on) => {
                self.config.notify_low = on;
                self.low_notified.clear();
                self.save_config();
            }
            Change::Threshold(percent) => {
                if self.config.low_threshold == percent {
                    return;
                }
                self.config.low_threshold = percent;
                self.low_notified.clear();
                self.save_config();
                self.refresh_icons();
            }
            Change::Autostart(on) => {
                if let Err(e) = config::set_autostart(on) {
                    log!("cannot change autostart: {e}");
                }
            }
            Change::Refresh => self.send(Command::Refresh),
            Change::Diagnostics => self.send(Command::WriteDiagnostics(
                self.data_dir.join("diagnostics.txt"),
            )),
            Change::OpenFolder => open_path(self.hwnd, &self.data_dir),
            Change::Exit => {
                // SAFETY: `hwnd` is our window; WM_CLOSE destroys it, and WM_DESTROY
                // cleans up and ends the message loop.
                unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
                return;
            }
        }
        self.sync_window();
    }
}

/// Icon text, colour and tooltip for a device (or for "nothing to show").
fn look(device: Option<&DeviceState>, threshold: u8, tip: String) -> Look {
    let text = match device {
        None => "-".to_owned(),
        Some(DeviceState { battery: None, .. }) => "?".to_owned(),
        Some(DeviceState {
            battery: Some(b), ..
        }) => b.percent.to_string(),
    };
    let color = device.map_or(icon::GRAY, |d| level_color(d, threshold));
    Look { text, color, tip }
}

/// Status colour of a device: gray when unknown or asleep, blue when charging,
/// then red, amber or green by level.
pub fn level_color(device: &DeviceState, threshold: u8) -> Rgb {
    match device.battery {
        None => icon::GRAY,
        Some(_) if !device.fresh => icon::GRAY,
        Some(b) if matches!(b.charging, ChargeState::Charging | ChargeState::Full) => icon::BLUE,
        Some(b) if b.percent <= threshold => icon::RED,
        Some(b) if b.percent <= threshold.max(40) => icon::AMBER,
        Some(_) => icon::GREEN,
    }
}

/// One human-readable line per device, used in the menu and tooltips.
fn device_line(d: &DeviceState) -> String {
    match (d.battery, d.fresh) {
        (None, _) => format!("{}: unavailable", d.name),
        (Some(b), false) => format!("{}: {}% (asleep, last known)", d.name, b.percent),
        (Some(b), true) => {
            let state = match b.charging {
                ChargeState::Charging => " (charging)",
                ChargeState::Full => " (full)",
                ChargeState::Discharging | ChargeState::Unknown => "",
            };
            format!("{}: {}%{state}", d.name, b.percent)
        }
    }
}

/// Menu flag for a checkmark next to an item.
fn checked(on: bool) -> u32 {
    if on { MF_CHECKED } else { 0 }
}

fn append(menu: HMENU, flags: u32, id: u32, text: &str) {
    // A single '&' would turn the next character into a keyboard accelerator.
    let text = wide(&text.replace('&', "&&"));
    // SAFETY: the menu handle is valid and `text` is null-terminated.
    unsafe { AppendMenuW(menu, MF_STRING | flags, id as usize, text.as_ptr()) };
}

fn append_popup(menu: HMENU, submenu: HMENU, text: &str) {
    let text = wide(text);
    // SAFETY: both menus are valid; `menu` takes ownership of `submenu`.
    unsafe { AppendMenuW(menu, MF_POPUP, submenu as usize, text.as_ptr()) };
}

fn separator(menu: HMENU) {
    // SAFETY: the menu handle is valid.
    unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null()) };
}

fn open_path(hwnd: HWND, path: &Path) {
    let verb = wide("open");
    let file = wide(&path.to_string_lossy());
    // SAFETY: all strings are null-terminated; `hwnd` is our window.
    unsafe {
        ShellExecuteW(
            hwnd,
            verb.as_ptr(),
            file.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
}
