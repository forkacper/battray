//! Razer mice, keyboards and other devices speaking Razer's control protocol, over their
//! HyperSpeed dongle or a USB cable. Over Bluetooth, Windows reports them itself.
//!
//! Protocol notes come from the openrazer project. A request and its reply are the same
//! 90-byte feature report (report ID 0): `[status, transaction ID, remaining packets (2),
//! protocol type, data size, command class, command ID, arguments (80), crc, reserved]`.
//! The device needs a moment to fill in the reply, longer when it is wireless.

use std::collections::HashMap;
use std::io;
use std::thread;
use std::time::{Duration, Instant};

use super::{Battery, ChargeState, DeviceKind, Provider, Reading};
use crate::hid::{self, Device, DeviceInfo};
use crate::log;

const VENDOR_RAZER: u16 = 0x1532;
/// Report ID plus the 90-byte report.
const REPORT_LEN: usize = 91;

const STATUS_NEW: u8 = 0x00;
const STATUS_BUSY: u8 = 0x01;
const STATUS_SUCCESS: u8 = 0x02;
/// The dongle got no answer from the device, which is asleep or switched off.
const STATUS_TIMEOUT: u8 = 0x04;

const CLASS_POWER: u8 = 0x07;
const CMD_BATTERY_LEVEL: u8 = 0x80;
const CMD_CHARGING: u8 = 0x84;

/// Transaction IDs the device families expect, tried in turn and remembered per device.
const TRANSACTION_IDS: [u8; 4] = [0x1F, 0x3F, 0x9F, 0xFF];

const FIRST_WAIT: Duration = Duration::from_millis(5);
const POLL_WAIT: Duration = Duration::from_millis(20);
const TIMEOUT: Duration = Duration::from_millis(500);

/// A device found to report its battery.
#[derive(Debug, Clone)]
struct Known {
    path: String,
    transaction_id: u8,
}

#[derive(Default)]
pub struct Razer {
    /// Devices probed so far, keyed by product ID. `None` marks one that has no battery,
    /// so it is not probed again until it is plugged in anew.
    products: HashMap<u16, Option<Known>>,
}

impl Provider for Razer {
    fn name(&self) -> &'static str {
        "razer"
    }

    fn poll(&mut self, hid: &[DeviceInfo], out: &mut Vec<Reading>) {
        let candidates: Vec<&DeviceInfo> = hid
            .iter()
            .filter(|i| {
                i.vendor_id == VENDOR_RAZER
                    && usize::from(i.feature_len) == REPORT_LEN
                    && i.parent.to_ascii_uppercase().starts_with("USB\\")
            })
            .collect();
        self.products
            .retain(|pid, _| candidates.iter().any(|i| i.product_id == *pid));

        let mut pids: Vec<u16> = candidates.iter().map(|i| i.product_id).collect();
        pids.sort_unstable();
        pids.dedup();
        for pid in pids {
            let collections = candidates.iter().copied().filter(|i| i.product_id == pid);
            let known = match self.products.get(&pid) {
                Some(Some(known)) => known.clone(),
                Some(None) => continue,
                None => {
                    let found = discover(collections.clone());
                    match &found {
                        Some(k) => log!("razer: found {pid:04X} on {}", k.path),
                        None => log!("razer: no battery on {pid:04X}"),
                    }
                    self.products.insert(pid, found.clone());
                    let Some(known) = found else { continue };
                    known
                }
            };
            let Some(info) = collections.clone().find(|i| i.path == known.path) else {
                self.products.remove(&pid);
                continue;
            };
            let battery = match read_battery(info, known.transaction_id) {
                Ok(b) => b,
                Err(e) => {
                    log!("razer: battery read on {} failed: {e}", info.path);
                    None
                }
            };
            let name = display_name(&info.product);
            let id = if info.parent.is_empty() {
                &info.path
            } else {
                &info.parent
            };
            out.push(Reading {
                id: format!("razer:{id}"),
                kind: kind_from_name(&name),
                name,
                battery,
            });
        }
    }
}

/// Finds the collection and transaction ID that answer the battery command.
fn discover<'a>(collections: impl Iterator<Item = &'a DeviceInfo>) -> Option<Known> {
    for info in collections {
        let Ok(device) = Device::open(info) else {
            continue;
        };
        for transaction_id in TRANSACTION_IDS {
            let reply = request(&device, transaction_id, CLASS_POWER, CMD_BATTERY_LEVEL, 2);
            if let Ok(Some(r)) = reply
                && matches!(r.status, STATUS_SUCCESS | STATUS_TIMEOUT)
            {
                return Some(Known {
                    path: info.path.clone(),
                    transaction_id,
                });
            }
        }
    }
    None
}

/// `Ok(None)` means the dongle is there but the device did not answer it.
fn read_battery(info: &DeviceInfo, transaction_id: u8) -> io::Result<Option<Battery>> {
    let device = Device::open(info)?;
    let Some(level) = request(&device, transaction_id, CLASS_POWER, CMD_BATTERY_LEVEL, 2)? else {
        return Err(io::Error::from(io::ErrorKind::TimedOut));
    };
    if level.status != STATUS_SUCCESS {
        return Ok(None);
    }
    let charging = match request(&device, transaction_id, CLASS_POWER, CMD_CHARGING, 2) {
        Ok(Some(r)) if r.status == STATUS_SUCCESS => {
            if r.args[1] != 0 {
                ChargeState::Charging
            } else {
                ChargeState::Discharging
            }
        }
        _ => ChargeState::Unknown,
    };
    Ok(Some(Battery {
        percent: percent_from_level(level.args[1]),
        charging,
    }))
}

