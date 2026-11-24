//! Xbox controllers and other XInput gamepads, through the Windows XInput API.
//!
//! XInput only reports a coarse level (empty, low, medium, full), so the
//! percentage is an estimate. Wired controllers have no battery and are skipped.

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::UI::Input::XboxController::{
    BATTERY_DEVTYPE_GAMEPAD, BATTERY_LEVEL_EMPTY, BATTERY_LEVEL_FULL, BATTERY_LEVEL_LOW,
    BATTERY_LEVEL_MEDIUM, BATTERY_TYPE_ALKALINE, BATTERY_TYPE_NIMH, XINPUT_BATTERY_INFORMATION,
    XINPUT_STATE, XInputGetBatteryInformation, XInputGetState, XUSER_MAX_COUNT,
};

use super::{Battery, ChargeState, DeviceKind, Provider, Reading};
use crate::hid::DeviceInfo;

pub struct XInput;

impl Provider for XInput {
    fn name(&self) -> &'static str {
        "xinput"
    }

    fn poll(&mut self, _hid: &[DeviceInfo], out: &mut Vec<Reading>) {
        for slot in 0..XUSER_MAX_COUNT {
            let mut state = XINPUT_STATE::default();
            // SAFETY: `state` is a valid, writable XINPUT_STATE.
            if unsafe { XInputGetState(slot, &mut state) } != ERROR_SUCCESS {
                continue;
            }
            let mut info = XINPUT_BATTERY_INFORMATION::default();
            // SAFETY: `info` is a valid, writable XINPUT_BATTERY_INFORMATION.
            if unsafe { XInputGetBatteryInformation(slot, BATTERY_DEVTYPE_GAMEPAD, &mut info) }
                != ERROR_SUCCESS
            {
                continue;
            }
            let Some(battery) = parse(info.BatteryType, info.BatteryLevel) else {
                continue;
            };
            out.push(Reading {
                id: format!("xinput:{slot}"),
                name: format!("Xbox controller {}", slot + 1),
                kind: DeviceKind::Gamepad,
                battery: Some(battery),
            });
        }
    }
}

/// Only real batteries count. Wired pads report `WIRED`, and virtual pads
/// (e.g. Steam Input mirroring a PlayStation controller) report `UNKNOWN`.
fn parse(battery_type: u8, level: u8) -> Option<Battery> {
    if !matches!(battery_type, BATTERY_TYPE_ALKALINE | BATTERY_TYPE_NIMH) {
        return None;
    }
    let percent = match level {
        BATTERY_LEVEL_EMPTY => 5,
        BATTERY_LEVEL_LOW => 25,
        BATTERY_LEVEL_MEDIUM => 60,
        BATTERY_LEVEL_FULL => 100,
        _ => return None,
    };
    Some(Battery {
        percent,
        charging: ChargeState::Unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::Input::XboxController::{BATTERY_TYPE_UNKNOWN, BATTERY_TYPE_WIRED};

    #[test]
    fn maps_levels_to_estimates() {
        assert_eq!(
            parse(BATTERY_TYPE_NIMH, BATTERY_LEVEL_MEDIUM).map(|b| b.percent),
            Some(60)
        );
        assert_eq!(
            parse(BATTERY_TYPE_ALKALINE, BATTERY_LEVEL_FULL).map(|b| b.percent),
            Some(100)
        );
    }

    #[test]
    fn skips_wired_and_virtual_pads() {
        assert_eq!(parse(BATTERY_TYPE_WIRED, BATTERY_LEVEL_FULL), None);
        assert_eq!(parse(BATTERY_TYPE_UNKNOWN, BATTERY_LEVEL_FULL), None);
    }
}
