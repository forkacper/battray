//! The settings window: the device list and every tray menu setting, editable in place.
//! Closing or minimising it only hides it; battray keeps running in the tray.

use std::cell::RefCell;
use std::ptr;

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateFontIndirectW, DeleteObject, GetSysColor,
    GetSysColorBrush, HDC, HFONT, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    BST_CHECKED, CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDDS_SUBITEM, CDRF_DODEFAULT,
    CDRF_NOTIFYITEMDRAW, CDRF_NOTIFYSUBITEMDRAW, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES,
    ICC_UPDOWN_CLASS, INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW,
    LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT,
    LVS_NOSORTHEADER, LVS_REPORT, LVS_SINGLESEL, NM_CUSTOMDRAW, NMHDR, NMLVCUSTOMDRAW,
    UDM_SETBUDDY, UDM_SETPOS32, UDM_SETRANGE32, UDS_ALIGNRIGHT, UDS_ARROWKEYS, UDS_NOTHOUSANDS,
    UDS_SETBUDDYINT, UPDOWN_CLASSW, WC_LISTVIEWW,
};
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, GetSystemMetricsForDpi,
    SystemParametersInfoForDpi,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, BS_PUSHBUTTON, CB_ADDSTRING,
    CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE, CBS_DROPDOWNLIST, CW_USEDEFAULT,
    CreateWindowExW, DefWindowProcW, EN_CHANGE, ES_NUMBER, GetDlgCtrlID, GetDlgItem,
    GetWindowTextW, IDCANCEL, IMAGE_ICON, IsIconic, IsWindowVisible, LoadImageW, MoveWindow,
    NONCLIENTMETRICSW, RegisterClassExW, SIZE_MINIMIZED, SM_CXICON, SM_CXSMICON,
    SPI_GETNONCLIENTMETRICS, SW_HIDE, SW_RESTORE, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOZORDER, SendMessageW, SetForegroundWindow, SetWindowPos, SetWindowTextW, ShowWindow,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_NOTIFY, WM_SETFONT, WM_SIZE,
    WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows_sys::core::{PCWSTR, w};

use crate::config::{self, Config, INTERVAL_CHOICES, TrayMode};
use crate::devices::ChargeState;
use crate::icon::{self, Rgb};
use crate::log;
use crate::tray::{self, Change};
use crate::util::wide;
use crate::worker::DeviceState;

const IDC_LIST: i32 = 10;
const IDC_MODE: i32 = 11;
const IDC_INTERVAL: i32 = 12;
const IDC_NOTIFY: i32 = 13;
const IDC_THRESHOLD: i32 = 14;
const IDC_UPDOWN: i32 = 15;
const IDC_AUTOSTART: i32 = 16;
const IDC_REFRESH: i32 = 17;
const IDC_DIAGNOSTICS: i32 = 18;
const IDC_FOLDER: i32 = 19;
const IDC_EXIT: i32 = 20;
const IDC_HINT: i32 = 21;

/// Client area size in device-independent pixels.
const CLIENT_W: i32 = 420;
const CLIENT_H: i32 = 428;
const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
/// Widths of the Device, Battery and Status columns.
const COLUMNS: [(&str, i32); 3] = [("Device", 196), ("Battery", 64), ("Status", 120)];
const THRESHOLD_RANGE: (u8, u8) = (1, 99);

/// One child control: identity, window class, initial text, style and position.
struct Control {
    id: i32,
    class: PCWSTR,
    text: &'static str,
    style: u32,
    ex_style: u32,
    rect: [i32; 4],
}

