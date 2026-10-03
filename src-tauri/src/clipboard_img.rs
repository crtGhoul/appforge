//! Clipboard image reading beyond the plugin's format list (v0.9.2).
//!
//! v0.9.1 read images through tauri-plugin-clipboard-manager, whose
//! `read_image` only accepts the registered "PNG" format or CF_DIBV5 on
//! Windows (trusting the OS to synthesize the latter from CF_DIB), and
//! only the image/png X11 target on Linux. A Win+Shift+S screenshot —
//! plain CF_DIB from the Snipping Tool, verified pasting into Paint — was
//! missed on the user's PC, so the watcher now reads the DIB itself
//! instead of hoping the narrow path covers it:
//! - Windows: CF_DIBV5, then CF_DIB, then the registered "PNG" format.
//! - Linux: image/png via the plugin (unchanged), then image/bmp direct.
//!
//! `decode_dib` is pure logic (any DIB header flavor -> RGBA) and is
//! unit-tested on every platform; the platform readers are thin.

/// Decoded image: 8-bit RGBA, row-major, top row first.
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

/// Sanity cap for decoded dimensions; real screenshots are far smaller and
/// this keeps a hostile clipboard from asking for gigabytes.
const MAX_DIM: i32 = 16384;

fn le_u16(dib: &[u8], off: usize) -> Result<u16, String> {
    dib.get(off..off + 2)
        .and_then(|b| b.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| "DIB ends inside the header".to_string())
}

fn le_u32(dib: &[u8], off: usize) -> Result<u32, String> {
    dib.get(off..off + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "DIB ends inside the header".to_string())
}

fn le_i32(dib: &[u8], off: usize) -> Result<i32, String> {
    dib.get(off..off + 4)
        .and_then(|b| b.try_into().ok())
        .map(i32::from_le_bytes)
        .ok_or_else(|| "DIB ends inside the header".to_string())
}

/// Scale one channel out of a bitfield mask to 8 bits.
fn extract_channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    // Contiguous masks in real DIBs; clamp defensively anyway.
    let bits = (32 - mask.leading_zeros() - shift).clamp(1, 16);
    let value = (pixel & mask) >> shift;
    ((value as u64 * 255) / ((1u64 << bits) - 1)) as u8
}

