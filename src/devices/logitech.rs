//! Logitech devices speaking HID++ 2.0: Lightspeed, Unifying and Bolt receivers,
//! devices connected directly over USB, and G series headsets on their USB dongle.
//! Newer headsets use a different transport, see [`centurion`].
//!
//! Protocol notes come from Logitech's published feature specs and the Solaar project.
//! A request is a long report `[0x11, device index, feature index, function << 4 | swid, params…]`.
//! Feature indexes are looked up once per device through the root feature (index 0).

use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use super::{Battery, ChargeState, DeviceKind, Provider, Reading};
use crate::hid::{self, Device, DeviceInfo};
use crate::log;

mod centurion;

const VENDOR_LOGITECH: u16 = 0x046D;

const REPORT_SHORT: u8 = 0x10;
const REPORT_LONG: u8 = 0x11;
const LONG_LEN: usize = 20;
const ERROR_HIDPP10: u8 = 0x8F;
const ERROR_HIDPP20: u8 = 0xFF;

/// Software ID tagged onto our requests so replies to other apps (e.g. G HUB) are ignored.
const SWID: u8 = 0x0D;
const TIMEOUT: Duration = Duration::from_millis(400);

/// Index of a device attached directly by cable. On a receiver it addresses the
/// receiver itself, which is not a HID++ 2.0 device and answers with an error.
const DIRECT_INDEX: u8 = 0xFF;
/// Receiver pairing slots.
const RECEIVER_INDEXES: [u8; 6] = [1, 2, 3, 4, 5, 6];

const FEATURE_DEVICE_NAME: u16 = 0x0005;
const FEATURE_BATTERY_STATUS: u16 = 0x1000;
const FEATURE_BATTERY_VOLTAGE: u16 = 0x1001;
const FEATURE_UNIFIED_BATTERY: u16 = 0x1004;
/// Battery voltage as read by headsets.
const FEATURE_ADC_MEASUREMENT: u16 = 0x1F20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Report {
    Short,
    Long,
}

#[derive(Debug, Clone, Copy)]
enum BatteryFeature {
    Unified(u8),
    Status(u8),
    Voltage(u8),
    Adc(u8),
}

#[derive(Debug, Clone)]
struct Slot {
    name: String,
    kind: DeviceKind,
    battery: BatteryFeature,
}

#[derive(Default)]
pub struct Logitech {
    /// Devices discovered so far, keyed by (receiver parent ID, device index).
    slots: HashMap<(String, u8), Slot>,
    centurion: centurion::Centurion,
}

impl Provider for Logitech {
    fn name(&self) -> &'static str {
        "logitech"
    }

    fn poll(&mut self, hid: &[DeviceInfo], out: &mut Vec<Reading>) {
        for long in hid.iter().filter(|i| hidpp_report(i) == Some(Report::Long)) {
            let short = hid.iter().find(|s| {
                hidpp_report(s) == Some(Report::Short)
                    && !long.parent.is_empty()
                    && s.parent == long.parent
            });
            let channel = match Channel::open(long, short) {
                Ok(c) => c,
                Err(e) => {
                    log!("logitech: cannot open {}: {e}", long.path);
                    continue;
                }
            };
            let receiver = if long.parent.is_empty() {
                &long.path
            } else {
                &long.parent
            };
            // Directly attached devices may answer on any index, so stop once one is found.
            if !self.poll_slot(&channel, receiver, DIRECT_INDEX, &long.product, out) {
                for index in RECEIVER_INDEXES {
                    self.poll_slot(&channel, receiver, index, "", out);
                }
            }
        }
        self.centurion.poll(hid, out);
    }
}