/// Every control, in tab order. Positions are in device-independent pixels.
fn controls() -> [Control; 16] {
    const fn c(id: i32, class: PCWSTR, text: &'static str, style: u32, rect: [i32; 4]) -> Control {
        Control {
            id,
            class,
            text,
            style,
            ex_style: 0,
            rect,
        }
    }
    let label = w!("STATIC");
    let button = w!("BUTTON");
    let combo = w!("COMBOBOX");
    let check = BS_AUTOCHECKBOX as u32 | WS_TABSTOP;
    let push = BS_PUSHBUTTON as u32 | WS_TABSTOP;
    let dropdown = CBS_DROPDOWNLIST as u32 | WS_TABSTOP | WS_VSCROLL;
    [
        c(0, label, "Devices", 0, [16, 12, 388, 20]),
        Control {
            ex_style: WS_EX_CLIENTEDGE,
            ..c(
                IDC_LIST,
                WC_LISTVIEWW,
                "",
                LVS_REPORT | LVS_SINGLESEL | LVS_NOSORTHEADER | WS_TABSTOP,
                [16, 34, 388, 150],
            )
        },
        c(0, label, "Tray shows", 0, [16, 200, 120, 20]),
        c(IDC_MODE, combo, "", dropdown, [140, 196, 264, 240]),
        c(0, label, "Refresh every", 0, [16, 234, 120, 20]),
        c(IDC_INTERVAL, combo, "", dropdown, [140, 230, 264, 240]),
        c(
            IDC_NOTIFY,
            button,
            "Low battery alert at",
            check,
            [16, 266, 178, 24],
        ),
        Control {
            ex_style: WS_EX_CLIENTEDGE,
            ..c(
                IDC_THRESHOLD,
                w!("EDIT"),
                "",
                ES_NUMBER as u32 | WS_TABSTOP,
                [198, 266, 56, 24],
            )
        },
        c(
            IDC_UPDOWN,
            UPDOWN_CLASSW,
            "",
            UDS_ALIGNRIGHT | UDS_ARROWKEYS | UDS_NOTHOUSANDS | UDS_SETBUDDYINT,
            [0, 0, 0, 0],
        ),
        c(0, label, "%", 0, [260, 270, 24, 20]),
        c(
            IDC_AUTOSTART,
            button,
            "Start with Windows",
            check,
            [16, 298, 388, 24],
        ),
        c(IDC_REFRESH, button, "Refresh now", push, [16, 340, 124, 30]),
        c(
            IDC_DIAGNOSTICS,
            button,
            "Save diagnostics",
            push,
            [148, 340, 124, 30],
        ),
        c(
            IDC_FOLDER,
            button,
            "Open data folder",
            push,
            [280, 340, 124, 30],
        ),
        c(IDC_HINT, label, "", 0, [16, 390, 276, 20]),
        c(IDC_EXIT, button, "Exit battray", push, [300, 384, 104, 30]),
    ]
}

/// Everything the window shows.
pub struct View<'a> {
    pub devices: &'a [DeviceState],
    pub polled: bool,
    pub config: &'a Config,
    pub autostart: bool,
}

struct State {
    hwnd: HWND,
    /// Child controls, in the order of `controls()`.
    children: Vec<HWND>,
    font: HFONT,
    /// The tray mode behind each entry of the "Tray shows" list.
    modes: Vec<TrayMode>,
    /// Text colour of the Battery column, per row.
    colors: Vec<COLORREF>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The window handle, once the window exists. Used for keyboard navigation.
pub fn handle() -> Option<HWND> {
    STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| s.hwnd))
}

pub fn is_visible() -> bool {
    // SAFETY: IsWindowVisible accepts any window handle.
    handle().is_some_and(|hwnd| unsafe { IsWindowVisible(hwnd) } != 0)
}