/// Decode a CF_DIB / CF_DIBV5 memory block into RGBA: 12-byte
/// BITMAPCOREHEADER, 40-byte BITMAPINFOHEADER (and its 52/56/64/108/124
/// extensions), 1/4/8-bit paletted, 16/24/32-bit, BI_RGB and BI_BITFIELDS,
/// bottom-up and top-down. Returns a plain-language error, never panics.
pub fn decode_dib(dib: &[u8]) -> Result<RgbaImage, String> {
    if dib.len() < 14 {
        return Err("DIB too short for a header".to_string());
    }
    let header_size = le_u32(dib, 0)? as usize;

    // Geometry + compression, per header flavor.
    let (width, height, bit_count, compression, palette_off, pixel_off, alpha_mask);
    if header_size == 12 {
        width = le_u16(dib, 4)? as i32;
        height = le_u16(dib, 6)? as i32;
        bit_count = le_u16(dib, 10)?;
        compression = BI_RGB;
        palette_off = 12;
        pixel_off = 12; // adjusted below once the palette size is known
        alpha_mask = 0;
    } else if header_size >= 40 {
        if dib.len() < 40 {
            return Err("DIB too short for a BITMAPINFOHEADER".to_string());
        }
        width = le_i32(dib, 4)?;
        height = le_i32(dib, 8)?;
        bit_count = le_u16(dib, 14)?;
        compression = le_u32(dib, 16)?;
        // Bitfield masks: inside the header for V4/V5 (108/124), right
        // after it for the 40..108 flavors.
        let masks_inside = header_size >= 108;
        let mask_off = if masks_inside { 40 } else { header_size };
        if matches!(compression, BI_BITFIELDS | BI_ALPHABITFIELDS) && dib.len() < mask_off + 12 {
            return Err("DIB ends inside the bitfield masks".to_string());
        }
        // V5 can carry color-profile bytes between header and pixels.
        let profile = if header_size >= 124 {
            le_u32(dib, 116).unwrap_or(0) as usize
        } else {
            0
        };
        pixel_off = header_size
            + profile
            + if matches!(compression, BI_BITFIELDS | BI_ALPHABITFIELDS) && !masks_inside {
                12
            } else {
                0
            };
        palette_off = pixel_off; // adjusted below for paletted depths
        // The alpha mask lives at offset 52, which only exists in V4/V5
        // headers; a 40-byte header has no such field.
        alpha_mask = if header_size >= 108 {
            le_u32(dib, 52).unwrap_or(0)
        } else {
            0
        };
    } else {
        return Err(format!("unsupported DIB header size {header_size}"));
    }

    if width <= 0 || width > MAX_DIM || height == 0 || height.abs() > MAX_DIM {
        return Err(format!("implausible DIB dimensions {width}x{height}"));
    }
    if !matches!(bit_count, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(format!("unsupported DIB bit depth {bit_count}"));
    }
    if bit_count <= 8 && compression != BI_RGB {
        return Err("paletted DIB with compression".to_string());
    }
    if !matches!(compression, BI_RGB | BI_BITFIELDS | BI_ALPHABITFIELDS) {
        return Err(format!("unsupported DIB compression {compression}"));
    }

    let w = width as u32;
    let h = height.unsigned_abs();
    let top_down = height < 0;
    let stride = ((w as u64 * bit_count as u64).div_ceil(32) * 4) as usize;
    let need = pixel_off
        .checked_add(stride.checked_mul(h as usize).ok_or("DIB too large")?)
        .ok_or("DIB too large")?;
    // Paletted depths keep their color table between palette_off and the
    // pixel data; locate the pixels past it.
    let (palette, pixels);
    if bit_count <= 8 {
        let entry_size = if header_size == 12 { 3 } else { 4 };
        let max_colors = 1usize << bit_count;
        let num_colors = if header_size == 12 {
            max_colors
        } else {
            let claimed = le_u32(dib, 32).unwrap_or(0) as usize;
            if claimed == 0 || claimed > max_colors {
                max_colors
            } else {
                claimed
            }
        };
        let table_end = palette_off
            .checked_add(num_colors.checked_mul(entry_size).ok_or("DIB too large")?)
            .ok_or("DIB too large")?;
        if dib.len() < table_end {
            return Err("DIB ends inside the palette".to_string());
        }
        let mut pal = Vec::with_capacity(num_colors);
        for i in 0..num_colors {
            let o = palette_off + i * entry_size;
            pal.push([dib[o + 2], dib[o + 1], dib[o], 255]); // BGR(A) -> RGB
        }
        let pix_off = table_end;
        let need_px = pix_off
            .checked_add(stride.checked_mul(h as usize).ok_or("DIB too large")?)
            .ok_or("DIB too large")?;
        if dib.len() < need_px {
            return Err("DIB ends inside the pixel data".to_string());
        }
        palette = pal;
        pixels = pix_off;
    } else {
        if dib.len() < need {
            return Err("DIB ends inside the pixel data".to_string());
        }
        palette = Vec::new();
        pixels = pixel_off;
    }

    // Bitfield masks for 16/32-bit BITFIELDS (and the alpha channel).
    let (mask_r, mask_g, mask_b) = if matches!(compression, BI_BITFIELDS | BI_ALPHABITFIELDS)
        && header_size != 12
    {
        let off = if header_size >= 108 { 40 } else { header_size };
        (le_u32(dib, off)?, le_u32(dib, off + 4)?, le_u32(dib, off + 8)?)
    } else {
        (0, 0, 0)
    };

    let mut rgba = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for row in 0..h {
        // Bottom-up DIBs store the last row first.
        let src_row = if top_down { row } else { h - 1 - row };
        let row_off = pixels + (src_row as usize) * stride;
        match (bit_count, compression) {
            (32, BI_RGB) => {
                for x in 0..w {
                    let o = row_off + (x as usize) * 4;
                    let b = dib[o];
                    let g = dib[o + 1];
                    let r = dib[o + 2];
                    // Screenshots put 0x00 or 0xFF in the alpha byte; honor
                    // an explicit V5 alpha mask, otherwise keep it opaque.
                    let a = if alpha_mask != 0 {
                        extract_channel(u32::from_le_bytes([b, g, r, dib[o + 3]]), alpha_mask)
                    } else {
                        255
                    };
                    rgba.extend_from_slice(&[r, g, b, a]);
                }
            }
            (32, BI_BITFIELDS) | (32, BI_ALPHABITFIELDS) => {
                for x in 0..w {
                    let o = row_off + (x as usize) * 4;
                    let px = u32::from_le_bytes([dib[o], dib[o + 1], dib[o + 2], dib[o + 3]]);
                    let a = if alpha_mask != 0 {
                        extract_channel(px, alpha_mask)
                    } else {
                        255
                    };
                    rgba.extend_from_slice(&[
                        extract_channel(px, mask_r),
                        extract_channel(px, mask_g),
                        extract_channel(px, mask_b),
                        a,
                    ]);
                }
            }
            (24, _) => {
                for x in 0..w {
                    let o = row_off + (x as usize) * 3;
                    rgba.extend_from_slice(&[dib[o + 2], dib[o + 1], dib[o], 255]);
                }
            }
            (16, BI_RGB) => {
                for x in 0..w {
                    let o = row_off + (x as usize) * 2;
                    let px = u16::from_le_bytes([dib[o], dib[o + 1]]);
                    // 16-bit BI_RGB is xRRRRRGGGGGBBBBB.
                    let r = (((px >> 10) & 0x1F) * 255 / 31) as u8;
                    let g = (((px >> 5) & 0x1F) * 255 / 31) as u8;
                    let b = ((px & 0x1F) * 255 / 31) as u8;
                    rgba.extend_from_slice(&[r, g, b, 255]);
                }
            }
            (16, BI_BITFIELDS) | (16, BI_ALPHABITFIELDS) => {
                for x in 0..w {
                    let o = row_off + (x as usize) * 2;
                    let px = u16::from_le_bytes([dib[o], dib[o + 1]]) as u32;
                    rgba.extend_from_slice(&[
                        extract_channel(px, mask_r),
                        extract_channel(px, mask_g),
                        extract_channel(px, mask_b),
                        255,
                    ]);
                }
            }
            (8, _) | (4, _) | (1, _) => {
                for x in 0..w {
                    let idx = match bit_count {
                        8 => dib[row_off + x as usize] as usize,
                        4 => {
                            let byte = dib[row_off + (x as usize) / 2];
                            if x % 2 == 0 {
                                (byte >> 4) as usize
                            } else {
                                (byte & 0x0F) as usize
                            }
                        }
                        _ => {
                            let byte = dib[row_off + (x as usize) / 8];
                            ((byte >> (7 - (x % 8))) & 1) as usize
                        }
                    };
                    let c = palette.get(idx).copied().unwrap_or([0, 0, 0, 255]);
                    rgba.extend_from_slice(&c);
                }
            }
            _ => return Err("unreachable DIB flavor".to_string()),
        }
    }

    Ok(RgbaImage {
        width: w,
        height: h,
        rgba,
    })
}

