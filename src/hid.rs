//! Thin wrapper over the Windows HID API: enumeration plus overlapped report I/O.
//!
//! Every device is opened with full read/write sharing, so battray can talk to
//! a device while vendor software (G HUB, Steam, ...) has it open too.

use std::io;
use std::mem;
use std::ptr;
use std::time::{Duration, Instant};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_IDW, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_PropertyW, CM_Get_Parent,
    CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_REPORT_TYPE, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS,
    HidD_FlushQueue, HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid,
    HidD_GetManufacturerString, HidD_GetPreparsedData, HidD_GetProductString, HidP_Feature,
    HidP_GetCaps, HidP_GetUsageValue, HidP_GetValueCaps, HidP_Input, PHIDP_PREPARSED_DATA,
};
use windows_sys::Win32::Devices::Properties::{DEVPKEY_Device_InstanceId, DEVPROPTYPE};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED,
};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForMultipleObjects};
use windows_sys::core::GUID;

use crate::util::{from_wide, wide};

/// `IOCTL_HID_GET_FEATURE` and `IOCTL_HID_SET_FEATURE` from hidclass.h (not exported by windows-sys).
const IOCTL_HID_GET_FEATURE: u32 = 0x000B_0192;
const IOCTL_HID_SET_FEATURE: u32 = 0x000B_0191;
/// How long a write may take before it is cancelled.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Static description of one HID top-level collection.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Device interface path, as passed to `CreateFileW`.
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub version: u16,
    pub usage_page: u16,
    pub usage: u16,
    /// Report lengths in bytes, including the report ID byte.
    pub input_len: u16,
    pub output_len: u16,
    pub feature_len: u16,
    pub product: String,
    pub manufacturer: String,
    /// Instance ID of the parent device node. Collections of one physical
    /// interface share it, so it groups e.g. the short and long HID++ collections.
    pub parent: String,
}

/// Lists every present HID collection. Collections that cannot be queried are skipped.
pub fn enumerate() -> Vec<DeviceInfo> {
    interface_paths(&hid_guid())
        .into_iter()
        .filter_map(|path| query_info(&path))
        .collect()
}

/// Returns the HID interface class GUID, used for device arrival notifications.
pub fn hid_guid() -> GUID {
    let mut guid = GUID::from_u128(0);
    // SAFETY: `guid` is a valid, writable GUID.
    unsafe { HidD_GetHidGuid(&mut guid) };
    guid
}

