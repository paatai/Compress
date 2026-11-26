//! PNG and BMP reading/writing. Pixels are decoded exactly (no colour management,
//! no premultiplication): palette and low-bit-depth PNGs are expanded to 8-bit
//! gray/RGB/RGBA, 16-bit PNGs stay 16-bit.

use crate::Image;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFormat {
    Png = 0,
    Bmp = 1,
    Other = 2,
}

impl SourceFormat {
    pub fn from_u8(v: u8) -> SourceFormat {
        match v {
            0 => SourceFormat::Png,
            1 => SourceFormat::Bmp,
            _ => SourceFormat::Other,
        }
    }
}

pub fn read_image(bytes: &[u8]) -> Result<(Image, SourceFormat), String> {
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        read_png(bytes).map(|i| (i, SourceFormat::Png))
    } else if bytes.len() >= 2 && &bytes[..2] == b"BM" {
        read_bmp(bytes).map(|i| (i, SourceFormat::Bmp))
    } else if bytes.len() >= 2 && bytes[0] == b'P' && (bytes[1] == b'5' || bytes[1] == b'6') {
        read_pnm(bytes).map(|i| (i, SourceFormat::Other))
    } else {
        Err("unsupported input: expected a PNG or BMP file".into())
    }
}

pub fn read_png(bytes: &[u8]) -> Result<Image, String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND);
    dec.ignore_checksums(false);
    let mut reader = dec.read_info().map_err(|e| format!("PNG: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size().ok_or("PNG: image too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("PNG: {e}"))?;
    let channels: u8 = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("PNG: unexpanded palette".into()),
    };
    let depth: u8 = match info.bit_depth {
        png::BitDepth::Sixteen => 16,
        png::BitDepth::Eight => 8,
        _ => return Err("PNG: unexpected bit depth after expansion".into()),
    };
    let (w, h) = (info.width as usize, info.height as usize);
    let n = w * h * channels as usize;
    let mut data = Vec::with_capacity(n);
    let line = info.line_size;
    for y in 0..h {
        let row = &buf[y * line..y * line + w * channels as usize * (depth as usize / 8)];
        if depth == 16 {
            for c in row.chunks_exact(2) {
                data.push(u16::from_be_bytes([c[0], c[1]]));
            }
        } else {
            data.extend(row.iter().map(|&b| b as u16));
        }
    }
    Ok(Image { width: info.width, height: info.height, channels, depth, data })
}

pub fn write_png(img: &Image) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(match img.channels {
            1 => png::ColorType::Grayscale,
            2 => png::ColorType::GrayscaleAlpha,
            3 => png::ColorType::Rgb,
            _ => png::ColorType::Rgba,
        });
        enc.set_depth(if img.depth == 16 { png::BitDepth::Sixteen } else { png::BitDepth::Eight });
        enc.set_compression(png::Compression::High);
        let mut wr = enc.write_header().map_err(|e| format!("PNG: {e}"))?;
        let bytes: Vec<u8> = if img.depth == 16 {
            img.data.iter().flat_map(|v| v.to_be_bytes()).collect()
        } else {
            img.data.iter().map(|&v| v as u8).collect()
        };
        wr.write_image_data(&bytes).map_err(|e| format!("PNG: {e}"))?;
    }
    Ok(out)
}