/// Which HID++ report a collection carries, if it is a Logitech HID++ collection.
fn hidpp_report(info: &DeviceInfo) -> Option<Report> {
    if info.vendor_id != VENDOR_LOGITECH {
        return None;
    }
    let (short, long) = match info.usage_page {
        // Receivers and most devices.
        0xFF00 => (0x0001, 0x0002),
        // G series headsets. Limited to USB, so Bluetooth devices, whose battery
        // Windows already reports, are not listed twice.
        0xFF43 if info.parent.to_ascii_uppercase().starts_with("USB\\") => (0x0201, 0x0202),
        _ => return None,
    };
    match info.usage {
        u if u == short => Some(Report::Short),
        u if u == long => Some(Report::Long),
        _ => None,
    }
}

impl Logitech {
    /// Reads one device index. Returns whether a HID++ 2.0 device is present there.
    /// `product` is the USB product name, used when the device does not report its own.
    fn poll_slot(
        &mut self,
        channel: &Channel,
        receiver: &str,
        index: u8,
        product: &str,
        out: &mut Vec<Reading>,
    ) -> bool {
        let key = (receiver.to_owned(), index);
        if !self.slots.contains_key(&key) {
            match discover(channel, index, product) {
                Ok(Some(slot)) => {
                    log!(
                        "logitech: found \"{}\" at index {index} on {receiver}",
                        slot.name
                    );
                    self.slots.insert(key.clone(), slot);
                }
                // A device without any battery feature.
                Ok(None) => return true,
                // Empty slot, the receiver itself, or a device that is asleep.
                Err(_) => return false,
            }
        }
        let Some(slot) = self.slots.get(&key) else {
            return false;
        };
        let battery = match read_battery(channel, index, slot.battery) {
            Ok(b) => b,
            Err(e) => {
                log!("logitech: battery read for \"{}\" failed: {e}", slot.name);
                None
            }
        };
        out.push(Reading {
            id: format!("logitech:{receiver}:{index}"),
            name: slot.name.clone(),
            kind: slot.kind,
            battery,
        });
        true
    }
}

#[derive(Debug)]
enum HidppError {
    Io(io::Error),
    Timeout,
    /// HID++ 1.0 error, sent by receivers e.g. for an empty or unreachable slot.
    Receiver(u8),
    /// HID++ 2.0 error returned by the device itself.
    Device(u8),
}

impl std::fmt::Display for HidppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Timeout => f.write_str("no reply"),
            Self::Receiver(code) => write!(f, "receiver error 0x{code:02X}"),
            Self::Device(code) => write!(f, "device error 0x{code:02X}"),
        }
    }
}

impl From<io::Error> for HidppError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// The HID++ collections of one receiver. Requests go out on the long collection;
/// replies may come back on either, since receivers send their errors as short reports.
struct Channel {
    long: Device,
    short: Option<Device>,
}

impl Channel {
    fn open(long: &DeviceInfo, short: Option<&DeviceInfo>) -> io::Result<Self> {
        let long = Device::open(long)?;
        let short = short.and_then(|s| Device::open(s).ok());
        Ok(Self { long, short })
    }

    /// Sends one request and returns the 16 parameter bytes of the matching reply.
    fn request(
        &self,
        index: u8,
        feature: u8,
        function: u8,
        params: &[u8],
    ) -> Result<[u8; 16], HidppError> {
        let fn_swid = (function << 4) | SWID;
        let mut req = [0u8; LONG_LEN];
        req[..4].copy_from_slice(&[REPORT_LONG, index, feature, fn_swid]);
        let n = params.len().min(LONG_LEN - 4);
        req[4..4 + n].copy_from_slice(&params[..n]);

        let devices: Vec<&Device> = std::iter::once(&self.long)
            .chain(self.short.as_ref())
            .collect();
        for dev in &devices {
            dev.flush();
        }
        self.long.write(&req)?;

        let deadline = Instant::now() + TIMEOUT;
        while let Some(left) = hid::remaining(deadline) {
            for report in hid::read_any(&devices, left)? {
                if let Some(reply) = match_reply(&report, index, feature, fn_swid) {
                    return reply;
                }
            }
        }
        Err(HidppError::Timeout)
    }

