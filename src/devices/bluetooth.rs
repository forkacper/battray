//! Battery levels that Windows itself reads from Bluetooth devices: the BLE Battery
//! Service and the hands-free profile of headsets. This is the value shown in
//! Settings > Bluetooth & devices, so any device that shows a level there works here.
//!
//! Windows stores it as a one-byte percentage on one of the device's nodes. A device
//! usually has several nodes (one per service), grouped by their container ID.

use std::collections::BTreeMap;

use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_ContainerId, DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_FriendlyName,
};
use windows_sys::Win32::Foundation::DEVPROPKEY;
use windows_sys::core::GUID;

use super::{Battery, ChargeState, DeviceKind, Provider, Reading};
use crate::devnode::{self, DevNode};
use crate::hid::DeviceInfo;

/// `{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2`, undocumented but stable since Windows 10.
const DEVPKEY_BLUETOOTH_BATTERY: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5),
    pid: 2,
};

/// Bluetooth LE and Bluetooth Classic device enumerators.
const ENUMERATORS: [&str; 2] = ["BTHLE", "BTHENUM"];

pub struct Bluetooth;

/// What the nodes of one physical device say, merged.
#[derive(Default)]
struct Container {
    name: Option<String>,
    /// Name from the node that represents the device itself rather than a service.
    device_name: Option<String>,
    percent: Option<u8>,
    connected: bool,
}

impl Provider for Bluetooth {
    fn name(&self) -> &'static str {
        "bluetooth"
    }

    fn poll(&mut self, _hid: &[DeviceInfo], out: &mut Vec<Reading>) {
        let mut containers: BTreeMap<u128, Container> = BTreeMap::new();
        for id in ENUMERATORS.iter().flat_map(|e| devnode::present_ids(e)) {
            let Some(node) = DevNode::locate(&id) else {
                continue;
            };
            let Some(container_id) = node.guid(&DEVPKEY_Device_ContainerId) else {
                continue;
            };
            let entry = containers.entry(container_id).or_default();
            let name = node
                .string(&DEVPKEY_Device_FriendlyName)
                .or_else(|| node.string(&DEVPKEY_Device_DeviceDesc));
            // Device nodes look like `BTHLE\DEV_<address>\…` or `BTHENUM\DEV_<address>\…`.
            let is_device_node = id.to_ascii_uppercase().contains("\\DEV_");
            if is_device_node && entry.device_name.is_none() {
                entry.device_name.clone_from(&name);
            }
            if entry.name.is_none() {
                entry.name = name;
            }
            if let Some(level) = node.byte(&DEVPKEY_BLUETOOTH_BATTERY).filter(|l| *l <= 100) {
                entry.percent = Some(entry.percent.map_or(level, |p| p.min(level)));
                entry.connected |= node.is_started();
            }
        }

        for (container_id, c) in containers {
            let Some(percent) = c.percent else {
                continue;
            };
            out.push(Reading {
                id: format!("bluetooth:{container_id:032x}"),
                name: c
                    .device_name
                    .or(c.name)
                    .unwrap_or_else(|| "Bluetooth device".to_owned()),
                kind: DeviceKind::Other,
                // Windows keeps the last value after a disconnect; report it as stale then.
                battery: c.connected.then_some(Battery {
                    percent,
                    charging: ChargeState::Unknown,
                }),
            });
        }
    }
}