fn le16(b: &[u8], o: usize) -> u32 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u32
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub fn read_bmp(b: &[u8]) -> Result<Image, String> {
    let err = || "BMP: truncated or unsupported file".to_string();
    if b.len() < 26 {
        return Err(err());
    }
    let off = le32(b, 10) as usize;
    let hsize = le32(b, 14) as usize;
    if hsize < 40 || b.len() < 14 + hsize.min(124) {
        return Err("BMP: only Windows BMP (BITMAPINFOHEADER or newer) is supported".into());
    }
    let w = le32(b, 18) as i32;
    let hraw = le32(b, 22) as i32;
    let planes = le16(b, 26);
    let bpp = le16(b, 28);
    let comp = le32(b, 30);
    if w <= 0 || hraw == 0 || planes != 1 {
        return Err(err());
    }
    let top_down = hraw < 0;
    let h = hraw.unsigned_abs() as usize;
    let w = w as usize;
    let mut masks = None;
    if comp == 3 || comp == 6 {
        if hsize >= 52 {
            masks = Some([le32(b, 54), le32(b, 58), le32(b, 62), if hsize >= 56 { le32(b, 66) } else { 0 }]);
        } else if b.len() >= 14 + hsize + 12 {
            let o = 14 + hsize;
            masks = Some([le32(b, o), le32(b, o + 4), le32(b, o + 8), if comp == 6 { le32(b, o + 12) } else { 0 }]);
        }
    } else if comp != 0 {
        return Err("BMP: compressed BMP files are not supported".into());
    }
    let row_bytes = (w * bpp as usize).div_ceil(32) * 4;
    if b.len() < off + row_bytes * (h - 1) + (w * bpp as usize).div_ceil(8) {
        return Err(err());
    }
    let row_at = |y: usize| {
        let sy = if top_down { y } else { h - 1 - y };
        &b[off + sy * row_bytes..]
    };
    match bpp {
        24 => {
            let mut data = Vec::with_capacity(w * h * 3);
            for y in 0..h {
                let r = row_at(y);
                for x in 0..w {
                    data.extend_from_slice(&[r[x * 3 + 2] as u16, r[x * 3 + 1] as u16, r[x * 3] as u16]);
                }
            }
            Ok(Image { width: w as u32, height: h as u32, channels: 3, depth: 8, data })
        }
        32 => {
            let m = masks.unwrap_or([0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000]);
            let ext = |v: u32, mask: u32| -> u16 {
                if mask == 0 {
                    return 0;
                }
                let sh = mask.trailing_zeros();
                let bits = (mask >> sh).count_ones();
                let x = (v & mask) >> sh;
                if bits >= 8 {
                    (x >> (bits - 8)) as u16
                } else {
                    ((x * 255 + ((1 << bits) - 1) / 2) / ((1 << bits) - 1)) as u16
                }
            };
            let mut data = Vec::with_capacity(w * h * 4);
            let mut any_alpha = false;
            for y in 0..h {
                let r = row_at(y);
                for x in 0..w {
                    let v = le32(r, x * 4);
                    let a = ext(v, m[3]);
                    if a != 0 {
                        any_alpha = true;
                    }
                    data.extend_from_slice(&[ext(v, m[0]), ext(v, m[1]), ext(v, m[2]), a]);
                }
            }
            if !any_alpha || m[3] == 0 {
                let rgb: Vec<u16> = data.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
                return Ok(Image { width: w as u32, height: h as u32, channels: 3, depth: 8, data: rgb });
            }
            Ok(Image { width: w as u32, height: h as u32, channels: 4, depth: 8, data })
        }
        1 | 4 | 8 => {
            let ncol = {
                let c = le32(b, 46) as usize;
                if c == 0 {
                    1usize << bpp
                } else {
                    c.min(256)
                }
            };
            let pal_off = 14 + hsize;
            if b.len() < pal_off + ncol * 4 {
                return Err(err());
            }
            let pal: Vec<[u16; 3]> = (0..ncol)
                .map(|i| {
                    let o = pal_off + i * 4;
                    [b[o + 2] as u16, b[o + 1] as u16, b[o] as u16]
                })
                .collect();
            let gray = pal.iter().all(|c| c[0] == c[1] && c[1] == c[2]);
            let mut data = Vec::with_capacity(w * h * if gray { 1 } else { 3 });
            for y in 0..h {
                let r = row_at(y);
                for x in 0..w {
                    let idx = match bpp {
                        8 => r[x] as usize,
                        4 => ((r[x / 2] >> (4 * (1 - (x & 1)))) & 15) as usize,
                        _ => ((r[x / 8] >> (7 - (x & 7))) & 1) as usize,
                    };
                    let c = pal.get(idx).copied().unwrap_or([0, 0, 0]);
                    if gray {
                        data.push(c[0]);
                    } else {
                        data.extend_from_slice(&c);
                    }
                }
            }
            Ok(Image { width: w as u32, height: h as u32, channels: if gray { 1 } else { 3 }, depth: 8, data })
        }
        _ => Err(format!("BMP: {bpp}-bit BMP files are not supported")),
    }
}

