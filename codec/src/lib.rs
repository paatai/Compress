//! Kolmo: lossless image compression by context mixing.
//!
//! File layout (multi-byte integers little-endian):
//!   fixed header:  "KLMO" | version | layout | varint width | varint height | reductions | mode | crc32
//!                  [palette mode: varint length | arithmetic-coded palette]
//!   stripe table:  varint stripe count | varint byte length of every stripe but the last
//!   stripes:       independently coded horizontal bands (decodable in parallel)
//!
//! layout = channels (1-4) | 8 if 16-bit | source format << 4. The CRC-32 covers the original
//! samples and is verified after decoding.

pub mod coder;
pub mod ct;
pub mod detmath;
pub mod io;
pub mod ols;
pub mod pal;
pub mod pix;
pub mod primitives;
pub mod wls;

pub use io::SourceFormat;

pub const MAGIC: &[u8; 4] = b"KLMO";
pub const VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// 1 = gray, 2 = gray+alpha, 3 = RGB, 4 = RGBA (interleaved)
    pub channels: u8,
    /// 8 or 16
    pub depth: u8,
    pub data: Vec<u16>,
}

impl Image {
    pub fn pixels(&self) -> usize {
        self.width as usize * self.height as usize
    }
    pub fn maxv(&self) -> u16 {
        if self.depth == 16 {
            65535
        } else {
            255
        }
    }
    /// Raw sample bytes (8-bit: one byte per sample, 16-bit: big-endian), as hashed by the checksum.
    pub fn sample_bytes(&self) -> Vec<u8> {
        if self.depth == 16 {
            self.data.iter().flat_map(|v| v.to_be_bytes()).collect()
        } else {
            self.data.iter().map(|&v| v as u8).collect()
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub tuning: ct::Tuning,
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varint(b: &[u8], pos: &mut usize) -> Result<u32, String> {
    let mut v = 0u32;
    for shift in (0..35).step_by(7) {
        let byte = *b.get(*pos).ok_or("truncated header")?;
        *pos += 1;
        v |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("bad varint".into())
}

const RED_OPAQUE: u8 = 1;
const RED_GRAY: u8 = 2;

/// Removes redundant channels: constant-opaque alpha, and R=G=B colour.
fn reduce(img: &Image) -> (Image, u8) {
    let nc = img.channels as usize;
    let maxv = img.maxv();
    let has_alpha = nc == 2 || nc == 4;
    let color = nc >= 3;
    let opaque = has_alpha && img.data.chunks_exact(nc).all(|p| p[nc - 1] == maxv);
    let gray = color && img.data.chunks_exact(nc).all(|p| p[0] == p[1] && p[1] == p[2]);
    let mut red = 0;
    if opaque {
        red |= RED_OPAQUE;
    }
    if gray {
        red |= RED_GRAY;
    }
    if red == 0 {
        return (img.clone(), 0);
    }
    let keep_alpha = has_alpha && !opaque;
    let base = if color && !gray { 3 } else { 1 };
    let onc = base + keep_alpha as usize;
    let mut data = Vec::with_capacity(img.pixels() * onc);
    for p in img.data.chunks_exact(nc) {
        data.extend_from_slice(&p[..base]);
        if keep_alpha {
            data.push(p[nc - 1]);
        }
    }
    (Image { width: img.width, height: img.height, channels: onc as u8, depth: img.depth, data }, red)
}

fn expand(img: Image, red: u8, target_channels: u8) -> Image {
    if red == 0 {
        return img;
    }
    let nc = img.channels as usize;
    let tnc = target_channels as usize;
    let maxv = img.maxv();
    let mut data = Vec::with_capacity(img.pixels() * tnc);
    let src_alpha = nc == 2 || nc == 4;
    for p in img.data.chunks_exact(nc) {
        let a = if src_alpha { p[nc - 1] } else { maxv };
        match tnc {
            1 => data.push(p[0]),
            2 => data.extend_from_slice(&[p[0], a]),
            3 => {
                if nc >= 3 {
                    data.extend_from_slice(&p[..3])
                } else {
                    data.extend_from_slice(&[p[0], p[0], p[0]])
                }
            }
            _ => {
                if nc >= 3 {
                    data.extend_from_slice(&[p[0], p[1], p[2], a])
                } else {
                    data.extend_from_slice(&[p[0], p[0], p[0], a])
                }
            }
        }
    }
    Image { width: img.width, height: img.height, channels: target_channels, depth: img.depth, data }
}


pub const MODE_PALETTE: u8 = 1;
pub const MODE_PIXFLAGS: u8 = 4;
/// Images are split into independent stripes of about this many pixels so that large
/// photos can be coded on several cores; images up to this size use a single stripe.
pub const STRIPE_PIXELS: usize = 1 << 22;

#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub depth: u8,
    pub source: SourceFormat,
    pub reductions: u8,
    pub mode: u8,
    pub crc: u32,
    pub coded_channels: u8,
    pub palette: Option<Vec<u64>>,
}

impl StreamInfo {
    pub fn stripes(&self) -> Vec<(usize, usize)> {
        stripe_rows(self.width as usize, self.height as usize)
    }
}

pub fn stripe_rows(w: usize, h: usize) -> Vec<(usize, usize)> {
    let n = (w * h).div_ceil(STRIPE_PIXELS).clamp(1, h.clamp(1, 64));
    let rows = h.div_ceil(n);
    (0..n).map(|k| (k * rows, ((k + 1) * rows).min(h))).filter(|r| r.0 < r.1).collect()
}

fn coded_channels(channels: u8, reductions: u8) -> u8 {
    let nc = channels as usize;
    let has_alpha = nc == 2 || nc == 4;
    let keep_alpha = has_alpha && reductions & RED_OPAQUE == 0;
    let base = if nc >= 3 && reductions & RED_GRAY == 0 { 3 } else { 1 };
    (base + keep_alpha as usize) as u8
}

fn pack(p: &[u16]) -> u64 {
    let mut k = 0u64;
    for (i, &v) in p.iter().enumerate() {
        k |= (v as u64) << (16 * i);
    }
    k
}

/// Chooses the coding mode from simple image statistics.
fn choose_mode(img: &Image) -> (u8, Option<Vec<u64>>) {
    let nc = img.channels as usize;
    let w = img.width as usize;
    let mut colors: Vec<u64> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut eq = 0usize;
    for (i, p) in img.data.chunks_exact(nc).enumerate() {
        let k = pack(p);
        if seen.len() <= 256 && seen.insert(k) {
            colors.push(k);
        }
        let x = i % w;
        if (x > 0 && img.data[(i - 1) * nc..i * nc] == *p) || (i >= w && img.data[(i - w) * nc..(i - w + 1) * nc] == *p) {
            eq += 1;
        }
    }
    let eq_rate = eq as f64 / img.pixels() as f64;
    if colors.len() <= 256 {
        sort_palette(&mut colors, nc);
        return (MODE_PALETTE | MODE_PIXFLAGS, Some(colors));
    }
    if eq_rate > 0.25 {
        return (MODE_PIXFLAGS, None);
    }
    (0, None)
}

/// Palette order: by luminance, then by value, so that similar colours get nearby indices.
fn sort_palette(colors: &mut [u64], nc: usize) {
    let luma = |k: u64| -> u64 {
        let c = |i: usize| (k >> (16 * i)) & 0xFFFF;
        match nc {
            1 | 2 => c(0) * 4,
            _ => c(0) + 2 * c(1) + c(2),
        }
    };
    colors.sort_by_key(|&k| (luma(k), k));
}

fn code_palette<C: coder::BitCoder>(c: &mut C, pal: &mut Vec<u64>, nc: usize, depth: u8, enc: bool) {
    let t = primitives::tables();
    let n = c.code_raw(pal.len().saturating_sub(1) as u32, 8) as usize + 1;
    let bits = depth as u32;
    let mut ctr = vec![primitives::CTR_INIT; 4 * (1 << 17)];
    let mut prev = [0u32; 4];
    if !enc {
        pal.clear();
    }
    for i in 0..n {
        let mut key = 0u64;
        for ch in 0..nc {
            let v = if enc { ((pal[i] >> (16 * ch)) & 0xFFFF) as u32 } else { 0 };
            let d = v.wrapping_sub(prev[ch]) & ((1 << bits) - 1);
            let mut node = 1u32;
            for b in (0..bits).rev() {
                let slot = (ch << 17) | (node as usize & 0x1FFFF);
                let p = primitives::clamp_p(primitives::ctr_p16(ctr[slot]));
                let bit = c.code((d >> b) & 1, p);
                primitives::ctr_update(&mut ctr[slot], bit, 30, &t.recip);
                node = (node << 1) | bit;
            }
            let dv = node & ((1 << bits) - 1);
            let v = (prev[ch] + dv) & ((1 << bits) - 1);
            prev[ch] = v;
            key |= (v as u64) << (16 * ch);
        }
        if !enc {
            pal.push(key);
        }
    }
}

/// An image ready to be coded stripe by stripe.
pub struct Prepared {
    pub info: StreamInfo,
    /// Fixed header bytes (everything before the stripe table).
    pub header: Vec<u8>,
    /// Samples after channel reductions, interleaved, row-major.
    pub coded: Image,
}

pub fn prepare(img: &Image, src: SourceFormat) -> Result<Prepared, String> {
    if img.width == 0 || img.height == 0 {
        return Err("empty image".into());
    }
    if !(1..=4).contains(&img.channels) || !(img.depth == 8 || img.depth == 16) {
        return Err("unsupported pixel format".into());
    }
    if img.data.len() != img.pixels() * img.channels as usize {
        return Err("pixel buffer size mismatch".into());
    }
    let crc = crc32(&img.sample_bytes());
    let (coded, red) = reduce(img);
    let (mode, palette) = choose_mode(&coded);
    let mut header = Vec::new();
    header.extend_from_slice(MAGIC);
    header.push(VERSION);
    header.push(img.channels | if img.depth == 16 { 8 } else { 0 } | ((src as u8) << 4));
    put_varint(&mut header, img.width);
    put_varint(&mut header, img.height);
    header.push(red);
    header.push(mode);
    header.extend_from_slice(&crc.to_le_bytes());
    if let Some(pal) = &palette {
        let mut e = coder::Encoder::new();
        let mut p = pal.clone();
        code_palette(&mut e, &mut p, coded.channels as usize, img.depth, true);
        let block = e.finish();
        put_varint(&mut header, block.len() as u32);
        header.extend_from_slice(&block);
    }
    let info = StreamInfo {
        width: img.width,
        height: img.height,
        channels: img.channels,
        depth: img.depth,
        source: src,
        reductions: red,
        mode,
        crc,
        coded_channels: coded.channels,
        palette,
    };
    Ok(Prepared { info, header, coded })
}

/// Parses the fixed header. Returns the stream description and the header length.
pub fn parse_header(b: &[u8]) -> Result<(StreamInfo, usize), String> {
    if b.len() < 6 || &b[..4] != MAGIC {
        return Err("not a Kolmo file".into());
    }
    if b[4] != VERSION {
        return Err(format!("unsupported Kolmo version {}", b[4]));
    }
    let layout = b[5];
    let mut pos = 6;
    let width = get_varint(b, &mut pos)?;
    let height = get_varint(b, &mut pos)?;
    if b.len() < pos + 6 {
        return Err("truncated header".into());
    }
    let reductions = b[pos];
    let mode = b[pos + 1];
    let crc = u32::from_le_bytes([b[pos + 2], b[pos + 3], b[pos + 4], b[pos + 5]]);
    pos += 6;
    let channels = layout & 7;
    if !(1..=4).contains(&channels) || width == 0 || height == 0 || (width as u64 * height as u64) > (1 << 31) {
        return Err("corrupt header".into());
    }
    let depth = if layout & 8 != 0 { 16 } else { 8 };
    let cnc = coded_channels(channels, reductions);
    let palette = if mode & MODE_PALETTE != 0 {
        let len = get_varint(b, &mut pos)? as usize;
        let block = b.get(pos..pos + len).ok_or("truncated palette")?;
        pos += len;
        let mut d = coder::Decoder::new(block);
        let mut pal = Vec::new();
        code_palette(&mut d, &mut pal, cnc as usize, depth, false);
        Some(pal)
    } else {
        None
    };
    let info = StreamInfo {
        width,
        height,
        channels,
        depth,
        source: SourceFormat::from_u8(layout >> 4),
        reductions,
        mode,
        crc,
        coded_channels: cnc,
        palette,
    };
    Ok((info, pos))
}

/// Splits a complete file into its stripe streams.
pub fn split_stripes<'a>(b: &'a [u8], info: &StreamInfo, header_len: usize) -> Result<Vec<&'a [u8]>, String> {
    let mut pos = header_len;
    let n = get_varint(b, &mut pos)? as usize;
    if n != info.stripes().len() {
        return Err("corrupt stripe table".into());
    }
    let mut lens = Vec::with_capacity(n);
    for _ in 0..n.saturating_sub(1) {
        lens.push(get_varint(b, &mut pos)? as usize);
    }
    let mut out = Vec::with_capacity(n);
    for l in lens {
        out.push(b.get(pos..pos + l).ok_or("truncated stripe")?);
        pos += l;
    }
    out.push(b.get(pos..).ok_or("truncated stripe")?);
    Ok(out)
}

/// Joins the fixed header and the coded stripes into a file.
pub fn assemble(header: &[u8], stripes: &[Vec<u8>]) -> Vec<u8> {
    let mut out = header.to_vec();
    put_varint(&mut out, stripes.len() as u32);
    for s in &stripes[..stripes.len().saturating_sub(1)] {
        put_varint(&mut out, s.len() as u32);
    }
    for s in stripes {
        out.extend_from_slice(s);
    }
    out
}

/// Codes one stripe. `rows` holds the coded samples of exactly this stripe's rows.
pub fn encode_stripe(info: &StreamInfo, rows: &[u16], stripe_height: usize, tune: &ct::Tuning, progress: &mut dyn FnMut(f32)) -> Vec<u8> {
    let (w, h, nc) = (info.width as usize, stripe_height, info.coded_channels as usize);
    let mut enc = coder::Encoder::new();
    let mut pix = pix::PixModel::new(w, h);
    if let Some(pal) = &info.palette {
        let map: std::collections::HashMap<u64, u32> = pal.iter().enumerate().map(|(i, &k)| (k, i as u32)).collect();
        let mut pm = pal::PalModel::new(w, h, pal.len());
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let idx = map[&pack(&rows[i * nc..(i + 1) * nc])];
                if pix.code(&mut enc, x, y, idx as u64).is_none() {
                    let eqp = pix.eqp();
                    pm.code(&mut enc, &pix.keys, x, y, idx, eqp);
                }
                pix.finish(x, y, idx as u64);
            }
            if y % 16 == 15 {
                progress((y + 1) as f32 / h as f32);
            }
        }
        progress(1.0);
        return enc.finish();
    }
    let flags = info.mode & MODE_PIXFLAGS != 0;
    let plan = ct::Plan::for_channels(nc);
    let mut model = ct::CtModel::new(w, h, info.depth as u32, plan.clone(), tune.clone());
    for y in 0..h {
        model.start_row(y);
        let row = &rows[y * w * nc..(y + 1) * w * nc];
        for x in 0..w {
            let p = &row[x * nc..(x + 1) * nc];
            let key = pack(p);
            let known = flags && pix.code(&mut enc, x, y, key).is_some();
            for ci in 0..nc {
                model.code_sample(&mut enc, x, ci, p[plan.src[ci]] as i32, known);
            }
            if flags {
                pix.finish(x, y, key);
            }
        }
        model.end_row();
        if y % 16 == 15 {
            progress((y + 1) as f32 / h as f32);
        }
    }
    progress(1.0);
    if let Some(s) = model.stats.as_ref() {
        eprintln!("{}", s.report());
    }
    enc.finish()
}