    /// Looks up the index of `feature` on the device, `None` if unsupported.
    fn feature_index(&self, index: u8, feature: u16) -> Result<Option<u8>, HidppError> {
        let reply = self.request(index, 0x00, 0x0, &feature.to_be_bytes())?;
        Ok((reply[0] != 0).then_some(reply[0]))
    }
}

/// Checks whether `data` answers our request; returns the outcome if it does.
fn match_reply(
    data: &[u8],
    index: u8,
    feature: u8,
    fn_swid: u8,
) -> Option<Result<[u8; 16], HidppError>> {
    let &[report_id, dev, b2, b3, ref rest @ ..] = data else {
        return None;
    };
    if !matches!(report_id, REPORT_SHORT | REPORT_LONG) || dev != index {
        return None;
    }
    let code = rest.get(1).copied().unwrap_or(0);
    match b2 {
        ERROR_HIDPP10 if b3 == feature && rest.first() == Some(&fn_swid) => {
            Some(Err(HidppError::Receiver(code)))
        }
        ERROR_HIDPP20 if b3 == feature && rest.first() == Some(&fn_swid) => {
            Some(Err(HidppError::Device(code)))
        }
        _ if b2 == feature && b3 == fn_swid => {
            let mut params = [0u8; 16];
            let n = rest.len().min(16);
            params[..n].copy_from_slice(&rest[..n]);
            Some(Ok(params))
        }
        _ => None,
    }
}

/// Probes one device index. `Ok(None)` means there is no battery-powered device there.
fn discover(channel: &Channel, index: u8, product: &str) -> Result<Option<Slot>, HidppError> {
    let battery = if let Some(i) = channel.feature_index(index, FEATURE_UNIFIED_BATTERY)? {
        BatteryFeature::Unified(i)
    } else if let Some(i) = channel.feature_index(index, FEATURE_BATTERY_STATUS)? {
        BatteryFeature::Status(i)
    } else if let Some(i) = channel.feature_index(index, FEATURE_BATTERY_VOLTAGE)? {
        BatteryFeature::Voltage(i)
    } else if let Some(i) = channel.feature_index(index, FEATURE_ADC_MEASUREMENT)? {
        BatteryFeature::Adc(i)
    } else {
        return Ok(None);
    };
    let (name, kind) = match device_name(channel, index) {
        Ok(Some(found)) => found,
        _ => {
            let product = product.trim();
            let name = if product.is_empty() {
                format!("Logitech device {index}")
            } else {
                product.to_owned()
            };
            // Only headsets report their battery through feature 0x1F20.
            let kind = match battery {
                BatteryFeature::Adc(_) => DeviceKind::Headset,
                _ => DeviceKind::Other,
            };
            (name, kind)
        }
    };
    Ok(Some(Slot {
        name,
        kind,
        battery,
    }))
}

fn device_name(channel: &Channel, index: u8) -> Result<Option<(String, DeviceKind)>, HidppError> {
    let Some(feature) = channel.feature_index(index, FEATURE_DEVICE_NAME)? else {
        return Ok(None);
    };
    let count = usize::from(channel.request(index, feature, 0x0, &[])?[0]);
    let mut name = Vec::with_capacity(count);
    while name.len() < count {
        let offset = u8::try_from(name.len()).unwrap_or(u8::MAX);
        let chunk = channel.request(index, feature, 0x1, &[offset])?;
        let take = (count - name.len()).min(chunk.len());
        name.extend_from_slice(&chunk[..take]);
    }
    let name = String::from_utf8_lossy(&name)
        .trim_matches(char::from(0))
        .trim()
        .to_owned();
    let kind = match channel.request(index, feature, 0x2, &[]) {
        Ok(reply) => kind_from_type(reply[0]),
        Err(_) => DeviceKind::Other,
    };
    Ok((!name.is_empty()).then_some((name, kind)))
}