pub fn write_bmp(img: &Image) -> Result<Vec<u8>, String> {
    if img.depth != 8 {
        return Err("BMP output supports 8-bit images only; use PNG for 16-bit".into());
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let nc = img.channels as usize;
    let (bpp, hsize, ncol) = match nc {
        1 => (8usize, 40usize, 256usize),
        3 => (24, 40, 0),
        _ => (32, 108, 0),
    };
    let row_bytes = (w * bpp).div_ceil(32) * 4;
    let off = 14 + hsize + ncol * 4;
    let total = off + row_bytes * h;
    let mut o = Vec::with_capacity(total);
    o.extend_from_slice(b"BM");
    o.extend_from_slice(&(total as u32).to_le_bytes());
    o.extend_from_slice(&0u32.to_le_bytes());
    o.extend_from_slice(&(off as u32).to_le_bytes());
    o.extend_from_slice(&(hsize as u32).to_le_bytes());
    o.extend_from_slice(&(w as i32).to_le_bytes());
    o.extend_from_slice(&(h as i32).to_le_bytes());
    o.extend_from_slice(&1u16.to_le_bytes());
    o.extend_from_slice(&(bpp as u16).to_le_bytes());
    o.extend_from_slice(&(if nc == 4 { 3u32 } else { 0u32 }).to_le_bytes());
    o.extend_from_slice(&((row_bytes * h) as u32).to_le_bytes());
    o.extend_from_slice(&2835u32.to_le_bytes());
    o.extend_from_slice(&2835u32.to_le_bytes());
    o.extend_from_slice(&(ncol as u32).to_le_bytes());
    o.extend_from_slice(&0u32.to_le_bytes());
    if nc == 4 {
        for m in [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000] {
            o.extend_from_slice(&m.to_le_bytes());
        }
        o.extend_from_slice(b"BGRs");
        o.extend_from_slice(&[0u8; 48]);
    }
    for i in 0..ncol {
        o.extend_from_slice(&[i as u8, i as u8, i as u8, 0]);
    }
    for y in (0..h).rev() {
        let row = &img.data[y * w * nc..(y + 1) * w * nc];
        let start = o.len();
        for x in 0..w {
            let p = &row[x * nc..x * nc + nc];
            match nc {
                1 => o.push(p[0] as u8),
                2 => o.push(p[0] as u8),
                3 => o.extend_from_slice(&[p[2] as u8, p[1] as u8, p[0] as u8]),
                _ => o.extend_from_slice(&[p[2] as u8, p[1] as u8, p[0] as u8, p[3] as u8]),
            }
        }
        while o.len() - start < row_bytes {
            o.push(0);
        }
    }
    if nc == 2 {
        return Err("BMP output does not support gray+alpha; use PNG".into());
    }
    Ok(o)
}

fn read_pnm(b: &[u8]) -> Result<Image, String> {
    let mut pos = 2;
    let mut fields = [0usize; 3];
    for f in fields.iter_mut() {
        loop {
            while pos < b.len() && b[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos < b.len() && b[pos] == b'#' {
                while pos < b.len() && b[pos] != b'\n' {
                    pos += 1;
                }
            } else {
                break;
            }
        }
        let st = pos;
        while pos < b.len() && b[pos].is_ascii_digit() {
            pos += 1;
        }
        *f = std::str::from_utf8(&b[st..pos]).ok().and_then(|s| s.parse().ok()).ok_or("PNM: bad header")?;
    }
    pos += 1;
    let (w, h, maxv) = (fields[0], fields[1], fields[2]);
    let nc = if b[1] == b'5' { 1 } else { 3 };
    let depth = if maxv > 255 { 16 } else { 8 };
    let n = w * h * nc;
    let bps = depth / 8;
    if b.len() < pos + n * bps {
        return Err("PNM: truncated".into());
    }
    let data = if depth == 16 {
        (0..n).map(|i| u16::from_be_bytes([b[pos + 2 * i], b[pos + 2 * i + 1]])).collect()
    } else {
        b[pos..pos + n].iter().map(|&v| v as u16).collect()
    };
    Ok(Image { width: w as u32, height: h as u32, channels: nc as u8, depth: depth as u8, data })
}
