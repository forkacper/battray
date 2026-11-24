//! User settings, stored as `key=value` lines in `%APPDATA%\battray\config.ini`,
//! and the "start with Windows" registry entry.

use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, MAX_PATH};
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

use crate::util::{from_wide, wide};

/// What the tray shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayMode {
    /// One icon with the lowest battery level among all devices.
    Lowest,
    /// One icon per device.
    PerDevice,
    /// One icon for the device with this ID.
    Single(String),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mode: TrayMode,
    pub interval_secs: u64,
    pub notify_low: bool,
    pub low_threshold: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: TrayMode::Lowest,
            interval_secs: 60,
            notify_low: true,
            low_threshold: 20,
        }
    }
}

pub const INTERVAL_CHOICES: [u64; 4] = [30, 60, 300, 900];

/// "Every minute", "Every 5 minutes", "Every 30 seconds".
pub fn interval_label(secs: u64) -> String {
    match secs {
        60 => "Every minute".to_owned(),
        s if s % 60 == 0 && s > 60 => format!("Every {} minutes", s / 60),
        s => format!("Every {s} seconds"),
    }
}

impl Config {
    /// Loads the config, falling back to defaults for a missing file or unknown values.
    pub fn load(path: &Path) -> Self {
        let mut cfg = Self::default();
        let Ok(text) = fs::read_to_string(path) else {
            return cfg;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "mode" => {
                    cfg.mode = match value {
                        "per_device" => TrayMode::PerDevice,
                        v => match v.strip_prefix("device:") {
                            Some(id) if !id.is_empty() => TrayMode::Single(id.to_owned()),
                            _ => TrayMode::Lowest,
                        },
                    };
                }
                "interval" => {
                    if let Ok(v) = value.parse::<u64>() {
                        cfg.interval_secs = v.clamp(10, 3600);
                    }
                }
                "notify_low" => cfg.notify_low = value != "0",
                "low_threshold" => {
                    if let Ok(v) = value.parse::<u8>() {
                        cfg.low_threshold = v.min(100);
                    }
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mode = match &self.mode {
            TrayMode::Lowest => "lowest".to_owned(),
            TrayMode::PerDevice => "per_device".to_owned(),
            TrayMode::Single(id) => format!("device:{id}"),
        };
        let text = format!(
            "mode={mode}\ninterval={}\nnotify_low={}\nlow_threshold={}\n",
            self.interval_secs,
            u8::from(self.notify_low),
            self.low_threshold
        );
        fs::write(path, text)
    }
}

/// `%APPDATA%\battray`, created if missing. Falls back to the executable's directory.
pub fn data_dir() -> PathBuf {
    let dir = std::env::var_os("APPDATA")
        .map(|appdata| PathBuf::from(appdata).join("battray"))
        .or_else(|| exe_path().and_then(|p| p.parent().map(Path::to_path_buf)))
        .unwrap_or_else(|| PathBuf::from("."));
    // If this fails, later file writes fail too and are handled there.
    let _ = fs::create_dir_all(&dir);
    dir
}

fn exe_path() -> Option<PathBuf> {
    let mut buf = vec![0u16; MAX_PATH as usize];
    loop {
        // SAFETY: `buf` is writable for the stated length; a null module means this executable.
        let len = unsafe { GetModuleFileNameW(ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) }
            as usize;
        if len == 0 {
            return None;
        }
        if len < buf.len() {
            return Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
        }
        buf.resize(buf.len() * 2, 0);
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "battray";
/// Command-line flag for starting in the tray without opening the window.
pub const TRAY_FLAG: &str = "--tray";

/// Whether the Run entry points at this executable.
pub fn autostart_enabled() -> bool {
    let (Some(exe), Some(current)) = (exe_path(), read_run_value()) else {
        return false;
    };
    run_target(&current).eq_ignore_ascii_case(&exe.to_string_lossy())
}

/// The program path of a Run command line, without quotes or arguments.
fn run_target(command: &str) -> &str {
    match command.strip_prefix('"') {
        Some(rest) => rest.split_once('"').map_or(rest, |(path, _)| path),
        None => command.split_once(' ').map_or(command, |(path, _)| path),
    }
}

/// Adds the tray flag to a Run entry written by an older version, so starting
/// with Windows does not open the window.
pub fn upgrade_autostart() {
    if autostart_enabled()
        && read_run_value().is_some_and(|v| !v.contains(TRAY_FLAG))
        && let Err(e) = set_autostart(true)
    {
        crate::log!("cannot update autostart: {e}");
    }
}

fn read_run_value() -> Option<String> {
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: strings are null-terminated; `buf` and `size` describe a writable buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    (status == ERROR_SUCCESS).then(|| from_wide(&buf))
}

pub fn set_autostart(enable: bool) -> std::io::Result<()> {
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    let status = if enable {
        let exe = exe_path().ok_or_else(std::io::Error::last_os_error)?;
        let value = wide(&format!("\"{}\" {TRAY_FLAG}", exe.display()));
        let bytes = (value.len() * 2) as u32;
        // SAFETY: strings are null-terminated and `bytes` matches the size of `value`.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                value.as_ptr().cast(),
                bytes,
            )
        }
    } else {
        // SAFETY: strings are null-terminated.
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) }
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(status as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_file() {
        let path = std::env::temp_dir().join("battray-config-test.ini");
        let cfg = Config {
            mode: TrayMode::Single("logitech:USB\\VID_046D:1".to_owned()),
            interval_secs: 300,
            notify_low: false,
            low_threshold: 15,
        };
        cfg.save(&path).expect("temp dir is writable");
        let loaded = Config::load(&path);
        let _ = fs::remove_file(&path);
        assert_eq!(loaded.mode, cfg.mode);
        assert_eq!(loaded.interval_secs, 300);
        assert!(!loaded.notify_low);
        assert_eq!(loaded.low_threshold, 15);
    }

    #[test]
    fn run_target_strips_quotes_and_arguments() {
        assert_eq!(
            run_target(r#""C:\a b\battray.exe" --tray"#),
            r"C:\a b\battray.exe"
        );
        assert_eq!(run_target(r#""C:\battray.exe""#), r"C:\battray.exe");
        assert_eq!(run_target(r"C:\battray.exe --tray"), r"C:\battray.exe");
    }

    #[test]
    fn interval_labels() {
        assert_eq!(interval_label(30), "Every 30 seconds");
        assert_eq!(interval_label(60), "Every minute");
        assert_eq!(interval_label(900), "Every 15 minutes");
    }

    #[test]
    fn unknown_values_fall_back_to_defaults() {
        let path = std::env::temp_dir().join("battray-config-bad.ini");
        fs::write(&path, "mode=bogus\ninterval=abc\n").expect("temp dir is writable");
        let loaded = Config::load(&path);
        let _ = fs::remove_file(&path);
        assert_eq!(loaded.mode, TrayMode::Lowest);
        assert_eq!(loaded.interval_secs, 60);
    }
}
