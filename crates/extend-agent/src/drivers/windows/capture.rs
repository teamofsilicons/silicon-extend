//! Screenshots with GDI: copy the screen into a bitmap, read its pixels, write a PNG.

use std::path::Path;

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::image::bgra_to_rgba;
use super::model::Rect;

/// The rectangle covering every monitor.
pub fn virtual_screen() -> Rect {
    unsafe {
        Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN) as f64,
            y: GetSystemMetrics(SM_YVIRTUALSCREEN) as f64,
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN) as f64,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN) as f64,
        }
    }
}

/// RGBA pixels of a screen rectangle.
pub fn grab(rect: Rect) -> Result<(Vec<u8>, u32, u32), String> {
    let (x, y, w, h) = (rect.x as i32, rect.y as i32, rect.width as i32, rect.height as i32);
    if w <= 0 || h <= 0 {
        return Err("nothing to capture: the area is empty".into());
    }
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("couldn't read the screen".into());
        }
        let mem = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, HGDIOBJ(bitmap.0));
        let copied = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        SelectObject(mem, old);
        let lines = GetDIBits(
            mem,
            bitmap,
            0,
            h as u32,
            Some(bgra.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        copied.map_err(|e| format!("couldn't copy the screen: {e}"))?;
        if lines == 0 {
            return Err("couldn't read the captured pixels".into());
        }
        Ok((bgra_to_rgba(&bgra), w as u32, h as u32))
    }
}

pub fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("couldn't create {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("couldn't write the PNG: {e}"))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| format!("couldn't write the PNG: {e}"))?;
    writer.finish().map_err(|e| format!("couldn't finish the PNG: {e}"))
}
