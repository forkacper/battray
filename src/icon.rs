//! Renders tray icons: a short text (the battery percentage) in a status colour.

use std::ptr;

use windows_sys::Win32::Foundation::{RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateBitmap,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FW_BOLD, GdiFlush,
    GetDC, GetTextExtentPoint32W, HDC, HFONT, OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

use crate::util::wide;

/// Text colour as 0xRRGGBB.
pub type Rgb = u32;

pub const GREEN: Rgb = 0x004C_D964;
pub const AMBER: Rgb = 0x00FF_B020;
pub const RED: Rgb = 0x00FF_453A;
pub const BLUE: Rgb = 0x0040_A8FF;
pub const GRAY: Rgb = 0x009A_9A9A;

/// Draws `text` centred on a transparent square icon of `size` pixels.
/// Returns `None` if GDI runs out of resources.
pub fn render(text: &str, color: Rgb, size: i32) -> Option<HICON> {
    let size = size.max(16);
    // SAFETY: a null window means the screen DC; it is released at the end of `render_on`.
    let screen = unsafe { GetDC(ptr::null_mut()) };
    if screen.is_null() {
        return None;
    }
    let icon = render_on(screen, text, color, size);
    // SAFETY: `screen` came from GetDC above.
    unsafe { ReleaseDC(ptr::null_mut(), screen) };
    icon
}

fn render_on(screen: HDC, text: &str, color: Rgb, size: i32) -> Option<HICON> {
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: size,
        // Negative height: top-down rows, so pixel (x, y) is at index y * size + x.
        biHeight: -size,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..Default::default()
    };
    let info = BITMAPINFO {
        bmiHeader: header,
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = ptr::null_mut();
    // SAFETY: `info` describes a 32-bit top-down DIB; `bits` receives its pixel pointer.
    let color_bmp =
        unsafe { CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0) };
    if color_bmp.is_null() || bits.is_null() {
        return None;
    }
    // SAFETY: creates a 1-bpp all-zero mask; the alpha channel drives transparency instead.
    let mask_bmp = unsafe { CreateBitmap(size, size, 1, 1, ptr::null()) };
    // SAFETY: plain GDI object creation from a valid DC.
    let dc = unsafe { CreateCompatibleDC(screen) };

    let mut icon = None;
    if !mask_bmp.is_null() && !dc.is_null() {
        // SAFETY: `dc` is a memory DC and `color_bmp` a DIB not selected anywhere else.
        let old_bmp = unsafe { SelectObject(dc, color_bmp) };
        draw_text(dc, text, size);
        // SAFETY: restores the DC's original bitmap and makes GDI finish drawing into `bits`.
        unsafe {
            SelectObject(dc, old_bmp);
            GdiFlush();
        }
        let pixel_count = (size * size) as usize;
        // SAFETY: the DIB owns `size * size` 32-bit pixels at `bits`, and GDI is done with them.
        let pixels = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u32>(), pixel_count) };
        colorize(pixels, color);

        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask_bmp,
            hbmColor: color_bmp,
        };
        // SAFETY: both bitmaps are valid; CreateIconIndirect copies them.
        let h = unsafe { CreateIconIndirect(&icon_info) };
        icon = (!h.is_null()).then_some(h);
    }
    // SAFETY: each object was created above, is no longer selected and is deleted once.
    unsafe {
        if !dc.is_null() {
            DeleteDC(dc);
        }
        if !mask_bmp.is_null() {
            DeleteObject(mask_bmp);
        }
        DeleteObject(color_bmp);
    }
    icon
}

/// Draws white text on the black DIB, shrinking the font until the text fits.
fn draw_text(dc: HDC, text: &str, size: i32) {
    let text_w = wide(text);
    let len = (text_w.len() - 1) as i32;
    let face = wide("Segoe UI");
    let mut em = size;
    let font = loop {
        let font = create_font(em, &face);
        if font.is_null() {
            return;
        }
        let mut extent = SIZE::default();
        // SAFETY: `dc` is a valid DC, `font` a valid font, `text_w` holds `len` units.
        let fits = unsafe {
            let old = SelectObject(dc, font);
            GetTextExtentPoint32W(dc, text_w.as_ptr(), len, &mut extent);
            SelectObject(dc, old);
            extent.cx <= size
        };
        if fits || em <= 6 {
            break font;
        }
        // SAFETY: `font` is not selected into any DC.
        unsafe { DeleteObject(font) };
        em -= 1;
    };
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: size,
        bottom: size,
    };
    // SAFETY: `dc` and `font` are valid; the font is deselected and deleted afterwards.
    unsafe {
        let old = SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0x00FF_FFFF);
        DrawTextW(
            dc,
            text_w.as_ptr(),
            len,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, old);
        DeleteObject(font);
    }
}

fn create_font(em: i32, face: &[u16]) -> HFONT {
    // SAFETY: `face` is a null-terminated font name; the rest are plain values.
    unsafe {
        CreateFontW(
            -em,
            0,
            0,
            0,
            FW_BOLD as i32,
            0,
            0,
            0,
            u32::from(DEFAULT_CHARSET),
            u32::from(OUT_DEFAULT_PRECIS),
            u32::from(CLIP_DEFAULT_PRECIS),
            u32::from(ANTIALIASED_QUALITY),
            0,
            face.as_ptr(),
        )
    }
}

/// Turns the white-on-black text mask into `color` with the brightness as alpha.
fn colorize(pixels: &mut [u32], color: Rgb) {
    for px in pixels {
        let [b, g, r, _] = px.to_le_bytes();
        let alpha = r.max(g).max(b);
        *px = (u32::from(alpha) << 24) | (color & 0x00FF_FFFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorize_maps_brightness_to_alpha() {
        let mut px = [0x0000_0000, 0x00FF_FFFF, 0x0080_8080];
        colorize(&mut px, RED);
        assert_eq!(px, [0x00FF_453A, 0xFFFF_453A, 0x80FF_453A]);
    }
}