fn interface_paths(guid: &GUID) -> Vec<String> {
    loop {
        let mut len = 0u32;
        // SAFETY: `len` is writable and `guid` outlives the call.
        let cr = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut len,
                guid,
                ptr::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr != CR_SUCCESS || len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: `buf` holds exactly `len` UTF-16 units, as the API was told.
        let cr = unsafe {
            CM_Get_Device_Interface_ListW(
                guid,
                ptr::null(),
                buf.as_mut_ptr(),
                len,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        match cr {
            CR_SUCCESS => {
                return buf
                    .split(|&c| c == 0)
                    .filter(|s| !s.is_empty())
                    .map(String::from_utf16_lossy)
                    .collect();
            }
            // A device arrived between the two calls; retry with a fresh size.
            CR_BUFFER_SMALL => continue,
            _ => return Vec::new(),
        }
    }
}

fn query_info(path: &str) -> Option<DeviceInfo> {
    // Zero access rights are enough for attributes and capabilities, and they
    // work even on keyboard and mouse collections that Windows opens exclusively.
    let handle = open_handle(path, 0).ok()?;
    let mut attrs = HIDD_ATTRIBUTES {
        Size: mem::size_of::<HIDD_ATTRIBUTES>() as u32,
        ..Default::default()
    };
    // SAFETY: `handle` is an open HID handle and `attrs.Size` is initialised.
    if !unsafe { HidD_GetAttributes(handle.0, &mut attrs) } {
        return None;
    }
    let caps = query_caps(&handle)?;
    Some(DeviceInfo {
        path: path.to_owned(),
        vendor_id: attrs.VendorID,
        product_id: attrs.ProductID,
        version: attrs.VersionNumber,
        usage_page: caps.UsagePage,
        usage: caps.Usage,
        input_len: caps.InputReportByteLength,
        output_len: caps.OutputReportByteLength,
        feature_len: caps.FeatureReportByteLength,
        // SAFETY: `handle` is an open HID handle; the callee bounds writes by the buffer size.
        product: hid_string(|b, l| unsafe { HidD_GetProductString(handle.0, b, l) }),
        // SAFETY: as above.
        manufacturer: hid_string(|b, l| unsafe { HidD_GetManufacturerString(handle.0, b, l) }),
        parent: parent_instance_id(path).unwrap_or_default(),
    })
}

fn query_caps(handle: &Handle) -> Option<HIDP_CAPS> {
    let mut data: PHIDP_PREPARSED_DATA = 0;
    // SAFETY: `handle` is an open HID handle and `data` is writable.
    if !unsafe { HidD_GetPreparsedData(handle.0, &mut data) } {
        return None;
    }
    let mut caps = HIDP_CAPS::default();
    // SAFETY: `data` was just returned by HidD_GetPreparsedData and is freed right after.
    let status = unsafe { HidP_GetCaps(data, &mut caps) };
    // SAFETY: `data` came from HidD_GetPreparsedData and is not used afterwards.
    unsafe { HidD_FreePreparsedData(data) };
    (status == HIDP_STATUS_SUCCESS).then_some(caps)
}

fn hid_string(get: impl FnOnce(*mut core::ffi::c_void, u32) -> bool) -> String {
    // USB string descriptors hold at most 126 UTF-16 units.
    let mut buf = [0u16; 127];
    if get(buf.as_mut_ptr().cast(), mem::size_of_val(&buf) as u32) {
        from_wide(&buf).trim().to_owned()
    } else {
        String::new()
    }
}

fn parent_instance_id(path: &str) -> Option<String> {
    let path_w = wide(path);
    let mut buf = [0u16; 512];
    let mut size = mem::size_of_val(&buf) as u32;
    let mut prop_type: DEVPROPTYPE = 0;
    // SAFETY: `path_w` is null-terminated; `buf` and `size` describe a writable buffer.
    let cr = unsafe {
        CM_Get_Device_Interface_PropertyW(
            path_w.as_ptr(),
            &DEVPKEY_Device_InstanceId,
            &mut prop_type,
            buf.as_mut_ptr().cast(),
            &mut size,
            0,
        )
    };
    if cr != CR_SUCCESS {
        return None;
    }
    let mut devinst = 0u32;
    // SAFETY: `buf` now holds a null-terminated instance ID.
    if unsafe { CM_Locate_DevNodeW(&mut devinst, buf.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) }
        != CR_SUCCESS
    {
        return None;
    }
    let mut parent = 0u32;
    // SAFETY: `devinst` is a valid device node handle and `parent` is writable.
    if unsafe { CM_Get_Parent(&mut parent, devinst, 0) } != CR_SUCCESS {
        return None;
    }
    let mut id = [0u16; 512];
    // SAFETY: `id` is a writable buffer of the stated length.
    if unsafe { CM_Get_Device_IDW(parent, id.as_mut_ptr(), id.len() as u32, 0) } != CR_SUCCESS {
        return None;
    }
    Some(from_wide(&id))
}

/// Owned Win32 handle, closed on drop.
struct Handle(HANDLE);

// SAFETY: kernel object handles are not tied to the thread that created them.
unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

fn open_handle(path: &str, access: u32) -> io::Result<Handle> {
    let path_w = wide(path);
    // SAFETY: `path_w` is a null-terminated path; all other arguments are plain values.
    let h = unsafe {
        CreateFileW(
            path_w.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(h))
    }
}

fn new_event() -> io::Result<Handle> {
    // SAFETY: creates an unnamed manual-reset event; no pointers are passed.
    let h = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if h.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(h))
    }
}

/// An open HID collection that can exchange reports.
pub struct Device {
    handle: Handle,
    event: Handle,
    input_len: usize,
    output_len: usize,
    feature_len: usize,
}