/// Maps the device type of feature 0x0005 to our coarse kinds.
fn kind_from_type(device_type: u8) -> DeviceKind {
    match device_type {
        0 | 2 => DeviceKind::Keyboard,
        3..=5 => DeviceKind::Mouse,
        8 => DeviceKind::Headset,
        11 | 12 => DeviceKind::Gamepad,
        _ => DeviceKind::Other,
    }
}

/// `Ok(None)` means the device answered but has no reading, e.g. a headset that is switched off
/// while its dongle stays plugged in.
fn read_battery(
    channel: &Channel,
    index: u8,
    feature: BatteryFeature,
) -> Result<Option<Battery>, HidppError> {
    Ok(match feature {
        BatteryFeature::Unified(f) => Some(parse_unified(&channel.request(index, f, 0x1, &[])?)),
        BatteryFeature::Status(f) => Some(parse_status(&channel.request(index, f, 0x0, &[])?)),
        BatteryFeature::Voltage(f) => Some(parse_voltage(&channel.request(index, f, 0x0, &[])?)),
        BatteryFeature::Adc(f) => parse_adc(&channel.request(index, f, 0x0, &[])?),
    })
}

/// Feature 0x1004 `get_status`: [state of charge %, level flags, charging status, external power].
fn parse_unified(p: &[u8; 16]) -> Battery {
    let percent = if p[0] != 0 {
        p[0].min(100)
    } else {
        // Devices without a fuel gauge only report a level: full, good, low, critical.
        match p[1] {
            l if l & 0x08 != 0 => 100,
            l if l & 0x04 != 0 => 60,
            l if l & 0x02 != 0 => 20,
            l if l & 0x01 != 0 => 5,
            _ => 0,
        }
    };
    let charging = match p[2] {
        0 => ChargeState::Discharging,
        1 | 2 => ChargeState::Charging,
        3 => ChargeState::Full,
        _ => ChargeState::Unknown,
    };
    Battery { percent, charging }
}

/// Feature 0x1000 `get_battery_level_status`: [level %, next level %, status].
fn parse_status(p: &[u8; 16]) -> Battery {
    let charging = match p[2] {
        0 => ChargeState::Discharging,
        1 | 2 | 4 => ChargeState::Charging,
        3 => ChargeState::Full,
        _ => ChargeState::Unknown,
    };
    Battery {
        percent: p[0].min(100),
        charging,
    }
}

/// Feature 0x1001 `get_battery_voltage`: [voltage mV (big endian), flags].
fn parse_voltage(p: &[u8; 16]) -> Battery {
    let millivolts = u16::from_be_bytes([p[0], p[1]]);
    let flags = p[2];
    let charging = match (flags & 0x80 != 0, flags & 0x07) {
        (false, _) => ChargeState::Discharging,
        (true, 1) => ChargeState::Full,
        (true, _) => ChargeState::Charging,
    };
    Battery {
        percent: percent_from_millivolts(&LI_ION_CURVE, millivolts),
        charging,
    }
}

/// Feature 0x1F20 `get_adc_measurement`: [voltage mV (big endian), flags].
/// Flag bit 0 says the reading is valid, bit 1 that the battery is charging.
fn parse_adc(p: &[u8; 16]) -> Option<Battery> {
    let millivolts = u16::from_be_bytes([p[0], p[1]]);
    let flags = p[2];
    if flags & 0x01 == 0 {
        return None;
    }
    let charging = if flags & 0x02 != 0 {
        ChargeState::Charging
    } else {
        ChargeState::Discharging
    };
    Some(Battery {
        percent: percent_from_millivolts(&HEADSET_CURVE, millivolts),
        charging,
    })
}

/// Discharge curve as (mV, %) points, from full to empty.
type Curve = [(u16, u8)];