/// Decodes one stripe into its coded samples.
pub fn decode_stripe(info: &StreamInfo, data: &[u8], stripe_height: usize, tune: &ct::Tuning, progress: &mut dyn FnMut(f32)) -> Vec<u16> {
    let (w, h, nc) = (info.width as usize, stripe_height, info.coded_channels as usize);
    let mut dec = coder::Decoder::new(data);
    let mut out = vec![0u16; w * h * nc];
    let mut pix = pix::PixModel::new(w, h);
    if let Some(pal) = &info.palette {
        let mut pm = pal::PalModel::new(w, h, pal.len());
        for y in 0..h {
            for x in 0..w {
                let idx = match pix.code(&mut dec, x, y, 0) {
                    Some(k) => k as u32,
                    None => {
                        let eqp = pix.eqp();
                        pm.code(&mut dec, &pix.keys, x, y, 0, eqp)
                    }
                };
                pix.finish(x, y, idx as u64);
                let key = pal.get(idx as usize).copied().unwrap_or(0);
                let i = y * w + x;
                for ch in 0..nc {
                    out[i * nc + ch] = ((key >> (16 * ch)) & 0xFFFF) as u16;
                }
            }
            if y % 16 == 15 {
                progress((y + 1) as f32 / h as f32);
            }
        }
        progress(1.0);
        return out;
    }
    let flags = info.mode & MODE_PIXFLAGS != 0;
    let plan = ct::Plan::for_channels(nc);
    let mut model = ct::CtModel::new(w, h, info.depth as u32, plan.clone(), tune.clone());
    for y in 0..h {
        model.start_row(y);
        for x in 0..w {
            let base = (y * w + x) * nc;
            let found = if flags { pix.code(&mut dec, x, y, 0) } else { None };
            match found {
                Some(k) => {
                    for ci in 0..nc {
                        let v = ((k >> (16 * plan.src[ci])) & 0xFFFF) as i32;
                        out[base + plan.src[ci]] = v as u16;
                        model.code_sample(&mut dec, x, ci, v, true);
                    }
                }
                None => {
                    for ci in 0..nc {
                        out[base + plan.src[ci]] = model.code_sample(&mut dec, x, ci, 0, false) as u16;
                    }
                }
            }
            if flags {
                pix.finish(x, y, pack(&out[base..base + nc]));
            }
        }
        model.end_row();
        if y % 16 == 15 {
            progress((y + 1) as f32 / h as f32);
        }
    }
    progress(1.0);
    out
}