impl Device {
    /// Opens a collection for report I/O, falling back to read-only access and then to
    /// no access at all. The last still allows feature reports on collections that
    /// Windows keeps for itself, such as keyboards and mice.
    pub fn open(info: &DeviceInfo) -> io::Result<Self> {
        let handle = open_handle(&info.path, GENERIC_READ | GENERIC_WRITE)
            .or_else(|_| open_handle(&info.path, GENERIC_READ))
            .or_else(|_| open_handle(&info.path, 0))?;
        Ok(Self {
            handle,
            event: new_event()?,
            input_len: usize::from(info.input_len),
            output_len: usize::from(info.output_len),
            feature_len: usize::from(info.feature_len),
        })
    }

    /// Drops input reports that queued up before this call.
    pub fn flush(&self) {
        // SAFETY: the handle is an open HID handle.
        unsafe { HidD_FlushQueue(self.handle.0) };
    }

    /// Sends an output report. `data[0]` is the report ID; the report is zero-padded
    /// to the collection's output report length.
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut buf = data.to_vec();
        buf.resize(self.output_len.max(data.len()), 0);
        let len = buf.len() as u32;
        let handle = self.handle.0;
        let written = self.overlapped(WRITE_TIMEOUT, |ov| {
            // SAFETY: `buf` and `ov` stay alive and unmoved until the I/O completes or is cancelled.
            unsafe { WriteFile(handle, buf.as_ptr(), len, ptr::null_mut(), ov) }
        })?;
        match written {
            Some(_) => Ok(()),
            None => Err(io::Error::from(io::ErrorKind::TimedOut)),
        }
    }

    /// Reads the next input report, or returns `None` on timeout.
    pub fn read(&self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        Ok(read_any(&[self], timeout)?.into_iter().next())
    }

    /// Reads feature report `report_id`. The returned buffer starts with the report ID.
    pub fn get_feature(&self, report_id: u8) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; self.feature_len.max(1)];
        buf[0] = report_id;
        let len = buf.len() as u32;
        let handle = self.handle.0;
        let ptr = buf.as_mut_ptr();
        let read = self.overlapped(WRITE_TIMEOUT, |ov| {
            // SAFETY: `buf` and `ov` stay alive and unmoved until the I/O completes or is
            // cancelled. The buffer is both input (report ID) and output, as hidclass expects.
            unsafe {
                DeviceIoControl(
                    handle,
                    IOCTL_HID_GET_FEATURE,
                    ptr.cast(),
                    len,
                    ptr.cast(),
                    len,
                    ptr::null_mut(),
                    ov,
                )
            }
        })?;
        match read {
            Some(n) => {
                buf.truncate(n as usize);
                Ok(buf)
            }
            None => Err(io::Error::from(io::ErrorKind::TimedOut)),
        }
    }

    /// Sends a feature report. `data[0]` is the report ID; the report is zero-padded
    /// to the collection's feature report length.
    pub fn set_feature(&self, data: &[u8]) -> io::Result<()> {
        let mut buf = data.to_vec();
        buf.resize(self.feature_len.max(data.len()), 0);
        let len = buf.len() as u32;
        let handle = self.handle.0;
        let ptr = buf.as_ptr();
        let sent = self.overlapped(WRITE_TIMEOUT, |ov| {
            // SAFETY: `buf` and `ov` stay alive and unmoved until the I/O completes or is cancelled.
            unsafe {
                DeviceIoControl(
                    handle,
                    IOCTL_HID_SET_FEATURE,
                    ptr.cast(),
                    len,
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    ov,
                )
            }
        })?;
        match sent {
            Some(_) => Ok(()),
            None => Err(io::Error::from(io::ErrorKind::TimedOut)),
        }
    }

    /// The parsed report descriptor, used to locate values inside reports.
    pub fn descriptor(&self) -> Option<Descriptor> {
        let mut data: PHIDP_PREPARSED_DATA = 0;
        // SAFETY: the handle is an open HID handle and `data` is writable.
        unsafe { HidD_GetPreparsedData(self.handle.0, &mut data) }.then_some(Descriptor(data))
    }

    /// Runs one overlapped operation and waits for it. Returns the transferred
    /// byte count, or `None` if it timed out and was cancelled.
    fn overlapped(
        &self,
        timeout: Duration,
        start: impl FnOnce(*mut OVERLAPPED) -> windows_sys::core::BOOL,
    ) -> io::Result<Option<u32>> {
        let mut ov = OVERLAPPED {
            hEvent: self.event.0,
            ..Default::default()
        };
        // SAFETY: the event handle is owned by `self`.
        unsafe { ResetEvent(self.event.0) };
        if start(&mut ov) == 0 {
            // SAFETY: reads the calling thread's last error code.
            let err = unsafe { GetLastError() };
            if err != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(err as i32));
            }
        }
        let events = [self.event.0];
        // SAFETY: `events` holds one valid event handle.
        let wait = unsafe { WaitForMultipleObjects(1, events.as_ptr(), 0, millis(timeout)) };
        if wait == WAIT_OBJECT_0 {
            finish(self.handle.0, &ov, false).map(Some)
        } else {
            cancel(self.handle.0, &ov);
            if wait == WAIT_FAILED {
                Err(io::Error::last_os_error())
            } else {
                Ok(None)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    Input,
    Feature,
}

impl ReportKind {
    fn raw(self) -> HIDP_REPORT_TYPE {
        match self {
            Self::Input => HidP_Input,
            Self::Feature => HidP_Feature,
        }
    }
}

/// Where a value with a given usage lives, as declared by the report descriptor.
#[derive(Debug, Clone, Copy)]
pub struct ValueField {
    pub kind: ReportKind,
    pub report_id: u8,
    pub usage_page: u16,
    pub usage: u16,
    pub link_collection: u16,
    pub logical_min: i32,
    pub logical_max: i32,
}

/// Parsed report descriptor (HID "preparsed data") of an open collection.
pub struct Descriptor(PHIDP_PREPARSED_DATA);

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the data came from HidD_GetPreparsedData and is freed exactly once.
        unsafe { HidD_FreePreparsedData(self.0) };
    }
}

