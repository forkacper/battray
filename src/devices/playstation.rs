//! Sony PlayStation controllers (DualShock 4, DualSense, DualSense Edge), over USB or Bluetooth.
//!
//! The battery lives in a status byte of the regular input report (see the Linux
//! `hid-playstation` driver). Over Bluetooth the controllers start in a reduced
//! report mode without that byte; reading the calibration feature report switches
//! them to full reports. Steam does the same, so with Steam running the controller
//! is usually in full mode already.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::{Battery, ChargeState, DeviceKind, Provider, Reading};
use crate::hid::{self, Device, DeviceInfo};
use crate::log;

const VENDOR_SONY: u16 = 0x054C;
const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
const USAGE_GAMEPAD: u16 = 0x05;
/// Largest USB input report (with report ID). Bluetooth collections report longer ones.
const USB_INPUT_LEN: u16 = 64;
const READ_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    DualShock4,
    DualSense,
}

const MODELS: &[(u16, &str, Family)] = &[
    (0x05C4, "DualShock 4", Family::DualShock4),
    (0x09CC, "DualShock 4", Family::DualShock4),
    (0x0BA0, "DualShock 4", Family::DualShock4), // Sony wireless adapter
    (0x0CE6, "DualSense", Family::DualSense),
    (0x0DF2, "DualSense Edge", Family::DualSense),
];

/// Where the status byte sits and how a family switches to full Bluetooth reports.
struct Layout {
    usb_report: u8,
    usb_status: usize,
    bt_full_report: u8,
    bt_status: usize,
    bt_simple_report: u8,
    bt_calibration_feature: u8,
}

impl Family {
    const fn layout(self) -> Layout {
        match self {
            // Common report body at data[1] (USB) / data[3] (BT 0x11), status at body + 29.
            Self::DualShock4 => Layout {
                usb_report: 0x01,
                usb_status: 30,
                bt_full_report: 0x11,
                bt_status: 32,
                bt_simple_report: 0x01,
                bt_calibration_feature: 0x05,
            },
            // Common report body at data[1] (USB) / data[2] (BT 0x31), status at body + 52.
            Self::DualSense => Layout {
                usb_report: 0x01,
                usb_status: 53,
                bt_full_report: 0x31,
                bt_status: 54,
                bt_simple_report: 0x01,
                bt_calibration_feature: 0x05,
            },
        }
    }

    fn parse_status(self, status: u8) -> Option<Battery> {
        match self {
            Self::DualShock4 => parse_dualshock4(status),
            Self::DualSense => parse_dualsense(status),
        }
    }
}

#[derive(Default)]
pub struct PlayStation {
    /// Controllers we already asked to switch to full Bluetooth reports.
    switched: HashSet<String>,
}

impl Provider for PlayStation {
    fn name(&self) -> &'static str {
        "playstation"
    }

    fn poll(&mut self, hid: &[DeviceInfo], out: &mut Vec<Reading>) {
        for info in hid {
            if info.vendor_id != VENDOR_SONY
                || info.usage_page != USAGE_PAGE_GENERIC_DESKTOP
                || info.usage != USAGE_GAMEPAD
            {
                continue;
            }
            let Some(&(_, model, family)) = MODELS.iter().find(|(pid, ..)| *pid == info.product_id)
            else {
                continue;
            };
            let battery = match self.read(info, family) {
                Ok(b) => b,
                Err(e) => {
                    log!("playstation: reading {} failed: {e}", info.path);
                    None
                }
            };
            let key = if info.parent.is_empty() {
                &info.path
            } else {
                &info.parent
            };
            out.push(Reading {
                id: format!("playstation:{key}"),
                name: model.to_owned(),
                kind: DeviceKind::Gamepad,
                battery,
            });
        }
    }
}

impl PlayStation {
    fn read(&mut self, info: &DeviceInfo, family: Family) -> std::io::Result<Option<Battery>> {
        let layout = family.layout();
        let dev = Device::open(info)?;
        let bluetooth = info.input_len > USB_INPUT_LEN;
        let deadline = Instant::now() + READ_TIMEOUT;
        while let Some(left) = hid::remaining(deadline) {
            let Some(report) = dev.read(left)? else {
                break;
            };
            let status = match (report.first().copied(), bluetooth) {
                (Some(id), true) if id == layout.bt_full_report => report.get(layout.bt_status),
                (Some(id), false) if id == layout.usb_report => report.get(layout.usb_status),
                (Some(id), true) if id == layout.bt_simple_report => {
                    if self.switched.insert(info.path.clone()) {
                        log!(
                            "playstation: switching {} to full Bluetooth reports",
                            info.path
                        );
                        dev.get_feature(layout.bt_calibration_feature)?;
                    }
                    None
                }
                _ => None,
            };
            if let Some(&status) = status {
                return Ok(family.parse_status(status));
            }
        }
        Ok(None)
    }
}

/// DualSense: low nibble is the level in steps of 10 %, high nibble the charge state.
fn parse_dualsense(status: u8) -> Option<Battery> {
    let step = ((status & 0x0F) * 10 + 5).min(100);
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

/// DualShock 4: low nibble is the level, bit 4 says whether a cable is plugged in.
/// With a cable, 11 means full and higher values are charging errors.
fn parse_dualshock4(status: u8) -> Option<Battery> {
    let level = status & 0x0F;
    let cable = status & 0x10 != 0;
    let step = (level * 10 + 5).min(100);
    match (cable, level) {
        (false, _) => Some(Battery {
            percent: step,
            charging: ChargeState::Discharging,
        }),
        (true, 0..=10) => Some(Battery {
            percent: step,
            charging: ChargeState::Charging,
        }),
        (true, 11) => Some(Battery {
            percent: 100,
            charging: ChargeState::Full,
        }),
        (true, _) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dualsense_discharging_level() {
        assert_eq!(
            parse_dualsense(0x07),
            Some(Battery {
                percent: 75,
                charging: ChargeState::Discharging
            })
        );
    }

    #[test]
    fn dualsense_caps_level_at_100() {
        assert_eq!(parse_dualsense(0x1A).map(|b| b.percent), Some(100));
    }

    #[test]
    fn dualsense_full_reports_100() {
        assert_eq!(
            parse_dualsense(0x20),
            Some(Battery {
                percent: 100,
                charging: ChargeState::Full
            })
        );
    }

    #[test]
    fn dualsense_error_states_are_unknown() {
        assert_eq!(parse_dualsense(0xF3), None);
    }

    #[test]
    fn dualshock4_on_battery() {
        assert_eq!(
            parse_dualshock4(0x04),
            Some(Battery {
                percent: 45,
                charging: ChargeState::Discharging
            })
        );
    }

    #[test]
    fn dualshock4_charging_and_full() {
        assert_eq!(
            parse_dualshock4(0x16).map(|b| b.charging),
            Some(ChargeState::Charging)
        );
        assert_eq!(
            parse_dualshock4(0x1B),
            Some(Battery {
                percent: 100,
                charging: ChargeState::Full
            })
        );
    }

    #[test]
    fn dualshock4_charge_error_is_unknown() {
        assert_eq!(parse_dualshock4(0x1F), None);
    }
}