/// Decode PNG bytes (the registered "PNG" clipboard format on Windows).
/// Used by the Windows reader; unit-tested on all platforms.
#[cfg(any(test, windows))]
fn decode_png(data: &[u8]) -> Result<RgbaImage, String> {
    let img = image::load_from_memory_with_format(data, image::ImageFormat::Png)
        .map_err(|e| format!("couldn't decode the PNG ({e})"))?;
    let rgba = img.to_rgba8();
    Ok(RgbaImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Decode BMP bytes (the image/bmp X11 target on Linux): a BMP file first,
/// falling back to a bare DIB for owners that serve headerless data.
/// Linux-only outside tests (Windows reads the DIB directly).
#[cfg(any(test, target_os = "linux"))]
fn decode_bmp(data: &[u8]) -> Result<RgbaImage, String> {
    match image::load_from_memory_with_format(data, image::ImageFormat::Bmp) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            Ok(RgbaImage {
                width: rgba.width(),
                height: rgba.height(),
                rgba: rgba.into_raw(),
            })
        }
        Err(_) => decode_dib(data).map_err(|e| format!("couldn't decode the BMP ({e})")),
    }
}

// ---------------------------------------------------------------------------
// Windows: read the DIB straight off the clipboard.
// ---------------------------------------------------------------------------