impl Descriptor {
    /// Every value field with `usage_page`/`usage` in reports of `kind`.
    pub fn find_values(&self, kind: ReportKind, usage_page: u16, usage: u16) -> Vec<ValueField> {
        let mut caps = HIDP_CAPS::default();
        // SAFETY: `self.0` is valid preparsed data and `caps` is writable.
        if unsafe { HidP_GetCaps(self.0, &mut caps) } != HIDP_STATUS_SUCCESS {
            return Vec::new();
        }
        let mut count = match kind {
            ReportKind::Input => caps.NumberInputValueCaps,
            ReportKind::Feature => caps.NumberFeatureValueCaps,
        };
        if count == 0 {
            return Vec::new();
        }
        let mut values = vec![HIDP_VALUE_CAPS::default(); usize::from(count)];
        // SAFETY: `values` holds `count` entries, as the API is told.
        if unsafe { HidP_GetValueCaps(kind.raw(), values.as_mut_ptr(), &mut count, self.0) }
            != HIDP_STATUS_SUCCESS
        {
            return Vec::new();
        }
        values.truncate(usize::from(count));
        values
            .iter()
            .filter(|v| v.UsagePage == usage_page && covers(v, usage))
            .map(|v| ValueField {
                kind,
                report_id: v.ReportID,
                usage_page,
                usage,
                link_collection: v.LinkCollection,
                logical_min: v.LogicalMin,
                logical_max: v.LogicalMax,
            })
            .collect()
    }

    /// Extracts `field` from a report of the matching kind and ID (with report ID byte).
    pub fn value(&self, field: &ValueField, report: &[u8]) -> Option<u32> {
        let mut value = 0u32;
        let len = u32::try_from(report.len()).ok()?;
        // SAFETY: `report` is readable for `len` bytes and `self.0` is valid preparsed data.
        let status = unsafe {
            HidP_GetUsageValue(
                field.kind.raw(),
                field.usage_page,
                field.link_collection,
                field.usage,
                &mut value,
                self.0,
                report.as_ptr(),
                len,
            )
        };
        (status == HIDP_STATUS_SUCCESS).then_some(value)
    }
}

