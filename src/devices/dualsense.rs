//! Sony DualSense and DualSense Edge, over USB or Bluetooth.
//!
//! The battery lives in the status byte of the regular input report (see the
//! Linux `hid-playstation` driver). Over Bluetooth the pad starts in a reduced
//! report mode without that byte; reading feature report 0x05 (calibration)
//! switches it to the full 0x31 report. Steam does the same, so with Steam
//! running the pad is usually in full mode already.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::{Battery, ChargeState, DeviceKind, Provider, Reading};
use crate::hid::{self, Device, DeviceInfo};
use crate::log;

const VENDOR_SONY: u16 = 0x054C;
const MODELS: &[(u16, &str)] = &[(0x0CE6, "DualSense"), (0x0DF2, "DualSense Edge")];

const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
const USAGE_GAMEPAD: u16 = 0x05;

/// Input report lengths (with report ID) as reported by Windows.
const BT_INPUT_LEN: u16 = 78;

const REPORT_USB: u8 = 0x01;
const REPORT_BT_FULL: u8 = 0x31;
const REPORT_BT_SIMPLE: u8 = 0x01;
const FEATURE_CALIBRATION: u8 = 0x05;

/// Offset of the status byte within the common input report body.
const STATUS_OFFSET: usize = 52;
/// The body starts after the report ID (USB) or after report ID + sequence tag (BT).
const USB_BODY: usize = 1;
const BT_BODY: usize = 2;

const READ_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Default)]
pub struct DualSense {
    /// Pads we already asked to switch to full Bluetooth reports.
    switched: HashSet<String>,
}

impl Provider for DualSense {
    fn name(&self) -> &'static str {
        "dualsense"
    }

    fn poll(&mut self, hid: &[DeviceInfo], out: &mut Vec<Reading>) {
        for info in hid {
            if info.vendor_id != VENDOR_SONY
                || info.usage_page != USAGE_PAGE_GENERIC_DESKTOP
                || info.usage != USAGE_GAMEPAD
            {
                continue;
            }
            let Some(&(_, model)) = MODELS.iter().find(|(pid, _)| *pid == info.product_id) else {
                continue;
            };
            let battery = match self.read(info) {
                Ok(b) => b,
                Err(e) => {
                    log!("dualsense: reading {} failed: {e}", info.path);
                    None
                }
            };
            let key = if info.parent.is_empty() {
                &info.path
            } else {
                &info.parent
            };
            out.push(Reading {
                id: format!("dualsense:{key}"),
                name: model.to_owned(),
                kind: DeviceKind::Gamepad,
                battery,
            });
        }
    }
}

impl DualSense {
    fn read(&mut self, info: &DeviceInfo) -> std::io::Result<Option<Battery>> {
        let dev = Device::open(info)?;
        let bluetooth = info.input_len >= BT_INPUT_LEN;
        let deadline = Instant::now() + READ_TIMEOUT;
        while let Some(left) = hid::remaining(deadline) {
            let Some(report) = dev.read(left)? else {
                break;
            };
            let status = match (report.first(), bluetooth) {
                (Some(&REPORT_BT_FULL), true) => report.get(BT_BODY + STATUS_OFFSET),
                (Some(&REPORT_USB), false) => report.get(USB_BODY + STATUS_OFFSET),
                (Some(&REPORT_BT_SIMPLE), true) => {
                    if self.switched.insert(info.path.clone()) {
                        log!(
                            "dualsense: switching {} to full Bluetooth reports",
                            info.path
                        );
                        dev.get_feature(FEATURE_CALIBRATION)?;
                    }
                    None
                }
                _ => None,
            };
            if let Some(&status) = status {
                return Ok(parse_status(status));
            }
        }
        Ok(None)
    }
}

/// Decodes the status byte: low nibble is the level in steps of 10 %, high nibble the charge state.
fn parse_status(status: u8) -> Option<Battery> {
    let level = status & 0x0F;
    let step = (level * 10 + 5).min(100);
    match status >> 4 {
        0x0 => Some(Battery {
            percent: step,
            charging: ChargeState::Discharging,
        }),
        0x1 => Some(Battery {
            percent: step,
            charging: ChargeState::Charging,
        }),
        0x2 => Some(Battery {
            percent: 100,
            charging: ChargeState::Full,
        }),
        // 0xA/0xB: voltage or temperature out of range, 0xF: charging error.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_discharging_level() {
        assert_eq!(
            parse_status(0x07),
            Some(Battery {
                percent: 75,
                charging: ChargeState::Discharging
            })
        );
    }

    #[test]
    fn caps_level_at_100() {
        assert_eq!(parse_status(0x1A).map(|b| b.percent), Some(100));
    }

    #[test]
    fn full_reports_100() {
        assert_eq!(
            parse_status(0x20),
            Some(Battery {
                percent: 100,
                charging: ChargeState::Full
            })
        );
    }

    #[test]
    fn error_states_are_unknown() {
        assert_eq!(parse_status(0xF3), None);
    }
}
