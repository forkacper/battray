//! battray: a lightweight Windows tray app that shows the battery level of
//! wireless peripherals (mice, keyboards, gamepads).

#![windows_subsystem = "windows"]

#[cfg(not(windows))]
compile_error!("battray only supports Windows");

mod config;
mod devices;
mod devnode;
mod hid;
mod icon;
mod logger;
mod tray;
mod util;
mod window;
mod worker;

use std::io::Write as _;

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

fn main() {
    if std::env::args().skip(1).any(|a| a == "--version") {
        print_version();
        return;
    }
    let in_tray = std::env::args().skip(1).any(|a| a == config::TRAY_FLAG);
    let name = util::wide("Local\\battray-single-instance");
    // SAFETY: `name` is null-terminated. The mutex handle is intentionally kept open
    // for the lifetime of the process.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: reads the calling thread's last error code right after CreateMutexW.
    if !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if !in_tray {
            tray::activate_running_instance();
        }
        return;
    }
    // SAFETY: called once at start-up, before any window exists. Crisp icons at any scale.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    let data_dir = config::data_dir();
    logger::init(&data_dir.join("battray.log"));
    log!("battray {} starting", env!("CARGO_PKG_VERSION"));
    let cfg = config::Config::load(&data_dir.join("config.ini"));
    std::process::exit(tray::run(cfg, data_dir, !in_tray));
}

/// Prints the version to the console that started battray, if any, and returns, so
/// scripts and package managers can check the installed version.
fn print_version() {
    // SAFETY: attaching to the parent's console has no preconditions; it fails
    // harmlessly when there is none or output is already redirected.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    // Without a console the write fails, and there is nobody to tell.
    let _ = writeln!(std::io::stdout(), "battray {}", env!("CARGO_PKG_VERSION"));
}