fn covers(cap: &HIDP_VALUE_CAPS, usage: u16) -> bool {
    // SAFETY: both union variants are plain u16 fields; `IsRange` says which one is valid.
    unsafe {
        if cap.IsRange {
            (cap.Anonymous.Range.UsageMin..=cap.Anonymous.Range.UsageMax).contains(&usage)
        } else {
            cap.Anonymous.NotRange.Usage == usage
        }
    }
}

/// Waits until at least one of `devices` delivers an input report, or the timeout passes.
///
/// Returns every report that completed, so a report arriving on one collection is never
/// dropped because another one won the wait. An empty result means timeout.
pub fn read_any(devices: &[&Device], timeout: Duration) -> io::Result<Vec<Vec<u8>>> {
    let mut bufs: Vec<Vec<u8>> = devices
        .iter()
        .map(|d| vec![0u8; d.input_len.max(1)])
        .collect();
    let mut ovs: Vec<OVERLAPPED> = devices
        .iter()
        .map(|d| OVERLAPPED {
            hEvent: d.event.0,
            ..Default::default()
        })
        .collect();
    let events: Vec<HANDLE> = devices.iter().map(|d| d.event.0).collect();

    // `bufs` and `ovs` are never resized below, so their elements keep stable addresses
    // for the whole lifetime of the pending reads.
    let mut pending = vec![false; devices.len()];
    let mut first_err = None;
    for (i, dev) in devices.iter().enumerate() {
        // SAFETY: the event handle is owned by `dev`.
        unsafe { ResetEvent(dev.event.0) };
        let len = bufs[i].len() as u32;
        // SAFETY: `bufs[i]` and `ovs[i]` outlive the read; it is completed or cancelled below.
        let ok = unsafe {
            ReadFile(
                dev.handle.0,
                bufs[i].as_mut_ptr(),
                len,
                ptr::null_mut(),
                &mut ovs[i],
            )
        };
        // SAFETY: reads the calling thread's last error code.
        if ok != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING {
            pending[i] = true;
        } else {
            first_err.get_or_insert_with(io::Error::last_os_error);
        }
    }
    if !pending.contains(&true) {
        return Err(first_err.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotConnected)));
    }

    // SAFETY: `events` holds valid event handles, one per device.
    let wait =
        unsafe { WaitForMultipleObjects(events.len() as u32, events.as_ptr(), 0, millis(timeout)) };
    if wait == WAIT_FAILED {
        first_err = Some(io::Error::last_os_error());
    }

    let mut reports = Vec::new();
    for (i, dev) in devices.iter().enumerate() {
        if !pending[i] {
            continue;
        }
        match finish(dev.handle.0, &ovs[i], false) {
            Ok(n) => {
                let mut data = mem::take(&mut bufs[i]);
                data.truncate(n as usize);
                reports.push(data);
            }
            Err(_) => cancel(dev.handle.0, &ovs[i]),
        }
    }
    if reports.is_empty()
        && wait != WAIT_TIMEOUT
        && let Some(err) = first_err
    {
        return Err(err);
    }
    Ok(reports)
}

/// Collects the result of an overlapped operation. With `wait == false` an
/// unfinished operation yields an error.
fn finish(handle: HANDLE, ov: &OVERLAPPED, wait: bool) -> io::Result<u32> {
    let mut n = 0u32;
    // SAFETY: `ov` is the OVERLAPPED of an operation issued on `handle`.
    if unsafe { GetOverlappedResult(handle, ov, &mut n, i32::from(wait)) } != 0 {
        Ok(n)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Cancels an overlapped operation and waits until the kernel is done with its buffers.
fn cancel(handle: HANDLE, ov: &OVERLAPPED) {
    // SAFETY: `ov` belongs to an operation issued on `handle`.
    unsafe { CancelIoEx(handle, ov) };
    // The operation either completes or reports cancellation; both are fine here.
    let _ = finish(handle, ov, true);
}

fn millis(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX - 1)
}

/// Time left until `deadline`, or `None` once it has passed.
pub fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
}
