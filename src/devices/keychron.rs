//! Keychron keyboards running the battray firmware patch (see `firmware/keychron-k3-max`).
//!
//! Stock Keychron firmware does not report its battery to the host. The patch answers
//! a query on VIA's custom value channel, over USB or through the Keychron Link dongle.
//! Devices that never answered (stock firmware, keyboard asleep at start-up) are not shown.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::{Battery, ChargeState, DeviceKind, Provider, Reading};
use crate::hid::{self, Device, DeviceInfo};
use crate::log;

const VENDOR_KEYCHRON: u16 = 0x3434;
const USAGE_PAGE_RAW_HID: u16 = 0xFF60;
const USAGE_RAW_HID: u16 = 0x61;

/// VIA `id_custom_get_value`, `id_custom_channel` and battray's value ID.
const QUERY: [u8; 3] = [0x08, 0x00, 0x42];
const VIA_UNHANDLED: u8 = 0xFF;
const REPLY_VERSION: u8 = 1;
/// The dongle adds a radio round trip, and a keyboard in low power mode may only
/// wake up on the first packet, so a silent attempt is retried once.
const TIMEOUT: Duration = Duration::from_millis(1500);
const ATTEMPTS: usize = 2;

#[derive(Default)]
pub struct Keychron {
    /// Raw HID interfaces that answered with the battray reply at least once,
    /// with the keyboard name they reported.
    known: HashMap<String, String>,
}

#[derive(Debug, PartialEq)]
enum Answer {
    Battery {
        name: String,
        battery: Battery,
    },
    /// The firmware does not know the query: stock firmware.
    Unsupported,
}

impl Provider for Keychron {
    fn name(&self) -> &'static str {
        "keychron"
    }

    fn poll(&mut self, hid: &[DeviceInfo], out: &mut Vec<Reading>) {
        let mut found = Vec::new();
        for info in hid {
            if info.vendor_id != VENDOR_KEYCHRON
                || info.usage_page != USAGE_PAGE_RAW_HID
                || info.usage != USAGE_RAW_HID
            {
                continue;
            }
            let key = if info.parent.is_empty() {
                &info.path
            } else {
                &info.parent
            };
            let id = format!("keychron:{key}");
            let started = Instant::now();
            match query(info) {
                Ok(Some(Answer::Battery { name, battery })) => {
                    if self.known.insert(id.clone(), name.clone()).is_none() {
                        log!(
                            "keychron: \"{name}\" answers on {} after {} ms",
                            info.path,
                            started.elapsed().as_millis()
                        );
                    }
                    found.push(Reading {
                        id,
                        name,
                        kind: DeviceKind::Keyboard,
                        battery: Some(battery),
                    });
                }
                Ok(Some(Answer::Unsupported)) => {}
                // No reply: the keyboard is asleep, out of range, or runs stock firmware.
                Ok(None) | Err(_) if self.known.contains_key(&id) => {
                    log!(
                        "keychron: no reply on {} within {} ms",
                        info.path,
                        started.elapsed().as_millis()
                    );
                    let name = self.known.get(&id).cloned().unwrap_or_default();
                    found.push(Reading {
                        id,
                        name,
                        kind: DeviceKind::Keyboard,
                        battery: None,
                    });
                }
                Ok(None) => {}
                Err(e) => log!("keychron: query on {} failed: {e}", info.path),
            }
        }
        // A keyboard on a cable next to its dongle shows up twice; drop the silent entry.
        let answered: HashSet<String> = found
            .iter()
            .filter(|r| r.battery.is_some())
            .map(|r| r.name.clone())
            .collect();
        found.retain(|r| r.battery.is_some() || !answered.contains(&r.name));
        out.append(&mut found);
    }
}

fn query(info: &DeviceInfo) -> std::io::Result<Option<Answer>> {
    let dev = Device::open(info)?;
    for _ in 0..ATTEMPTS {
        if let Some(answer) = query_once(&dev)? {
            return Ok(Some(answer));
        }
    }
    Ok(None)
}

fn query_once(dev: &Device) -> std::io::Result<Option<Answer>> {
    dev.flush();
    // Report ID 0 followed by the 32-byte VIA packet.
    let mut request = [0u8; 33];
    request[1..1 + QUERY.len()].copy_from_slice(&QUERY);
    dev.write(&request)?;

    let deadline = Instant::now() + TIMEOUT;
    while let Some(left) = hid::remaining(deadline) {
        let Some(report) = dev.read(left)? else {
            break;
        };
        if let Some(answer) = parse_reply(report.get(1..).unwrap_or_default()) {
            return Ok(Some(answer));
        }
    }
    Ok(None)
}

/// Parses a 32-byte VIA packet (without the report ID). `None` if it is not our reply.
fn parse_reply(p: &[u8]) -> Option<Answer> {
    let (&[cmd, channel, value], rest) = p.split_first_chunk::<3>()?;
    if [channel, value] != QUERY[1..] {
        return None;
    }
    match cmd {
        VIA_UNHANDLED => Some(Answer::Unsupported),
        c if c == QUERY[0] => {
            let &[
                version,
                percent,
                _mv_lo,
                _mv_hi,
                charge,
                _transport,
                ref name @ ..,
            ] = rest
            else {
                return None;
            };
            if version != REPLY_VERSION {
                return Some(Answer::Unsupported);
            }
            let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            let name = String::from_utf8_lossy(&name[..end]).trim().to_owned();
            let charging = match charge {
                0 => ChargeState::Discharging,
                1 => ChargeState::Charging,
                2 => ChargeState::Full,
                _ => ChargeState::Unknown,
            };
            Some(Answer::Battery {
                name: if name.is_empty() {
                    "Keychron keyboard".to_owned()
                } else {
                    name
                },
                battery: Battery {
                    percent: percent.min(100),
                    charging,
                },
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(bytes: &[u8]) -> [u8; 32] {
        let mut p = [0u8; 32];
        p[..bytes.len()].copy_from_slice(bytes);
        p
    }

    #[test]
    fn parses_battery_reply() {
        let mut bytes = vec![0x08, 0x00, 0x42, 1, 73, 0x10, 0x0F, 0, 4];
        bytes.extend_from_slice(b"Keychron K3 Max");
        assert_eq!(
            parse_reply(&packet(&bytes)),
            Some(Answer::Battery {
                name: "Keychron K3 Max".to_owned(),
                battery: Battery {
                    percent: 73,
                    charging: ChargeState::Discharging
                },
            })
        );
    }

    #[test]
    fn stock_firmware_is_unsupported() {
        assert_eq!(
            parse_reply(&packet(&[0xFF, 0x00, 0x42])),
            Some(Answer::Unsupported)
        );
    }

    #[test]
    fn ignores_unrelated_reports() {
        // Layer change notification pushed by the keyboard.
        assert_eq!(parse_reply(&packet(&[0xA3, 0x02, 0x02])), None);
    }
}
