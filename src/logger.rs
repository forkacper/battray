//! Minimal file logger: one log file per run, written to the app data directory.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

static LOG: Mutex<Option<File>> = Mutex::new(None);

/// Starts a fresh log file at `path`. Logging is silently disabled if the file cannot be created.
pub fn init(path: &Path) {
    let file = File::create(path).ok();
    *LOG.lock().unwrap_or_else(PoisonError::into_inner) = file;
}

/// Appends one timestamped line to the log file.
pub fn write(args: std::fmt::Arguments<'_>) {
    let mut guard = LOG.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(file) = guard.as_mut() else {
        return;
    };
    let mut t = SYSTEMTIME::default();
    // SAFETY: `t` is a valid, writable SYSTEMTIME.
    unsafe { GetLocalTime(&mut t) };
    // A failed log write is not worth surfacing anywhere.
    let _ = writeln!(
        file,
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {args}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    );
}

/// Logs a formatted line, like `println!`.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::logger::write(format_args!($($arg)*))
    };
}
