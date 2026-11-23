//! Small helpers shared by the Win32 code.

use std::iter;

/// Encodes `s` as a null-terminated UTF-16 string for Win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(iter::once(0)).collect()
}

/// Decodes a UTF-16 buffer up to the first null terminator.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Copies `s` into a fixed-size UTF-16 field, truncating and always leaving a null terminator.
pub fn copy_wide(dst: &mut [u16], s: &str) {
    let Some(max) = dst.len().checked_sub(1) else {
        return;
    };
    let mut len = 0;
    for (slot, unit) in dst.iter_mut().zip(s.encode_utf16().take(max)) {
        *slot = unit;
        len += 1;
    }
    dst[len] = 0;
}
