//! Background thread that polls all providers and hands results to the UI thread.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::devices::{self, Battery, Provider, Reading};
use crate::hid;
use crate::log;

/// Device arrival is noisy (one event per collection) and drivers need a moment
/// before the device answers, so polls after a change are delayed by this much.
const SETTLE_DELAY: Duration = Duration::from_secs(2);

pub enum Command {
    Refresh,
    DevicesChanged,
    SetInterval(Duration),
    WriteDiagnostics(PathBuf),
}

/// A device as presented to the UI.
#[derive(Debug, Clone)]
pub struct DeviceState {
    pub id: String,
    pub name: String,
    /// Last known battery state, kept while the device is asleep.
    pub battery: Option<Battery>,
    /// Whether `battery` was read during the latest poll.
    pub fresh: bool,
}

pub type Shared = Arc<Mutex<Vec<DeviceState>>>;

/// Where the worker reports to: a window and the messages it understands.
#[derive(Clone, Copy)]
pub struct Notify {
    /// The window handle as an integer, since raw pointers are not `Send`.
    pub hwnd: usize,
    pub updated_msg: u32,
    pub diagnostics_msg: u32,
}

impl Notify {
    fn post(self, msg: u32) {
        // SAFETY: PostMessageW is thread-safe and tolerates a window that no longer exists.
        unsafe { PostMessageW(self.hwnd as _, msg, 0, 0) };
    }
}

pub fn spawn(notify: Notify, shared: Shared, interval: Duration) -> Sender<Command> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("poller".to_owned())
        .spawn(move || run(&rx, notify, &shared, interval))
        .expect("spawning the poller thread only fails when the OS is out of resources");
    tx
}

fn run(rx: &Receiver<Command>, notify: Notify, shared: &Shared, mut interval: Duration) {
    let mut providers = devices::providers();
    let mut last_known: HashMap<String, Battery> = HashMap::new();
    let mut wait = Duration::ZERO;
    loop {
        match rx.recv_timeout(wait) {
            Ok(Command::Refresh) | Err(RecvTimeoutError::Timeout) => {}
            Ok(Command::DevicesChanged) => {
                settle(rx, &mut interval);
            }
            Ok(Command::SetInterval(d)) => {
                interval = d;
                wait = interval;
                continue;
            }
            Ok(Command::WriteDiagnostics(path)) => {
                let report = diagnostics(&mut providers);
                match std::fs::write(&path, report) {
                    Ok(()) => notify.post(notify.diagnostics_msg),
                    Err(e) => log!("cannot write diagnostics to {}: {e}", path.display()),
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let states = poll(&mut providers, &mut last_known);
        *shared.lock().unwrap_or_else(PoisonError::into_inner) = states;
        notify.post(notify.updated_msg);
        wait = interval;
    }
}

/// Waits out a burst of device change events, still honouring interval changes.
fn settle(rx: &Receiver<Command>, interval: &mut Duration) {
    let deadline = Instant::now() + SETTLE_DELAY;
    while let Some(left) = hid::remaining(deadline) {
        match rx.recv_timeout(left) {
            Ok(Command::SetInterval(d)) => *interval = d,
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn poll(
    providers: &mut [Box<dyn Provider>],
    last_known: &mut HashMap<String, Battery>,
) -> Vec<DeviceState> {
    let started = Instant::now();
    let hid = hid::enumerate();
    let mut readings: Vec<Reading> = Vec::new();
    for provider in providers.iter_mut() {
        provider.poll(&hid, &mut readings);
    }
    let states: Vec<DeviceState> = readings
        .into_iter()
        .map(|r| {
            let fresh = r.battery.is_some();
            if let Some(b) = r.battery {
                last_known.insert(r.id.clone(), b);
            }
            DeviceState {
                battery: r.battery.or_else(|| last_known.get(&r.id).copied()),
                fresh,
                id: r.id,
                name: r.name,
            }
        })
        .collect();
    log!(
        "poll: {} hid collections, {} devices in {} ms",
        hid.len(),
        states.len(),
        started.elapsed().as_millis()
    );
    states
}

fn diagnostics(providers: &mut [Box<dyn Provider>]) -> String {
    let hid = hid::enumerate();
    let mut out = String::new();
    let _ = writeln!(out, "battray {} diagnostics\n", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "== HID collections ({}) ==", hid.len());
    for d in &hid {
        let _ = writeln!(
            out,
            "\nvid={:04X} pid={:04X} ver={:04X} usage_page={:04X} usage={:04X} in={} out={} feature={}\n  product: {}\n  manufacturer: {}\n  parent: {}\n  path: {}",
            d.vendor_id,
            d.product_id,
            d.version,
            d.usage_page,
            d.usage,
            d.input_len,
            d.output_len,
            d.feature_len,
            d.product,
            d.manufacturer,
            d.parent,
            d.path
        );
    }
    let _ = writeln!(out, "\n== Provider readings ==");
    for provider in providers.iter_mut() {
        let mut readings = Vec::new();
        provider.poll(&hid, &mut readings);
        let _ = writeln!(out, "\n[{}] {} device(s)", provider.name(), readings.len());
        for r in readings {
            let _ = writeln!(
                out,
                "  {} ({:?}) id={} battery={:?}",
                r.name, r.kind, r.id, r.battery
            );
        }
    }
    out
}