/// Shows the window, creating it on first use, and brings it to the front.
pub fn show(view: &View) {
    let hwnd = match handle() {
        Some(hwnd) => hwnd,
        None => match create() {
            Some(hwnd) => hwnd,
            None => return,
        },
    };
    fill(hwnd, view);
    // SAFETY: `hwnd` is our window.
    unsafe {
        ShowWindow(
            hwnd,
            if IsIconic(hwnd) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        SetForegroundWindow(hwnd);
    }
}

/// Refreshes the window contents if it is open.
pub fn update(view: &View) {
    if is_visible()
        && let Some(hwnd) = handle()
    {
        fill(hwnd, view);
    }
}

fn create() -> Option<HWND> {
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES | ICC_LISTVIEW_CLASSES | ICC_UPDOWN_CLASS,
    };
    // SAFETY: `icc` is a valid INITCOMMONCONTROLSEX.
    unsafe { InitCommonControlsEx(&icc) };

    let class = w!("battray-settings");
    // SAFETY: a null module name returns this executable's handle.
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    // SAFETY: plain metric queries.
    let (big, small) = unsafe {
        let dpi = GetDpiForSystem();
        (
            GetSystemMetricsForDpi(SM_CXICON, dpi),
            GetSystemMetricsForDpi(SM_CXSMICON, dpi),
        )
    };
    // SAFETY: resource 1 is the application icon embedded by build.rs; a missing icon
    // yields null, which the class accepts.
    let (icon, icon_small) = unsafe {
        (
            LoadImageW(instance, 1 as PCWSTR, IMAGE_ICON, big, big, 0),
            LoadImageW(instance, 1 as PCWSTR, IMAGE_ICON, small, small, 0),
        )
    };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        hIcon: icon,
        hIconSm: icon_small,
        // SAFETY: GetSysColorBrush returns a shared system brush that needs no cleanup.
        hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
        lpszClassName: class,
        ..Default::default()
    };
    // SAFETY: `wc` is fully initialised; `class` is a static string.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        log!(
            "RegisterClassExW failed: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    // SAFETY: the class was registered above; all pointers are valid or null.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class,
            w!("battray"),
            STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CLIENT_W,
            CLIENT_H,
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
        return None;
    }
    let mut children = Vec::new();
    for control in controls() {
        let text = wide(control.text);
        // SAFETY: `hwnd` is our window; the class names are system classes registered
        // by InitCommonControlsEx; `text` is null-terminated.
        let child = unsafe {
            CreateWindowExW(
                control.ex_style,
                control.class,
                text.as_ptr(),
                WS_CHILD | WS_VISIBLE | control.style,
                0,
                0,
                0,
                0,
                hwnd,
                control.id as isize as _,
                instance,
                ptr::null(),
            )
        };
        children.push(child);
    }
    init_controls(hwnd);
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd,
            children,
            font: ptr::null_mut(),
            modes: Vec::new(),
            colors: Vec::new(),
        });
    });
    // SAFETY: `hwnd` is our window.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    resize(hwnd, dpi, None);
    layout(hwnd, dpi);
    Some(hwnd)
}

/// One-time contents: list columns, interval choices, threshold range.
fn init_controls(hwnd: HWND) {
    let list = item(hwnd, IDC_LIST);
    let ex = (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize;
    send(list, LVM_SETEXTENDEDLISTVIEWSTYLE, ex as usize, ex);
    for (i, (title, width)) in COLUMNS.iter().enumerate() {
        let mut text = wide(title);
        let column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: *width,
            pszText: text.as_mut_ptr(),
            ..Default::default()
        };
        // SAFETY: `column` and its text stay alive for the duration of the call.
        unsafe { SendMessageW(list, LVM_INSERTCOLUMNW, i, &raw const column as isize) };
    }
    let interval = item(hwnd, IDC_INTERVAL);
    for secs in INTERVAL_CHOICES {
        add_string(interval, &config::interval_label(secs));
    }
    let updown = item(hwnd, IDC_UPDOWN);
    let (min, max) = THRESHOLD_RANGE;
    send(updown, UDM_SETRANGE32, usize::from(min), isize::from(max));
    set_text(
        item(hwnd, IDC_HINT),
        &format!(
            "battray {} · closing keeps it in the tray",
            env!("CARGO_PKG_VERSION")
        ),
    );
}

/// Sizes the window so its client area is `CLIENT_W` x `CLIENT_H` at `dpi`.
fn resize(hwnd: HWND, dpi: u32, origin: Option<(i32, i32)>) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: px(CLIENT_W, dpi),
        bottom: px(CLIENT_H, dpi),
    };
    // SAFETY: `rect` is writable.
    unsafe { AdjustWindowRectExForDpi(&mut rect, STYLE, 0, 0, dpi) };
    let (x, y) = origin.unwrap_or((0, 0));
    let mut flags = SWP_NOZORDER | SWP_NOACTIVATE;
    if origin.is_none() {
        flags |= SWP_NOMOVE;
    }
    // SAFETY: `hwnd` is our window.
    unsafe {
        SetWindowPos(
            hwnd,
            ptr::null_mut(),
            x,
            y,
            rect.right - rect.left,
            rect.bottom - rect.top,
            flags,
        )
    };
}

