//! Logitech headsets on the newer centurion transport: PRO X 2 LIGHTSPEED, PRO X 3
//! LIGHTSPEED, G522 and similar. The dongle has a small HID++ style feature table of its
//! own and relays requests to the headset through its bridge feature.
//!
//! Protocol notes come from the HeadsetControl and Solaar projects. A frame is
//! `[report prefix…, length, flags, payload…]`, 64 bytes after the prefix is counted in,
//! where `length` covers the flags byte and the payload. A payload is
//! `[feature index, function << 4 | swid, params…]` as in HID++ 2.0.

use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use super::VENDOR_LOGITECH;
use crate::devices::{Battery, ChargeState, DeviceKind, Reading};
use crate::hid::{self, Device, DeviceInfo};
use crate::log;

const USAGE_PAGE: u16 = 0xFFA0;
const USAGE: u16 = 0x0001;
const FRAME_LEN: usize = 64;

/// Report prefixes in use: plain (PRO X 2), and addressed to the headset (G522, PRO X 3).
const PREFIXES: [&[u8]; 2] = [&[0x51], &[0x50, 0x23]];

const SWID: u8 = 0x0D;
/// Requests to the headset go over the air, so allow more than for the dongle itself.
const TIMEOUT: Duration = Duration::from_millis(1000);

const FEATURE_BRIDGE: u16 = 0x0003;
const FEATURE_BATTERY_SOC: u16 = 0x0104;
/// Bridge function that carries a message to the headset, and the event carrying its reply.
const BRIDGE_MESSAGE: u8 = 0x1;
const ERROR: u8 = 0xFF;

/// A dongle whose headset has a battery feature.
#[derive(Debug, Clone, Copy)]
struct Link {
    prefix: &'static [u8],
    bridge: u8,
    battery: u8,
}

#[derive(Default)]
pub struct Centurion {
    /// Collections probed so far, keyed by path. `None` marks one that is not a centurion
    /// dongle or whose headset has no battery feature, so it is not probed again.
    links: HashMap<String, Option<Link>>,
}

impl Centurion {
    pub fn poll(&mut self, hid: &[DeviceInfo], out: &mut Vec<Reading>) {
        let dongles = hid.iter().filter(|i| {
            i.vendor_id == VENDOR_LOGITECH && i.usage_page == USAGE_PAGE && i.usage == USAGE
        });
        for info in dongles {
            if let Some(None) = self.links.get(&info.path) {
                continue;
            }
            let device = match Device::open(info) {
                Ok(d) => d,
                Err(e) => {
                    log!("logitech: cannot open {}: {e}", info.path);
                    continue;
                }
            };
            let link = match self.links.get(&info.path).copied().flatten() {
                Some(link) => link,
                None => match discover(&device) {
                    Ok(found) => {
                        match found {
                            Some(_) => log!("logitech: found centurion headset on {}", info.path),
                            None => log!("logitech: no centurion battery on {}", info.path),
                        }
                        self.links.insert(info.path.clone(), found);
                        let Some(link) = found else { continue };
                        link
                    }
                    // Typically the headset is switched off; try again on the next poll.
                    Err(e) => {
                        log!(
                            "logitech: centurion headset on {} did not answer: {e}",
                            info.path
                        );
                        continue;
                    }
                },
            };
            let channel = Channel {
                device: &device,
                prefix: link.prefix,
            };
            let battery = match channel.bridge_request(link.bridge, link.battery, 0x0, &[]) {
                Ok(p) => Some(parse_battery(&p)),
                Err(Error::Offline) => None,
                Err(e) => {
                    log!("logitech: centurion battery read failed: {e}");
                    None
                }
            };
            let id = if info.parent.is_empty() {
                &info.path
            } else {
                &info.parent
            };
            let name = match info.product.trim() {
                "" => "Logitech headset",
                product => product,
            };
            out.push(Reading {
                id: format!("logitech:{id}"),
                name: name.to_owned(),
                kind: DeviceKind::Headset,
                battery,
            });
        }
    }
}

#[derive(Debug)]
enum Error {
    Io(io::Error),
    Timeout,
    /// The dongle reports no headset connected.
    Offline,
    /// HID++ style error code from the dongle or the headset.
    Rejected(u8),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Timeout => f.write_str("no reply"),
            Self::Offline => f.write_str("headset not connected"),
            Self::Rejected(code) => write!(f, "error 0x{code:02X}"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Finds the report prefix the dongle accepts, its bridge and the headset's battery feature.
/// `Ok(None)` means this is not a centurion dongle or the headset has no battery feature;
/// an error means the dongle answered but the headset did not.
fn discover(device: &Device) -> Result<Option<Link>, Error> {
    for prefix in PREFIXES {
        let channel = Channel { device, prefix };
        // A prefix the dongle does not declare fails to write or goes unanswered.
        let Ok(bridge) = channel.feature_index(FEATURE_BRIDGE) else {
            continue;
        };
        let Some(bridge) = bridge else {
            return Ok(None);
        };
        let reply =
            channel.bridge_request(bridge, 0x00, 0x0, &FEATURE_BATTERY_SOC.to_be_bytes())?;
        return Ok(match reply.first() {
            Some(&battery) if battery != 0 => Some(Link {
                prefix,
                bridge,
                battery,
            }),
            _ => None,
        });
    }
    Ok(None)
}

struct Channel<'a> {
    device: &'a Device,
    prefix: &'static [u8],
}

impl Channel<'_> {
    /// Looks up a feature of the dongle itself through its root feature (index 0).
    fn feature_index(&self, feature: u16) -> Result<Option<u8>, Error> {
        let reply = self.request(0x00, 0x0, &feature.to_be_bytes())?;
        Ok(reply.first().copied().filter(|&i| i != 0))
    }

