//! Any HID device that reports its battery the standard way: the Battery Strength
//! usage (Generic Device Controls page 0x06, usage 0x20) in a feature or input report.
//!
//! Report descriptors are inspected once per collection. A device is only listed
//! after it returned a level at least once, since some descriptors declare the
//! usage without ever filling it in.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::{Battery, ChargeState, DeviceKind, Provider, Reading};
use crate::hid::{self, Descriptor, Device, DeviceInfo, ReportKind, ValueField};
use crate::log;

const USAGE_PAGE_GENERIC_DEVICE: u16 = 0x06;
const USAGE_BATTERY_STRENGTH: u16 = 0x20;
/// How long to wait for an input report that carries the level.
const INPUT_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct HidBattery {
    /// Battery fields per collection path; an empty list means the collection has none.
    fields: HashMap<String, Vec<ValueField>>,
    /// Devices that returned a level at least once, with their display name.
    known: HashMap<String, String>,
}

impl Provider for HidBattery {
    fn name(&self) -> &'static str {
        "hid-battery"
    }

    fn poll(&mut self, hid: &[DeviceInfo], out: &mut Vec<Reading>) {
        for info in hid {
            let fields = self
                .fields
                .entry(info.path.clone())
                .or_insert_with(|| probe(info));
            if fields.is_empty() {
                continue;
            }
            let key = if info.parent.is_empty() {
                &info.path
            } else {
                &info.parent
            };
            let id = format!("hid:{key}");
            match read(info, fields) {
                Some(percent) => {
                    let name = display_name(info);
                    if self.known.insert(id.clone(), name.clone()).is_none() {
                        log!(
                            "hid-battery: \"{name}\" reports Battery Strength on {}",
                            info.path
                        );
                    }
                    out.push(Reading {
                        id,
                        name,
                        kind: DeviceKind::Other,
                        battery: Some(Battery {
                            percent,
                            charging: ChargeState::Unknown,
                        }),
                    });
                }
                None => {
                    if let Some(name) = self.known.get(&id) {
                        out.push(Reading {
                            id,
                            name: name.clone(),
                            kind: DeviceKind::Other,
                            battery: None,
                        });
                    }
                }
            }
        }
    }
}

fn display_name(info: &DeviceInfo) -> String {
    match (info.manufacturer.is_empty(), info.product.is_empty()) {
        (_, false) if info.product.starts_with(&info.manufacturer) => info.product.clone(),
        (false, false) => format!("{} {}", info.manufacturer, info.product),
        (true, false) => info.product.clone(),
        _ => format!("HID device {:04X}:{:04X}", info.vendor_id, info.product_id),
    }
}

/// Finds the Battery Strength fields declared by a collection's descriptor.
fn probe(info: &DeviceInfo) -> Vec<ValueField> {
    let Ok(dev) = Device::open(info) else {
        return Vec::new();
    };
    let Some(descriptor) = dev.descriptor() else {
        return Vec::new();
    };
    [ReportKind::Feature, ReportKind::Input]
        .into_iter()
        .flat_map(|kind| {
            descriptor.find_values(kind, USAGE_PAGE_GENERIC_DEVICE, USAGE_BATTERY_STRENGTH)
        })
        .collect()
}

/// Reads the level, preferring feature reports, which can be requested on demand.
fn read(info: &DeviceInfo, fields: &[ValueField]) -> Option<u8> {
    let dev = Device::open(info).ok()?;
    let descriptor = dev.descriptor()?;
    for field in fields.iter().filter(|f| f.kind == ReportKind::Feature) {
        if let Ok(report) = dev.get_feature(field.report_id)
            && let Some(percent) = level(&descriptor, field, &report)
        {
            return Some(percent);
        }
    }
    let inputs: Vec<&ValueField> = fields
        .iter()
        .filter(|f| f.kind == ReportKind::Input)
        .collect();
    if inputs.is_empty() {
        return None;
    }
    let deadline = Instant::now() + INPUT_TIMEOUT;
    while let Some(left) = hid::remaining(deadline) {
        let report = dev.read(left).ok()??;
        let report_id = report.first().copied().unwrap_or(0);
        for field in inputs
            .iter()
            .filter(|f| f.report_id == report_id || f.report_id == 0)
        {
            if let Some(percent) = level(&descriptor, field, &report) {
                return Some(percent);
            }
        }
    }
    None
}

fn level(descriptor: &Descriptor, field: &ValueField, report: &[u8]) -> Option<u8> {
    let raw = descriptor.value(field, report)?;
    scale(raw, field.logical_min, field.logical_max)
}

/// Maps a raw value onto 0–100 using the field's logical range.
fn scale(raw: u32, min: i32, max: i32) -> Option<u8> {
    let (raw, min, max) = (i64::from(raw), i64::from(min), i64::from(max));
    if max <= min {
        return None;
    }
    let percent = (raw.clamp(min, max) - min) * 100 / (max - min);
    u8::try_from(percent).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_percent_range() {
        assert_eq!(scale(64, 0, 100), Some(64));
    }

    #[test]
    fn scales_byte_range() {
        assert_eq!(scale(255, 0, 255), Some(100));
        assert_eq!(scale(51, 0, 255), Some(20));
    }

    #[test]
    fn rejects_empty_range() {
        assert_eq!(scale(5, 0, 0), None);
    }

    #[test]
    fn clamps_out_of_range_values() {
        assert_eq!(scale(150, 0, 100), Some(100));
    }
}