/// Positions every control and applies the system message font for `dpi`.
fn layout(hwnd: HWND, dpi: u32) {
    let font = message_font(dpi);
    let children = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|state| state.children.clone())
            .unwrap_or_default()
    });
    for (child, control) in children.into_iter().zip(controls()) {
        let [x, y, w, h] = control.rect;
        // SAFETY: `child` is one of our controls.
        unsafe { MoveWindow(child, px(x, dpi), px(y, dpi), px(w, dpi), px(h, dpi), 1) };
        send(child, WM_SETFONT, font as usize, 1);
    }
    // The up-down control re-attaches to the moved edit box.
    send(
        item(hwnd, IDC_UPDOWN),
        UDM_SETBUDDY,
        item(hwnd, IDC_THRESHOLD) as usize,
        0,
    );
    let list = item(hwnd, IDC_LIST);
    for (i, (_, width)) in COLUMNS.iter().enumerate() {
        send(list, LVM_SETCOLUMNWIDTH, i, px(*width, dpi) as isize);
    }
    let old = STATE.with(|s| {
        s.borrow_mut()
            .as_mut()
            .map(|state| std::mem::replace(&mut state.font, font))
    });
    if let Some(old) = old
        && !old.is_null()
    {
        // SAFETY: the old font was created by us and no control uses it any more.
        unsafe { DeleteObject(old) };
    }
}

fn message_font(dpi: u32) -> HFONT {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: `metrics` is writable and its size is set.
    let ok = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            (&raw mut metrics).cast(),
            0,
            dpi,
        )
    };
    if ok == 0 {
        return ptr::null_mut();
    }
    // SAFETY: `lfMessageFont` is a valid LOGFONTW filled in above.
    unsafe { CreateFontIndirectW(&metrics.lfMessageFont) }
}

/// Puts the current devices and settings into the controls.
fn fill(hwnd: HWND, view: &View) {
    let threshold = view.config.low_threshold;

    let list = item(hwnd, IDC_LIST);
    send(list, LVM_DELETEALLITEMS, 0, 0);
    let mut colors = Vec::new();
    if view.devices.is_empty() {
        let text = if view.polled {
            "No supported devices found"
        } else {
            "Searching for devices…"
        };
        insert_row(list, 0, [text, "", ""]);
        // SAFETY: plain colour query.
        colors.push(unsafe { GetSysColor(COLOR_GRAYTEXT) });
    }
    for (i, d) in view.devices.iter().enumerate() {
        let level = d
            .battery
            .map(|b| format!("{}%", b.percent))
            .unwrap_or_default();
        insert_row(list, i, [&d.name, &level, status(d)]);
        colors.push(text_color(tray::level_color(d, threshold)));
    }

    let mode = item(hwnd, IDC_MODE);
    send(mode, CB_RESETCONTENT, 0, 0);
    let mut modes = vec![TrayMode::Lowest, TrayMode::PerDevice];
    add_string(mode, "Lowest battery");
    add_string(mode, "Each device separately");
    for d in view.devices {
        add_string(mode, &format!("Only {}", d.name));
        modes.push(TrayMode::Single(d.id.clone()));
    }
    if let TrayMode::Single(id) = &view.config.mode
        && !view.devices.iter().any(|d| &d.id == id)
    {
        add_string(mode, "Only the selected device (not connected)");
        modes.push(view.config.mode.clone());
    }
    let selected = modes.iter().position(|m| *m == view.config.mode);
    send(mode, CB_SETCURSEL, selected.unwrap_or(usize::MAX), 0);

    let interval = INTERVAL_CHOICES
        .iter()
        .position(|&s| s == view.config.interval_secs);
    send(
        item(hwnd, IDC_INTERVAL),
        CB_SETCURSEL,
        interval.unwrap_or(usize::MAX),
        0,
    );

    set_check(item(hwnd, IDC_NOTIFY), view.config.notify_low);
    let edit = item(hwnd, IDC_THRESHOLD);
    // Leave the box alone while it already shows this value, so typing is not disturbed.
    if read_number(edit) != Some(threshold) {
        send(
            item(hwnd, IDC_UPDOWN),
            UDM_SETPOS32,
            0,
            isize::from(threshold),
        );
    }
    let enabled = i32::from(view.config.notify_low);
    // SAFETY: both are our child windows.
    unsafe {
        EnableWindow(edit, enabled);
        EnableWindow(item(hwnd, IDC_UPDOWN), enabled);
    }
    set_check(item(hwnd, IDC_AUTOSTART), view.autostart);

    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.modes = modes;
            state.colors = colors;
        }
    });
}