/// Read an image from the Windows clipboard without going through
/// arboard's narrow format list: CF_DIBV5 first, then plain CF_DIB (what
/// Win+Shift+S / the Snipping Tool place), then the registered "PNG"
/// format. The clipboard is briefly locked right after a screenshot, so
/// the open is retried a few times; a still-locked clipboard is an Err and
/// the watcher's next tick retries.
#[cfg(windows)]
pub fn read_windows_image() -> Result<RgbaImage, String> {
    use clipboard_win::{formats, raw, register_format, Clipboard};
    use std::time::Duration;

    // RAII open; closes on drop. Sleep between attempts (arboard's
    // reasoning: Sleep(0) can race open/close in single-threaded apps).
    let mut attempts = 5;
    let _open = loop {
        match Clipboard::new() {
            Ok(c) => break c,
            Err(_) if attempts > 0 => {
                attempts -= 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(format!("couldn't open the clipboard ({e})")),
        }
    };

    for (label, fmt) in [
        ("CF_DIBV5", formats::CF_DIBV5),
        ("CF_DIB", formats::CF_DIB),
    ] {
        if raw::is_format_avail(fmt) {
            let mut data = Vec::new();
            raw::get_vec(fmt, &mut data)
                .map_err(|e| format!("{label}: couldn't read the data ({e})"))?;
            return decode_dib(&data).map_err(|e| format!("{label}: {e}"));
        }
    }

    if let Some(png_format) = register_format("PNG") {
        let id: u32 = png_format.get();
        if raw::is_format_avail(id) {
            let mut data = Vec::new();
            raw::get_vec(id, &mut data)
                .map_err(|e| format!("PNG: couldn't read the data ({e})"))?;
            return decode_png(&data);
        }
    }

    Err("no image on the clipboard".to_string())
}

// ---------------------------------------------------------------------------
// Linux: image/bmp fallback, read straight from X11.
// ---------------------------------------------------------------------------

/// Read the image/bmp target from the X11 clipboard (CLIPBOARD selection).
/// arboard only ever requests image/png, so BMP-flavored owners are
/// invisible to it. Plain ICCCM transfer with INCR support; gives up after
/// ~2s so a hung selection owner can't wedge the watcher tick.
#[cfg(target_os = "linux")]
pub fn read_linux_image_bmp() -> Result<RgbaImage, String> {
    use std::time::{Duration, Instant};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::*;

    let (conn, screen_num) =
        x11rb::connect(None).map_err(|e| format!("couldn't reach X11 ({e})"))?;
    let root = conn.setup().roots[screen_num].root;

    // A 1x1 window to own the conversion property.
    let win = conn.generate_id().map_err(|e| format!("X11 ({e})"))?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| format!("X11 ({e})"))?
    .check()
    .map_err(|e| format!("X11 ({e})"))?;

    let clipboard = lookup_atom(&conn, b"CLIPBOARD")?;
    let target = lookup_atom(&conn, b"image/bmp")?;
    let property = lookup_atom(&conn, b"APPMAKA_IMAGE_BMP")?;
    let incr = lookup_atom(&conn, b"INCR")?;

    conn.convert_selection(win, clipboard, target, property, x11rb::CURRENT_TIME)
        .map_err(|e| format!("X11 ({e})"))?
        .check()
        .map_err(|e| format!("X11 ({e})"))?;
    conn.flush().map_err(|e| format!("X11 ({e})"))?;

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut data: Vec<u8> = Vec::new();
    let mut using_incr = false;
    let result: Result<Vec<u8>, String> = loop {
        if Instant::now() > deadline {
            break Err("timed out waiting for the image".to_string());
        }
        let event = match conn.poll_for_event().map_err(|e| format!("X11 ({e})"))? {
            Some(e) => e,
            None => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        match event {
            x11rb::protocol::Event::SelectionNotify(ev) => {
                if ev.property == 0 {
                    // The owner refused the conversion: no image/bmp.
                    break Err("image/bmp not available".to_string());
                }
                let reply = conn
                    .get_property(false, win, property, AtomEnum::ANY, 0, u32::MAX)
                    .map_err(|e| format!("X11 ({e})"))?
                    .reply()
                    .map_err(|e| format!("X11 ({e})"))?;
                if reply.type_ == incr {
                    using_incr = true;
                    conn.delete_property(win, property)
                        .map_err(|e| format!("X11 ({e})"))?
                        .check()
                        .map_err(|e| format!("X11 ({e})"))?;
                } else {
                    break Ok(reply.value);
                }
            }
            x11rb::protocol::Event::PropertyNotify(ev)
                if using_incr && ev.atom == property && ev.state == Property::NEW_VALUE =>
            {
                let reply = conn
                    .get_property(false, win, property, AtomEnum::ANY, 0, u32::MAX)
                    .map_err(|e| format!("X11 ({e})"))?
                    .reply()
                    .map_err(|e| format!("X11 ({e})"))?;
                if reply.value.is_empty() {
                    break Ok(std::mem::take(&mut data));
                }
                data.extend_from_slice(&reply.value);
                conn.delete_property(win, property)
                    .map_err(|e| format!("X11 ({e})"))?
                    .check()
                    .map_err(|e| format!("X11 ({e})"))?;
            }
            _ => {}
        }
    };
    let _ = conn.destroy_window(win);
    decode_bmp(&result?)
}

#[cfg(target_os = "linux")]
fn lookup_atom<Conn: x11rb::connection::Connection>(conn: &Conn, name: &[u8]) -> Result<u32, String> {
    x11rb::protocol::xproto::intern_atom(conn, false, name)
        .map_err(|e| format!("X11 ({e})"))?
        .reply()
        .map(|r| r.atom)
        .map_err(|e| format!("X11 ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w32(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }
    fn w16(v: u16) -> [u8; 2] {
        v.to_le_bytes()
    }

    /// Pixel (x, y): r=(x+y)*25, g=y*40, b=x*40 — distinct per pixel so a
    /// wrong orientation or channel swap fails loudly.
    fn px(x: u32, y: u32) -> [u8; 4] {
        [((x + y) * 25) as u8, (y * 40) as u8, (x * 40) as u8, 255]
    }

    /// 40-byte BITMAPINFOHEADER DIB, bottom-up unless top_down.
    fn dib_info(
        w: u32,
        h: u32,
        bit_count: u16,
        compression: u32,
        top_down: bool,
        body: &[u8],
        palette: &[u8],
    ) -> Vec<u8> {
        let mut dib = Vec::new();
        let height_field = if top_down { -(h as i32) } else { h as i32 };
        dib.extend_from_slice(&w32(40));
        dib.extend_from_slice(&w32(w));
        dib.extend_from_slice(&(height_field as u32).to_le_bytes());
        dib.extend_from_slice(&w16(1));
        dib.extend_from_slice(&w16(bit_count));
        dib.extend_from_slice(&w32(compression));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(palette);
        dib.extend_from_slice(body);
        dib
    }

    fn body_32(w: u32, h: u32, bottom_up: bool) -> Vec<u8> {
        let mut body = Vec::new();
        let rows: Vec<u32> = if bottom_up {
            (0..h).rev().collect()
        } else {
            (0..h).collect()
        };
        for y in rows {
            for x in 0..w {
                let p = px(x, y);
                body.extend_from_slice(&[p[2], p[1], p[0], 0xFF]); // BGRA
            }
        }
        body
    }

    fn expect_pixels(img: &RgbaImage, w: u32, h: u32) {
        assert_eq!((img.width, img.height), (w, h));
        assert_eq!(img.rgba.len(), (w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 4) as usize;
                assert_eq!(
                    &img.rgba[o..o + 4],
                    &px(x, y),
                    "pixel ({x},{y}) wrong — orientation or channel order"
                );
            }
        }
    }

    #[test]
    fn dib_32bit_bi_rgb_bottom_up() {
        // The Win+Shift+S shape: plain CF_DIB, 32-bit BI_RGB.
        let dib = dib_info(5, 5, 32, BI_RGB, false, &body_32(5, 5, true), &[]);
        expect_pixels(&decode_dib(&dib).expect("32-bit BI_RGB must decode"), 5, 5);
    }

    #[test]
    fn dib_32bit_top_down() {
        let dib = dib_info(5, 5, 32, BI_RGB, true, &body_32(5, 5, false), &[]);
        expect_pixels(&decode_dib(&dib).expect("top-down must decode"), 5, 5);
    }

    #[test]
    fn dib_v5_header_32bit() {
        // 124-byte BITMAPV5HEADER as Windows' CF_DIB->CF_DIBV5 synthesis
        // (or the Snipping Tool) produces: BI_RGB, alpha mask 0.
        let mut dib = Vec::new();
        dib.extend_from_slice(&w32(124));
        dib.extend_from_slice(&w32(5));
        dib.extend_from_slice(&w32(5));
        dib.extend_from_slice(&w16(1));
        dib.extend_from_slice(&w16(32));
        dib.extend_from_slice(&w32(BI_RGB));
        dib.extend_from_slice(&w32(100));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0x00FF0000));
        dib.extend_from_slice(&w32(0x0000FF00));
        dib.extend_from_slice(&w32(0x000000FF));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0x73524742));
        dib.extend_from_slice(&[0u8; 36]);
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(4));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        dib.extend_from_slice(&w32(0));
        assert_eq!(dib.len(), 124);
        dib.extend_from_slice(&body_32(5, 5, true));
        expect_pixels(&decode_dib(&dib).expect("V5 32-bit must decode"), 5, 5);
    }

    #[test]
    fn dib_24bit() {
        let mut body = Vec::new();
        for y in (0..4).rev() {
            for x in 0..6 {
                let p = px(x, y);
                body.extend_from_slice(&[p[2], p[1], p[0]]);
            }
            body.extend_from_slice(&[0u8; 2]); // stride 20 -> pad to 20? 6*3=18 -> pad 2
        }
        let dib = dib_info(6, 4, 24, BI_RGB, false, &body, &[]);
        expect_pixels(&decode_dib(&dib).expect("24-bit must decode"), 6, 4);
    }

    #[test]
    fn dib_16bit_555() {
        // 16-bit BI_RGB is xRRRRRGGGGGBBBBB.
        let mut body = Vec::new();
        for y in (0..3).rev() {
            for x in 0..4 {
                let p = px(x, y);
                let r5 = (p[0] as u16 * 31 / 255) & 0x1F;
                let g5 = (p[1] as u16 * 31 / 255) & 0x1F;
                let b5 = (p[2] as u16 * 31 / 255) & 0x1F;
                body.extend_from_slice(&((r5 << 10 | g5 << 5 | b5).to_le_bytes()));
            }
        }
        let dib = dib_info(4, 3, 16, BI_RGB, false, &body, &[]);
        let img = decode_dib(&dib).expect("16-bit 555 must decode");
        assert_eq!((img.width, img.height), (4, 3));
        // 5-bit quantization: allow off-by-a-few per channel.
        for y in 0..3 {
            for x in 0..4 {
                let o = ((y * 4 + x) * 4) as usize;
                let want = px(x, y);
                for (got, w) in img.rgba[o..o + 3].iter().zip(want[..3].iter()) {
                    assert!(
                        (*got as i16 - *w as i16).abs() <= 8,
                        "pixel ({x},{y}) channel off"
                    );
                }
            }
        }
    }

    #[test]
    fn dib_32bit_bitfields() {
        // BI_BITFIELDS with explicit masks after the 40-byte header.
        let mut dib = dib_info(5, 5, 32, BI_BITFIELDS, false, &[], &[]);
        let mask_off = 40;
        dib.splice(
            mask_off..mask_off,
            [w32(0x00FF0000), w32(0x0000FF00), w32(0x000000FF)].concat(),
        );
        dib.extend_from_slice(&body_32(5, 5, true));
        expect_pixels(&decode_dib(&dib).expect("BITFIELDS must decode"), 5, 5);
    }

    #[test]
    fn dib_8bit_paletted() {
        // 2x2, palette of red/green/blue/white; indices 0,1,2,3.
        let mut palette = Vec::new();
        for c in [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]] {
            palette.extend_from_slice(&[c[2], c[1], c[0], 0]);
        }
        let mut body = Vec::new();
        body.extend_from_slice(&[2, 3, 0, 0]); // bottom row first (stride 4)
        body.extend_from_slice(&[0, 1, 0, 0]);
        let mut dib = dib_info(2, 2, 8, BI_RGB, false, &body, &palette);
        dib[32..36].copy_from_slice(&4u32.to_le_bytes()); // biClrUsed = 4
        let img = decode_dib(&dib).expect("8-bit paletted must decode");
        assert_eq!((img.width, img.height), (2, 2));
        // Top row: indices 0,1 -> red, green. Bottom row: 2,3 -> blue, white.
        assert_eq!(&img.rgba[0..8], &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(&img.rgba[8..16], &[0, 0, 255, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn dib_core_header_24bit() {
        // 12-byte BITMAPCOREHEADER with RGBTRIPLE palette entries (none
        // needed for 24-bit).
        let mut dib = Vec::new();
        dib.extend_from_slice(&w32(12));
        dib.extend_from_slice(&w16(3));
        dib.extend_from_slice(&w16(2));
        dib.extend_from_slice(&w16(1));
        dib.extend_from_slice(&w16(24));
        for y in (0..2).rev() {
            for x in 0..3 {
                let p = px(x, y);
                dib.extend_from_slice(&[p[2], p[1], p[0]]);
            }
            dib.extend_from_slice(&[0u8; 3]); // 3*3=9 -> stride 12
        }
        expect_pixels(&decode_dib(&dib).expect("core header must decode"), 3, 2);
    }

    #[test]
    fn dib_screenshot_dimensions() {
        // Real screenshot size: dims + length only, keeps the test fast.
        let dib = dib_info(1920, 1080, 32, BI_RGB, false, &vec![0u8; 1920 * 1080 * 4], &[]);
        let img = decode_dib(&dib).expect("1080p DIB must decode");
        assert_eq!((img.width, img.height), (1920, 1080));
        assert_eq!(img.rgba.len(), 1920 * 1080 * 4);
    }

    #[test]
    fn png_format_decodes() {
        // The registered "PNG" clipboard format path on Windows: encode a
        // 2x2 PNG with the png crate, decode it back.
        let mut buf = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut buf, 2, 2);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 255, 255, 255, 255,
            ])
            .unwrap();
        }
        let img = decode_png(&buf).expect("PNG must decode");
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(
            img.rgba,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 255, 255, 255, 255,
            ]
        );
    }

    /// End-to-end of the Linux image/bmp fallback: serve a BMP on the X11
    /// clipboard with xclip, read it back through read_linux_image_bmp.
    /// Skips itself where there is no X display or xclip (e.g. CI).
    #[cfg(target_os = "linux")]
    #[test]
    fn x11_bmp_roundtrip_via_xclip() {
        use std::process::Command;
        use std::time::Duration;

        if std::env::var("DISPLAY").is_err() {
            eprintln!("skip x11_bmp_roundtrip_via_xclip: no DISPLAY");
            return;
        }
        if Command::new("xclip")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("skip x11_bmp_roundtrip_via_xclip: no xclip");
            return;
        }

        let (w, h) = (4u32, 4u32);
        let mut pixels = Vec::new();
        for y in (0..h).rev() {
            for x in 0..w {
                // BMP is BGR, bottom-up.
                pixels.extend_from_slice(&[
                    ((x + y) * 32 % 256) as u8,
                    ((y * 64) % 256) as u8,
                    ((x * 64) % 256) as u8,
                ]);
            }
        }
        let mut bmp = Vec::new();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(14u32 + 40 + pixels.len() as u32).to_le_bytes());
        bmp.extend_from_slice(&[0u8; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&w.to_le_bytes());
        bmp.extend_from_slice(&h.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        bmp.extend_from_slice(&[0u8; 16]);
        bmp.extend_from_slice(&pixels);

        let path = std::env::temp_dir().join("appmaka-bmp-test.bmp");
        std::fs::write(&path, &bmp).unwrap();
        // xclip forks and holds the selection until another client takes it.
        let _ = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "image/bmp", "-i"])
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .spawn();
        std::thread::sleep(Duration::from_millis(500));

        let result = read_linux_image_bmp();

        // Take the selection back so the xclip daemon exits; tidy up.
        let _ = Command::new("xclip")
            .args(["-selection", "clipboard", "-i", "/dev/null"])
            .stdin(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_file(&path);

        let img = result.expect("must read image/bmp from the X11 clipboard");
        assert_eq!((img.width, img.height), (w, h));
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 4) as usize;
                assert_eq!(
                    (img.rgba[o], img.rgba[o + 1], img.rgba[o + 2]),
                    (
                        ((x * 64) % 256) as u8,
                        ((y * 64) % 256) as u8,
                        ((x + y) * 32 % 256) as u8
                    ),
                    "pixel ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn bmp_file_and_bare_dib_fallback() {
        // image/bmp owners serve a BMP file (with the 14-byte file header);
        // some serve a bare DIB. Both must decode.
        let dib = dib_info(3, 2, 24, BI_RGB, false, &{
            let mut body = Vec::new();
            for y in (0..2).rev() {
                for x in 0..3 {
                    let p = px(x, y);
                    body.extend_from_slice(&[p[2], p[1], p[0]]);
                }
                body.extend_from_slice(&[0u8; 3]); // 3*3=9 -> stride 12
            }
            body
        }, &[]);
        // Bare DIB: the fallback path.
        expect_pixels(&decode_bmp(&dib).expect("bare DIB must decode"), 3, 2);
        // BMP file: BITMAPFILEHEADER + the same DIB.
        let mut file = Vec::new();
        file.extend_from_slice(b"BM");
        file.extend_from_slice(&(14u32 + dib.len() as u32).to_le_bytes());
        file.extend_from_slice(&[0u8; 4]);
        file.extend_from_slice(&54u32.to_le_bytes()); // pixel offset
        file.extend_from_slice(&dib);
        expect_pixels(&decode_bmp(&file).expect("BMP file must decode"), 3, 2);
    }

    #[test]
    fn dib_garbage_is_err_not_panic() {
        assert!(decode_dib(&[]).is_err());
        assert!(decode_dib(&[1, 2, 3]).is_err());
        assert!(decode_dib(&[99, 0, 0, 0, 1, 2, 3]).is_err()); // bad header size
        // Truncated pixel data.
        let mut dib = dib_info(5, 5, 32, BI_RGB, false, &[0u8; 10], &[]);
        assert!(decode_dib(&dib).is_err());
        // Zero width.
        dib = dib_info(5, 5, 32, BI_RGB, false, &body_32(5, 5, true), &[]);
        dib[4] = 0;
        assert!(decode_dib(&dib).is_err());
    }
}
