//! Read-only access to the Windows device tree (device nodes and their properties).

use std::mem;
use std::ptr;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_PRESENT, CM_Get_DevNode_PropertyW,
    CM_Get_DevNode_Status, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW,
    CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_BUFFER_SMALL, CR_SUCCESS, DN_HAS_PROBLEM,
    DN_STARTED,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPROP_TYPE_BYTE, DEVPROP_TYPE_GUID, DEVPROP_TYPE_STRING, DEVPROPTYPE,
};
use windows_sys::Win32::Foundation::DEVPROPKEY;
use windows_sys::core::GUID;

use crate::util::{from_wide, wide};

/// Instance IDs of all present device nodes created by `enumerator` (e.g. `BTHLE`).
pub fn present_ids(enumerator: &str) -> Vec<String> {
    let filter = wide(enumerator);
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    loop {
        let mut len = 0u32;
        // SAFETY: `filter` is null-terminated and `len` is writable.
        if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, filter.as_ptr(), flags) } != CR_SUCCESS
            || len == 0
        {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: `buf` holds exactly `len` UTF-16 units, as the API was told.
        let cr = unsafe { CM_Get_Device_ID_ListW(filter.as_ptr(), buf.as_mut_ptr(), len, flags) };
        match cr {
            CR_SUCCESS => {
                return buf
                    .split(|&c| c == 0)
                    .filter(|s| !s.is_empty())
                    .map(String::from_utf16_lossy)
                    .collect();
            }
            // A device appeared between the two calls; retry with a fresh size.
            CR_BUFFER_SMALL => continue,
            _ => return Vec::new(),
        }
    }
}

/// A located device node.
#[derive(Clone, Copy)]
pub struct DevNode(u32);

impl DevNode {
    pub fn locate(instance_id: &str) -> Option<Self> {
        let id = wide(instance_id);
        let mut devinst = 0u32;
        // SAFETY: `id` is null-terminated and `devinst` is writable.
        let cr = unsafe { CM_Locate_DevNodeW(&mut devinst, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) };
        (cr == CR_SUCCESS).then_some(Self(devinst))
    }

    /// Whether the node's driver is running without problems.
    pub fn is_started(self) -> bool {
        let (mut status, mut problem) = (0u32, 0u32);
        // SAFETY: both out-parameters are writable.
        let cr = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, self.0, 0) };
        cr == CR_SUCCESS && status & DN_STARTED != 0 && status & DN_HAS_PROBLEM == 0
    }

    pub fn byte(self, key: &DEVPROPKEY) -> Option<u8> {
        let (ty, data) = self.property(key)?;
        (ty == DEVPROP_TYPE_BYTE)
            .then(|| data.first().copied())
            .flatten()
    }

    pub fn string(self, key: &DEVPROPKEY) -> Option<String> {
        let (ty, data) = self.property(key)?;
        if ty != DEVPROP_TYPE_STRING {
            return None;
        }
        let units: Vec<u16> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let s = from_wide(&units).trim().to_owned();
        (!s.is_empty()).then_some(s)
    }

    pub fn guid(self, key: &DEVPROPKEY) -> Option<u128> {
        let (ty, data) = self.property(key)?;
        if ty != DEVPROP_TYPE_GUID || data.len() < mem::size_of::<GUID>() {
            return None;
        }
        let bytes: [u8; 16] = data[..16].try_into().ok()?;
        Some(u128::from_le_bytes(bytes))
    }

    fn property(self, key: &DEVPROPKEY) -> Option<(DEVPROPTYPE, Vec<u8>)> {
        let mut ty: DEVPROPTYPE = 0;
        let mut size = 0u32;
        // SAFETY: a null buffer with size 0 asks for the required size only.
        let cr = unsafe {
            CM_Get_DevNode_PropertyW(self.0, key, &mut ty, ptr::null_mut(), &mut size, 0)
        };
        if cr != CR_BUFFER_SMALL || size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        // SAFETY: `data` is writable for `size` bytes, as the API was told.
        let cr = unsafe {
            CM_Get_DevNode_PropertyW(self.0, key, &mut ty, data.as_mut_ptr(), &mut size, 0)
        };
        if cr != CR_SUCCESS {
            return None;
        }
        data.truncate(size as usize);
        Some((ty, data))
    }
}
