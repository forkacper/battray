//! Device support. Every vendor protocol lives in its own module and implements [`Provider`].
//!
//! To add a device family: create a module, implement [`Provider`] for it and
//! register it in [`providers`].

mod bluetooth;
mod keychron;
mod logitech;
mod playstation;
mod xinput;

use crate::hid::DeviceInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Mouse,
    Keyboard,
    Gamepad,
    Headset,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeState {
    Discharging,
    Charging,
    Full,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    /// 0–100. Some devices only report coarse steps, so this can be an estimate.
    pub percent: u8,
    pub charging: ChargeState,
}

/// One device as seen during a poll.
#[derive(Debug, Clone)]
pub struct Reading {
    /// Stable identifier, used to remember settings and the last known level.
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    /// `None` when the device is known (e.g. paired to a receiver) but did not answer,
    /// typically because it is asleep or switched off.
    pub battery: Option<Battery>,
}

/// A device family that knows how to find its devices and read their battery.
pub trait Provider: Send {
    /// Short name used in logs and diagnostics.
    fn name(&self) -> &'static str;

    /// Appends a reading for every supported device currently present.
    /// `hid` is the HID collection list enumerated once per poll for all providers.
    fn poll(&mut self, hid: &[DeviceInfo], out: &mut Vec<Reading>);
}

/// All built-in providers, in display order.
pub fn providers() -> Vec<Box<dyn Provider>> {
    vec![
        Box::new(logitech::Logitech::default()),
        Box::new(keychron::Keychron::default()),
        Box::new(playstation::PlayStation::default()),
        Box::new(xinput::XInput),
        Box::new(bluetooth::Bluetooth),
    ]
}