/// Typical Li-ion discharge curve, as used by Solaar for voltage-only devices.
const LI_ION_CURVE: [(u16, u8); 13] = [
    (4186, 100),
    (4067, 90),
    (3989, 80),
    (3922, 70),
    (3859, 60),
    (3811, 50),
    (3778, 40),
    (3751, 30),
    (3717, 20),
    (3671, 10),
    (3646, 5),
    (3579, 2),
    (3500, 0),
];

/// G series headsets run their cells lower before switching off than mice do.
const HEADSET_CURVE: [(u16, u8); 8] = [
    (4150, 100),
    (4000, 85),
    (3900, 70),
    (3830, 50),
    (3780, 30),
    (3740, 20),
    (3670, 5),
    (3350, 0),
];

fn percent_from_millivolts(curve: &Curve, mv: u16) -> u8 {
    if mv >= curve[0].0 {
        return 100;
    }
    for pair in curve.windows(2) {
        let [(hi_mv, hi_pct), (lo_mv, lo_pct)] = [pair[0], pair[1]];
        if mv >= lo_mv {
            let span = u32::from(hi_mv - lo_mv);
            let offset = u32::from(mv - lo_mv);
            let pct = u32::from(lo_pct) + offset * u32::from(hi_pct - lo_pct) / span;
            return u8::try_from(pct).unwrap_or(100);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(bytes: &[u8]) -> [u8; 16] {
        let mut p = [0u8; 16];
        p[..bytes.len()].copy_from_slice(bytes);
        p
    }

    #[test]
    fn unified_uses_state_of_charge() {
        let b = parse_unified(&params(&[83, 0x04, 0, 0]));
        assert_eq!(
            b,
            Battery {
                percent: 83,
                charging: ChargeState::Discharging
            }
        );
    }

    #[test]
    fn unified_falls_back_to_level_flags() {
        assert_eq!(parse_unified(&params(&[0, 0x02, 1, 1])).percent, 20);
    }

    #[test]
    fn unified_charge_complete_is_full() {
        assert_eq!(
            parse_unified(&params(&[100, 0x08, 3, 1])).charging,
            ChargeState::Full
        );
    }

    #[test]
    fn voltage_curve_interpolates() {
        assert_eq!(percent_from_millivolts(&LI_ION_CURVE, 4200), 100);
        assert_eq!(percent_from_millivolts(&LI_ION_CURVE, 3811), 50);
        assert_eq!(percent_from_millivolts(&LI_ION_CURVE, 3835), 55);
        assert_eq!(percent_from_millivolts(&LI_ION_CURVE, 3400), 0);
    }

    #[test]
    fn adc_reads_voltage_and_charging() {
        // 3830 mV, valid and charging.
        let b = parse_adc(&params(&[0x0E, 0xF6, 0x03]));
        assert_eq!(
            b,
            Some(Battery {
                percent: 50,
                charging: ChargeState::Charging
            })
        );
    }

    #[test]
    fn adc_without_valid_flag_has_no_reading() {
        assert_eq!(parse_adc(&params(&[0x0E, 0xF6, 0x00])), None);
    }

    #[test]
    fn matches_reply_to_our_request() {
        let mut data = vec![REPORT_LONG, 1, 4, 0x1D, 77];
        data.resize(LONG_LEN, 0);
        let reply = match_reply(&data, 1, 4, 0x1D).map(|r| r.map(|p| p[0]));
        assert!(matches!(reply, Some(Ok(77))));
    }

    #[test]
    fn ignores_reply_for_other_software_id() {
        let mut data = vec![REPORT_LONG, 1, 4, 0x1A, 77];
        data.resize(LONG_LEN, 0);
        assert!(match_reply(&data, 1, 4, 0x1D).is_none());
    }

    #[test]
    fn recognises_receiver_error() {
        let data = [REPORT_SHORT, 2, ERROR_HIDPP10, 0x00, 0x0D, 0x09, 0x00];
        assert!(matches!(
            match_reply(&data, 2, 0x00, 0x0D),
            Some(Err(HidppError::Receiver(0x09)))
        ));
    }
}