struct Reply {
    status: u8,
    args: [u8; 80],
}

/// Sends a command and waits for the device to fill in its reply. `Ok(None)` means it did
/// not within the timeout.
fn request(
    device: &Device,
    transaction_id: u8,
    class: u8,
    command: u8,
    data_size: u8,
) -> io::Result<Option<Reply>> {
    let mut report = [0u8; REPORT_LEN];
    report[2] = transaction_id;
    report[6] = data_size;
    report[7] = class;
    report[8] = command;
    report[89] = crc(&report);
    device.set_feature(&report)?;

    let deadline = Instant::now() + TIMEOUT;
    thread::sleep(FIRST_WAIT);
    loop {
        let reply = device.get_feature(0)?;
        if let Some(reply) = parse_reply(&reply, class, command) {
            return Ok(Some(reply));
        }
        match hid::remaining(deadline) {
            Some(left) => thread::sleep(left.min(POLL_WAIT)),
            None => return Ok(None),
        }
    }
}

/// Returns the reply to `class`/`command` once the device has finished with it.
fn parse_reply(report: &[u8], class: u8, command: u8) -> Option<Reply> {
    if report.len() < REPORT_LEN || report[7] != class || report[8] != command {
        return None;
    }
    let status = report[1];
    if matches!(status, STATUS_NEW | STATUS_BUSY) {
        return None;
    }
    let mut args = [0u8; 80];
    args.copy_from_slice(&report[9..89]);
    Some(Reply { status, args })
}

/// XOR of the report bytes from the remaining packets field to the end of the arguments.
fn crc(report: &[u8; REPORT_LEN]) -> u8 {
    report[3..89].iter().fold(0, |acc, b| acc ^ b)
}

/// The battery level comes as 0–255.
fn percent_from_level(level: u8) -> u8 {
    u8::try_from((u16::from(level) * 100 + 127) / 255).unwrap_or(100)
}

/// The product name without the "Dongle" or "Receiver" of a wireless adapter.
fn display_name(product: &str) -> String {
    let name = product.trim();
    let name = ["Dongle", "Receiver"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(name)
        .trim();
    if name.is_empty() {
        "Razer device".to_owned()
    } else {
        name.to_owned()
    }
}

/// Razer does not report the device type, but its product lines are distinct.
fn kind_from_name(name: &str) -> DeviceKind {
    const MICE: [&str; 11] = [
        "mouse",
        "deathadder",
        "viper",
        "basilisk",
        "naga",
        "orochi",
        "mamba",
        "lancehead",
        "atheris",
        "pro click",
        "cobra",
    ];
    const KEYBOARDS: [&str; 7] = [
        "keyboard",
        "blackwidow",
        "huntsman",
        "deathstalker",
        "ornata",
        "cynosa",
        "pro type",
    ];
    const HEADSETS: [&str; 6] = [
        "headset",
        "barracuda",
        "blackshark",
        "kraken",
        "nari",
        "hammerhead",
    ];
    let name = name.to_ascii_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if any(&MICE) {
        DeviceKind::Mouse
    } else if any(&KEYBOARDS) {
        DeviceKind::Keyboard
    } else if any(&HEADSETS) {
        DeviceKind::Headset
    } else {
        DeviceKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u8, class: u8, command: u8, args: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; REPORT_LEN];
        r[1] = status;
        r[7] = class;
        r[8] = command;
        r[9..9 + args.len()].copy_from_slice(args);
        r
    }

    #[test]
    fn crc_xors_the_command_bytes() {
        let mut r = [0u8; REPORT_LEN];
        r[2] = 0x1F;
        r[6] = 0x02;
        r[7] = 0x07;
        r[8] = 0x80;
        // The transaction ID is not covered.
        assert_eq!(crc(&r), 0x02 ^ 0x07 ^ 0x80);
    }

    #[test]
    fn waits_while_busy() {
        let r = reply(STATUS_BUSY, CLASS_POWER, CMD_BATTERY_LEVEL, &[0, 200]);
        assert!(parse_reply(&r, CLASS_POWER, CMD_BATTERY_LEVEL).is_none());
    }

    #[test]
    fn ignores_reply_to_other_command() {
        let r = reply(STATUS_SUCCESS, CLASS_POWER, CMD_CHARGING, &[0, 1]);
        assert!(parse_reply(&r, CLASS_POWER, CMD_BATTERY_LEVEL).is_none());
    }

    #[test]
    fn reads_level() {
        let r = reply(STATUS_SUCCESS, CLASS_POWER, CMD_BATTERY_LEVEL, &[0, 255]);
        let parsed = parse_reply(&r, CLASS_POWER, CMD_BATTERY_LEVEL).map(|r| r.args[1]);
        assert_eq!(parsed, Some(255));
        assert_eq!(percent_from_level(255), 100);
        assert_eq!(percent_from_level(128), 50);
        assert_eq!(percent_from_level(0), 0);
    }

    #[test]
    fn names_and_kinds() {
        assert_eq!(
            display_name("Razer Viper Ultimate Dongle"),
            "Razer Viper Ultimate"
        );
        assert_eq!(
            kind_from_name("Razer BlackWidow V3 Pro"),
            DeviceKind::Keyboard
        );
        assert_eq!(kind_from_name("Razer Viper Ultimate"), DeviceKind::Mouse);
        assert_eq!(kind_from_name("Razer Barracuda X"), DeviceKind::Headset);
    }
}