fn status(d: &DeviceState) -> &'static str {
    match (d.battery, d.fresh) {
        (None, _) => "Unavailable",
        (Some(_), false) => "Asleep, last known",
        (Some(b), true) => match b.charging {
            ChargeState::Charging => "Charging",
            ChargeState::Full => "Full",
            ChargeState::Discharging | ChargeState::Unknown => "",
        },
    }
}

/// Darker versions of the tray colours, readable on a light window background.
fn text_color(rgb: Rgb) -> COLORREF {
    let rgb = match rgb {
        icon::GREEN => 0x1A8F3C,
        icon::AMBER => 0xB86E00,
        icon::RED => 0xD01F1F,
        icon::BLUE => 0x0A6ED1,
        _ => 0x808080,
    };
    // COLORREF is 0x00BBGGRR.
    ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // Escape, translated by IsDialogMessageW.
        WM_COMMAND if wparam & 0xFFFF == IDCANCEL as usize => {
            // SAFETY: `hwnd` is our window.
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            0
        }
        WM_COMMAND => {
            if let Some(change) = command(hwnd, wparam) {
                tray::apply(change);
            }
            0
        }
        WM_NOTIFY => {
            // SAFETY: for WM_NOTIFY, lparam points to an NMHDR that lives for the call.
            let header = unsafe { &*(lparam as *const NMHDR) };
            if header.idFrom == IDC_LIST as usize && header.code == NM_CUSTOMDRAW {
                // SAFETY: list view custom draw notifications carry an NMLVCUSTOMDRAW.
                let draw = unsafe { &mut *(lparam as *mut NMLVCUSTOMDRAW) };
                return custom_draw(draw) as LRESULT;
            }
            // SAFETY: forwarding the original message arguments.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CTLCOLORSTATIC => {
            let hdc = wparam as HDC;
            // SAFETY: `hdc` is the control's device context for this paint; the brush
            // is a shared system brush.
            unsafe {
                SetBkMode(hdc, TRANSPARENT as i32);
                if GetDlgCtrlID(lparam as HWND) == IDC_HINT {
                    SetTextColor(hdc, GetSysColor(COLOR_GRAYTEXT));
                }
                GetSysColorBrush(COLOR_WINDOW) as LRESULT
            }
        }
        WM_SIZE if wparam as u32 == SIZE_MINIMIZED => {
            // SAFETY: `hwnd` is our window.
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            0
        }
        WM_CLOSE => {
            // SAFETY: `hwnd` is our window.
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            0
        }
        WM_DPICHANGED => {
            // SAFETY: for WM_DPICHANGED, lparam points to the suggested window rectangle.
            let suggested = unsafe { &*(lparam as *const RECT) };
            let dpi = (wparam >> 16) as u32 & 0xFFFF;
            resize(hwnd, dpi, Some((suggested.left, suggested.top)));
            layout(hwnd, dpi);
            0
        }
        // SAFETY: forwarding the original message arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Turns a control notification into a settings change.
fn command(hwnd: HWND, wparam: WPARAM) -> Option<Change> {
    let id = (wparam & 0xFFFF) as i32;
    let code = ((wparam >> 16) & 0xFFFF) as u32;
    let control = item(hwnd, id);
    match (id, code) {
        (IDC_MODE, CBN_SELCHANGE) => {
            let index = send(control, CB_GETCURSEL, 0, 0);
            let mode = STATE.with(|s| {
                let state = s.try_borrow().ok()?;
                state
                    .as_ref()?
                    .modes
                    .get(usize::try_from(index).ok()?)
                    .cloned()
            })?;
            Some(Change::Mode(mode))
        }
        (IDC_INTERVAL, CBN_SELCHANGE) => {
            let index = usize::try_from(send(control, CB_GETCURSEL, 0, 0)).ok()?;
            INTERVAL_CHOICES.get(index).map(|&s| Change::Interval(s))
        }
        (IDC_NOTIFY, BN_CLICKED) => Some(Change::NotifyLow(is_checked(control))),
        (IDC_AUTOSTART, BN_CLICKED) => Some(Change::Autostart(is_checked(control))),
        (IDC_THRESHOLD, EN_CHANGE) => {
            let (min, max) = THRESHOLD_RANGE;
            read_number(control)
                .filter(|v| (min..=max).contains(v))
                .map(Change::Threshold)
        }
        (IDC_REFRESH, BN_CLICKED) => Some(Change::Refresh),
        (IDC_DIAGNOSTICS, BN_CLICKED) => Some(Change::Diagnostics),
        (IDC_FOLDER, BN_CLICKED) => Some(Change::OpenFolder),
        (IDC_EXIT, BN_CLICKED) => Some(Change::Exit),
        _ => None,
    }
}

/// Colours the Battery column; everything else draws as usual.
fn custom_draw(draw: &mut NMLVCUSTOMDRAW) -> u32 {
    match draw.nmcd.dwDrawStage {
        CDDS_PREPAINT => CDRF_NOTIFYITEMDRAW,
        CDDS_ITEMPREPAINT => CDRF_NOTIFYSUBITEMDRAW,
        stage if stage == CDDS_ITEMPREPAINT | CDDS_SUBITEM => {
            let row = draw.nmcd.dwItemSpec;
            let color = STATE.with(|s| {
                let state = s.try_borrow().ok()?;
                state.as_ref()?.colors.get(row).copied()
            });
            draw.clrText = match (draw.iSubItem, color) {
                (1, Some(color)) => color,
                // SAFETY: plain colour query.
                _ => unsafe { GetSysColor(COLOR_WINDOWTEXT) },
            };
            CDRF_DODEFAULT
        }
        _ => CDRF_DODEFAULT,
    }
}

fn insert_row(list: HWND, index: usize, cells: [&str; 3]) {
    for (sub, text) in cells.iter().enumerate() {
        let mut text = wide(text);
        let row = LVITEMW {
            mask: LVIF_TEXT,
            iItem: index as i32,
            iSubItem: sub as i32,
            pszText: text.as_mut_ptr(),
            ..Default::default()
        };
        let msg = if sub == 0 {
            LVM_INSERTITEMW
        } else {
            LVM_SETITEMTEXTW
        };
        // SAFETY: `row` and its text stay alive for the duration of the call.
        unsafe { SendMessageW(list, msg, index, &raw const row as isize) };
    }
}

/// Sends a message whose parameters are plain values, not pointers.
fn send(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> LRESULT {
    // SAFETY: no pointers are passed; SendMessageW tolerates any window handle.
    unsafe { SendMessageW(hwnd, msg, wparam, lparam) }
}

fn item(hwnd: HWND, id: i32) -> HWND {
    // SAFETY: GetDlgItem tolerates any window handle and ID.
    unsafe { GetDlgItem(hwnd, id) }
}

fn add_string(combo: HWND, text: &str) {
    let text = wide(text);
    // SAFETY: `text` is null-terminated and outlives the call.
    unsafe { SendMessageW(combo, CB_ADDSTRING, 0, text.as_ptr() as isize) };
}

fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    // SAFETY: `text` is null-terminated and outlives the call.
    unsafe { SetWindowTextW(hwnd, text.as_ptr()) };
}

fn read_number(edit: HWND) -> Option<u8> {
    let mut buf = [0u16; 8];
    // SAFETY: `buf` is writable for its full length.
    let len = unsafe { GetWindowTextW(edit, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..usize::try_from(len).ok()?])
        .parse()
        .ok()
}

fn set_check(button: HWND, on: bool) {
    send(button, BM_SETCHECK, usize::from(on), 0);
}

fn is_checked(button: HWND) -> bool {
    send(button, BM_GETCHECK, 0, 0) == BST_CHECKED as isize
}

/// Scales device-independent pixels to `dpi`.
fn px(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}