    /// Sends a request to the dongle and returns the params of its reply.
    fn request(&self, feature: u8, function: u8, params: &[u8]) -> Result<Vec<u8>, Error> {
        let fn_swid = (function << 4) | SWID;
        let mut payload = vec![feature, fn_swid];
        payload.extend_from_slice(params);
        self.exchange(&payload, |p| match *p {
            [ERROR, f, fs, code, ..] if f == feature && fs == fn_swid => {
                Some(Err(Error::Rejected(code)))
            }
            [f, fs, ref rest @ ..] if f == feature && fs == fn_swid => Some(Ok(rest.to_vec())),
            _ => None,
        })
    }

    /// Sends a request to the headset through the bridge and returns the params of its reply.
    fn bridge_request(
        &self,
        bridge: u8,
        feature: u8,
        function: u8,
        params: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let send = (BRIDGE_MESSAGE << 4) | SWID;
        let event = BRIDGE_MESSAGE << 4;
        // The relayed message: [0x00, feature index, function << 4 | swid, params…],
        // preceded by its 12-bit length.
        let len = 3 + params.len();
        let mut payload = vec![bridge, send, (len >> 8) as u8 & 0x0F, len as u8];
        payload.extend_from_slice(&[0x00, feature, (function << 4) | SWID]);
        payload.extend_from_slice(params);
        self.exchange(&payload, |p| match *p {
            // The dongle refused to relay, as it does with no headset connected.
            [ERROR, b, fs, ..] if b == bridge && fs == send => Some(Err(Error::Offline)),
            // Connection state change; a zero state means the headset went away.
            [b, 0x00, _, state, ..] if b == bridge => (state == 0).then_some(Err(Error::Offline)),
            // The headset's reply, [len, len, 0x00, feature index, function | swid, params…].
            [b, e, _, _, 0x00, f, _, ref rest @ ..]
                if b == bridge && e == event && f == feature =>
            {
                Some(Ok(rest.to_vec()))
            }
            [b, e, _, _, 0x00, ERROR, f, _, ref rest @ ..]
                if b == bridge && e == event && f == feature =>
            {
                Some(Err(Error::Rejected(rest.first().copied().unwrap_or(0))))
            }
            // Anything else, including the dongle's acknowledgement of our request.
            _ => None,
        })
    }

    /// Writes one single-fragment frame and waits for a reply payload that `matches` accepts.
    fn exchange(
        &self,
        payload: &[u8],
        mut matches: impl FnMut(&[u8]) -> Option<Result<Vec<u8>, Error>>,
    ) -> Result<Vec<u8>, Error> {
        self.device.flush();
        self.device.write(&frame(self.prefix, payload))?;
        let deadline = Instant::now() + TIMEOUT;
        while let Some(left) = hid::remaining(deadline) {
            let Some(report) = self.device.read(left)? else {
                break;
            };
            if let Some(result) = unframe(self.prefix, &report).and_then(&mut matches) {
                return result;
            }
        }
        Err(Error::Timeout)
    }
}

fn frame(prefix: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut frame = prefix.to_vec();
    // Length of flags plus payload; flags 0 marks a single, final fragment.
    frame.push(u8::try_from(payload.len() + 1).unwrap_or(u8::MAX));
    frame.push(0x00);
    frame.extend_from_slice(payload);
    frame.resize(FRAME_LEN, 0);
    frame
}

/// Returns the payload of a frame sent with `prefix`.
fn unframe<'a>(prefix: &[u8], report: &'a [u8]) -> Option<&'a [u8]> {
    let rest = report.strip_prefix(prefix)?;
    let (&len, rest) = rest.split_first()?;
    // `rest` starts with the flags byte, which `len` counts.
    rest.get(1..usize::from(len))
}

/// Battery state of charge feature 0x0104: [level %, _, charging status].
fn parse_battery(p: &[u8]) -> Battery {
    let charging = match p.get(2) {
        Some(1 | 2) => ChargeState::Charging,
        _ => ChargeState::Discharging,
    };
    Battery {
        percent: p.first().copied().unwrap_or(0).min(100),
        charging,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let f = frame(&[0x50, 0x23], &[0x00, 0x0D, 0x00, 0x03]);
        assert_eq!(f.len(), FRAME_LEN);
        assert_eq!(&f[..8], &[0x50, 0x23, 5, 0x00, 0x00, 0x0D, 0x00, 0x03]);
        assert_eq!(
            unframe(&[0x50, 0x23], &f),
            Some(&[0x00, 0x0D, 0x00, 0x03][..])
        );
        assert_eq!(unframe(&[0x51], &f), None);
    }

    #[test]
    fn rejects_truncated_frame() {
        assert_eq!(unframe(&[0x51], &[0x51, 40, 0x00, 0x01]), None);
    }

    #[test]
    fn parses_battery() {
        assert_eq!(
            parse_battery(&[76, 0, 1]),
            Battery {
                percent: 76,
                charging: ChargeState::Charging
            }
        );
        assert_eq!(
            parse_battery(&[40, 0, 0]).charging,
            ChargeState::Discharging
        );
    }
}