/// Rebuilds the original image from the decoded coded samples and verifies the checksum.
pub fn finish(info: &StreamInfo, coded: Vec<u16>) -> Result<Image, String> {
    let img = Image { width: info.width, height: info.height, channels: info.coded_channels, depth: info.depth, data: coded };
    let img = expand(img, info.reductions, info.channels);
    if crc32(&img.sample_bytes()) != info.crc {
        return Err("checksum mismatch: file is corrupt".into());
    }
    Ok(img)
}

fn run_parallel<T: Send, F: Fn(usize) -> T + Sync>(n: usize, f: F) -> Vec<T> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if n > 1 {
            return std::thread::scope(|s| {
                let f = &f;
                let hs: Vec<_> = (0..n).map(|k| s.spawn(move || f(k))).collect();
                hs.into_iter().map(|h| h.join().expect("worker thread panicked")).collect()
            });
        }
    }
    (0..n).map(f).collect()
}

pub fn compress(img: &Image, src: SourceFormat, opts: &Options, progress: &mut dyn FnMut(f32)) -> Result<Vec<u8>, String> {
    let prep = prepare(img, src)?;
    let stripes = prep.info.stripes();
    let w = prep.info.width as usize;
    let nc = prep.info.coded_channels as usize;
    let streams = if stripes.len() == 1 {
        vec![encode_stripe(&prep.info, &prep.coded.data, stripes[0].1, &opts.tuning, progress)]
    } else {
        let s = run_parallel(stripes.len(), |k| {
            let (y0, y1) = stripes[k];
            encode_stripe(&prep.info, &prep.coded.data[y0 * w * nc..y1 * w * nc], y1 - y0, &opts.tuning, &mut |_| {})
        });
        progress(1.0);
        s
    };
    Ok(assemble(&prep.header, &streams))
}

pub fn decompress(b: &[u8], opts: &Options, progress: &mut dyn FnMut(f32)) -> Result<(Image, SourceFormat), String> {
    let (info, hl) = parse_header(b)?;
    let parts = split_stripes(b, &info, hl)?;
    let stripes = info.stripes();
    let decoded: Vec<Vec<u16>> = if stripes.len() == 1 {
        vec![decode_stripe(&info, parts[0], stripes[0].1, &opts.tuning, progress)]
    } else {
        let d = run_parallel(stripes.len(), |k| decode_stripe(&info, parts[k], stripes[k].1 - stripes[k].0, &opts.tuning, &mut |_| {}));
        progress(1.0);
        d
    };
    let coded: Vec<u16> = decoded.into_iter().flatten().collect();
    Ok((finish(&info, coded)?, info.source))
}
