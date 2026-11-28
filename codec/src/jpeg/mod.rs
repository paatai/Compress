//! Lossless JPEG recompression: a `.komla` file that decodes back to the exact original bytes.
//!
//! Container (multi-byte integers little-endian, `varint` = LEB128):
//!   "KMLA" | 'J' | version | mode | varint original length | CRC-32 of the original file
//!   mode RAW:  the original bytes
//!   mode CM:   the original bytes coded with the general-purpose byte model
//!   mode JPEG: varint header-blob length | varint coded length | coded header blob
//!              varint trailer length | varint coded length | trailer kind | coded trailer
//!              varint side-info length | side info
//!              varint stripe count | varint byte length of every stripe but the last | stripes
//!
//! The header blob is the input with the entropy-coded scan data removed (all marker segments,
//! including EXIF/ICC/XMP and the Huffman and quantization tables, verbatim). The stripes hold
//! the quantized DCT coefficients coded by `model`, in bands of MCU rows that are independent of
//! each other. The side info holds what the coefficients do not determine (padding bits, bytes
//! between scan data and the next marker, end-of-band run boundaries). Before anything is
//! written the scans are regenerated from the coefficients and compared with the input; any
//! difference, or anything unsupported, falls back to storing the input as it is.
//!
//! Data after the end of the image (trailer kind 0) is coded with the byte model; if it is
//! itself a JPEG, as cameras append previews and depth maps, it can instead be a complete
//! nested .komla JPEG file (kind 1).

pub mod bytecm;
pub mod fast;
pub mod format;
pub mod huff;
pub mod model;

use crate::coder::{Decoder, Encoder};
use format::{Frame, Layout};

pub const KIND: u8 = b'J';
pub const VERSION: u8 = 1;
pub const MODE_RAW: u8 = 0;
pub const MODE_CM: u8 = 1;
pub const MODE_JPEG: u8 = 2;

/// Stripes hold about this many blocks (all components); smaller images use one stripe.
pub const STRIPE_BLOCKS: usize = 1 << 17;

pub fn is_jpeg(b: &[u8]) -> bool {
    b.len() >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF
}

pub fn is_container(b: &[u8]) -> bool {
    b.len() >= 6 && &b[..4] == crate::MAGIC && b[4] == KIND
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varint(b: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*pos).ok_or("truncated .komla file")?;
        *pos += 1;
        v |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("corrupt .komla file".into())
}

fn take<'a>(b: &'a [u8], pos: &mut usize, n: u64) -> Result<&'a [u8], String> {
    let n = usize::try_from(n).map_err(|_| "corrupt .komla file")?;
    let end = pos.checked_add(n).filter(|&e| e <= b.len()).ok_or("truncated .komla file")?;
    let s = &b[*pos..end];
    *pos = end;
    Ok(s)
}

/// Summary of a JPEG for display.
#[derive(Clone, Debug, Default)]
pub struct JpegInfo {
    pub width: u32,
    pub height: u32,
    pub components: u8,
    pub progressive: bool,
    /// e.g. "4:2:0", "4:4:4", "gray"
    pub sampling: String,
    pub stripes: u32,
    /// how the file is stored: MODE_RAW, MODE_CM or MODE_JPEG
    pub mode: u8,
    /// why the file could not be recompressed (empty if it was)
    pub note: String,
}

fn sampling(f: &Frame) -> String {
    if f.comps.len() == 1 {
        return "gray".into();
    }
    let c = &f.comps;
    if c.len() == 3 && c[1].h == c[2].h && c[1].v == c[2].v && c[1].h == 1 && c[1].v == 1 {
        return match (c[0].h, c[0].v) {
            (1, 1) => "4:4:4".into(),
            (2, 1) => "4:2:2".into(),
            (2, 2) => "4:2:0".into(),
            (1, 2) => "4:4:0".into(),
            (4, 1) => "4:1:1".into(),
            _ => c.iter().map(|x| format!("{}x{}", x.h, x.v)).collect::<Vec<_>>().join(","),
        };
    }
    c.iter().map(|x| format!("{}x{}", x.h, x.v)).collect::<Vec<_>>().join(",")
}

/// MCU rows of each stripe.
pub fn stripe_rows(f: &Frame) -> Vec<(usize, usize)> {
    let blocks_per_mcu_row: usize = f.comps.iter().map(|c| c.pw * c.v).sum();
    let total = blocks_per_mcu_row * f.mcuy;
    let n = total.div_ceil(STRIPE_BLOCKS).clamp(1, f.mcuy.clamp(1, 64));
    let rows = f.mcuy.div_ceil(n);
    (0..n).map(|k| (k * rows, ((k + 1) * rows).min(f.mcuy))).filter(|r| r.0 < r.1).collect()
}

/// Which blocks of each component the scans cover: (DC coded, AC coded, DC extent, AC extent).
fn coverage(l: &Layout) -> Vec<(bool, bool, (usize, usize), (usize, usize))> {
    let f = &l.frame;
    f.comps
        .iter()
        .enumerate()
        .map(|(ci, c)| {
            let (mut dc, mut ac, mut dcp, mut acp) = (false, false, false, false);
            for s in &l.scans {
                if !s.comps.contains(&ci) {
                    continue;
                }
                if s.ss == 0 {
                    dc = true;
                    dcp |= s.interleaved();
                }
                if s.se > 0 {
                    ac = true;
                    acp |= s.interleaved();
                }
            }
            let ext = |p: bool| if p { (c.pw, c.ph) } else { (c.bw, c.bh) };
            (dc, ac, ext(dcp), ext(acp))
        })
        .collect()
}

fn stripe_model(l: &Layout, k: usize) -> (model::Model, Vec<(usize, usize)>) {
    let f = &l.frame;
    let (my0, my1) = stripe_rows(f)[k];
    let rows: Vec<(usize, usize)> = f.comps.iter().map(|c| (my0 * c.v, my1 * c.v)).collect();
    let cov = coverage(l);
    let coded: Vec<(bool, bool)> = cov.iter().map(|c| (c.0, c.1)).collect();
    let mut m = model::Model::new(f, &rows, &coded);
    for (ci, c) in cov.iter().enumerate() {
        let r0 = rows[ci].0;
        m.set_extent(ci, (c.2 .0, c.2 .1.saturating_sub(r0)), (c.3 .0, c.3 .1.saturating_sub(r0)));
    }
    (m, rows)
}

/// Codes the coefficients of stripe `k` (`coefs`: every component's rows of the stripe,
/// concatenated in component order).
pub fn encode_stripe(blob: &[u8], k: usize, coefs: &[i16], progress: &mut dyn FnMut(f32)) -> Result<Vec<u8>, String> {
    let l = format::parse(blob, false)?;
    if k >= stripe_rows(&l.frame).len() {
        return Err("bad stripe index".into());
    }
    let (mut m, rows) = stripe_model(&l, k);
    let mut buf = coefs.to_vec();
    let mut planes = split_planes(&l.frame, &rows, &mut buf)?;
    let mut e = Encoder::new();
    m.code(&mut e, &mut planes, progress);
    Ok(e.finish())
}

/// Decodes stripe `k`; returns its coefficients in the layout of `encode_stripe`.
pub fn decode_stripe(blob: &[u8], k: usize, data: &[u8], progress: &mut dyn FnMut(f32)) -> Result<Vec<i16>, String> {
    let l = format::parse(blob, false)?;
    if k >= stripe_rows(&l.frame).len() {
        return Err("bad stripe index".into());
    }
    let (mut m, rows) = stripe_model(&l, k);
    let n: usize = l.frame.comps.iter().zip(rows.iter()).map(|(c, r)| c.pw * (r.1 - r.0) * 64).sum();
    let mut buf = vec![0i16; n];
    {
        let mut planes = split_planes(&l.frame, &rows, &mut buf)?;
        let mut d = Decoder::new(data);
        m.code(&mut d, &mut planes, progress);
    }
    Ok(buf)
}

fn split_planes<'a>(f: &Frame, rows: &[(usize, usize)], buf: &'a mut [i16]) -> Result<Vec<&'a mut [i16]>, String> {
    let mut planes = Vec::with_capacity(f.comps.len());
    let mut rest = buf;
    for (c, r) in f.comps.iter().zip(rows.iter()) {
        let n = c.pw * (r.1 - r.0) * 64;
        if rest.len() < n {
            return Err("stripe size mismatch".into());
        }
        let (a, b) = rest.split_at_mut(n);
        planes.push(a);
        rest = b;
    }
    Ok(planes)
}

/// Gathers the coefficients of one stripe from whole-image planes.
fn stripe_coefs(f: &Frame, coefs: &[Vec<i16>], my0: usize, my1: usize) -> Vec<i16> {
    let mut out = Vec::new();
    for (c, plane) in f.comps.iter().zip(coefs.iter()) {
        out.extend_from_slice(&plane[my0 * c.v * c.pw * 64..my1 * c.v * c.pw * 64]);
    }
    out
}

/// Everything needed to code the stripes and write the file.
pub struct Prepared {
    /// container bytes before the stripe table (the complete file in a stored mode)
    pub header: Vec<u8>,
    /// header blob (input for `encode_stripe`)
    pub blob: Vec<u8>,
    /// coefficients of each stripe
    pub stripes: Vec<Vec<i16>>,
    pub info: JpegInfo,
}

fn stored(data: &[u8], note: String) -> Prepared {
    let mut h = Vec::with_capacity(data.len() + 16);
    h.extend_from_slice(crate::MAGIC);
    h.push(KIND);
    h.push(VERSION);
    let cm = bytecm::encode(data);
    let mode = if cm.len() < data.len() { MODE_CM } else { MODE_RAW };
    h.push(mode);
    put_varint(&mut h, data.len() as u64);
    h.extend_from_slice(&crate::crc32(data).to_le_bytes());
    h.extend_from_slice(if mode == MODE_CM { &cm } else { data });
    Prepared { header: h, blob: Vec::new(), stripes: Vec::new(), info: JpegInfo { mode, note, ..Default::default() } }
}

/// Parses and verifies a JPEG and prepares it for coding. Never fails: input that cannot be
/// recompressed exactly is stored (see `JpegInfo::note`).
pub fn prepare(data: &[u8]) -> Prepared {
    match analyze(data) {
        Ok(p) => p,
        Err(e) => stored(data, e),
    }
}

const TRAILER_CM: u8 = 0;
const TRAILER_JPEG: u8 = 1;

/// Codes the bytes after the end of the image: recursively as a JPEG if they are one (and that
/// is smaller), otherwise with the byte model.
fn encode_trailer(t: &[u8]) -> (u8, Vec<u8>) {
    let cm = bytecm::encode(t);
    if is_jpeg(t) {
        let (nested, info) = compress(t, &mut |_| {});
        if info.mode == MODE_JPEG && nested.len() < cm.len() {
            return (TRAILER_JPEG, nested);
        }
    }
    (TRAILER_CM, cm)
}

fn decode_trailer(kind: u8, coded: &[u8], len: usize) -> Result<Vec<u8>, String> {
    let t = match kind {
        TRAILER_CM => bytecm::decode(coded, len)?,
        TRAILER_JPEG => decompress(coded, &mut |_| {})?,
        _ => return Err("corrupt .komla file".into()),
    };
    if t.len() != len {
        return Err("checksum mismatch: the .komla file is damaged or incomplete".into());
    }
    Ok(t)
}

fn analyze(data: &[u8]) -> Result<Prepared, String> {
    if data.len() > u32::MAX as usize {
        return Err("file too large".into());
    }
    let l = format::parse(data, true)?;
    let (coefs, sides) = format::decode_scans(&l, data)?;
    // regenerate every scan from the coefficients; this also codes the side info
    let mut side_enc = Encoder::new();
    let mut counts = [[0u32; 2]; 8];
    for (s, rec) in l.scans.iter().zip(sides.iter()) {
        let mut sc = format::SideCoder::new(&mut side_enc, Some(rec), &mut counts);
        let regen = format::encode_scan(&l.frame, s, &coefs, &mut sc)?;
        if regen[..] != data[s.seg.0..s.seg.1] {
            return Err("scan data cannot be reproduced exactly from its coefficients".into());
        }
    }
    let side = side_enc.finish();
    if let Some(path) = std::env::var_os("KOMLA_DUMP") {
        // debugging aid: quantization tables and coefficients of every component
        let mut d = Vec::new();
        for (c, plane) in l.frame.comps.iter().zip(coefs.iter()) {
            for v in [c.pw as u32, c.ph as u32, c.h as u32, c.v as u32] {
                d.extend_from_slice(&v.to_le_bytes());
            }
            d.extend(c.q.iter().flat_map(|v| v.to_le_bytes()));
            d.extend(plane.iter().flat_map(|v| v.to_le_bytes()));
        }
        let _ = std::fs::write(path, d);
    }
    let trailer = &data[l.end..];
    let f = &l.frame;
    let mut h = Vec::with_capacity(4096);
    h.extend_from_slice(crate::MAGIC);
    h.push(KIND);
    h.push(VERSION);
    h.push(MODE_JPEG);
    put_varint(&mut h, data.len() as u64);
    h.extend_from_slice(&crate::crc32(data).to_le_bytes());
    let c = bytecm::encode(&l.blob);
    put_varint(&mut h, l.blob.len() as u64);
    put_varint(&mut h, c.len() as u64);
    h.extend_from_slice(&c);
    let (kind, c) = encode_trailer(trailer);
    put_varint(&mut h, trailer.len() as u64);
    put_varint(&mut h, c.len() as u64);
    h.push(kind);
    h.extend_from_slice(&c);
    put_varint(&mut h, side.len() as u64);
    h.extend_from_slice(&side);
    let rows = stripe_rows(f);
    let stripes = rows.iter().map(|&(a, b)| stripe_coefs(f, &coefs, a, b)).collect();
    let info = JpegInfo {
        width: f.width as u32,
        height: f.height as u32,
        components: f.comps.len() as u8,
        progressive: f.progressive,
        sampling: sampling(f),
        stripes: rows.len() as u32,
        mode: MODE_JPEG,
        note: String::new(),
    };
    Ok(Prepared { header: h, blob: l.blob, stripes, info })
}

/// Joins the header and the coded stripes into a .komla file.
pub fn assemble(p: &Prepared, streams: &[Vec<u8>]) -> Vec<u8> {
    if p.info.mode != MODE_JPEG {
        return p.header.clone();
    }
    let mut out = p.header.clone();
    put_varint(&mut out, streams.len() as u64);
    for s in &streams[..streams.len().saturating_sub(1)] {
        put_varint(&mut out, s.len() as u64);
    }
    for s in streams {
        out.extend_from_slice(s);
    }
    out
}

fn run_parallel<T: Send, F: Fn(usize) -> T + Sync>(n: usize, f: F) -> Vec<T> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if n > 1 && std::env::var("KOMLA_THREADS").as_deref() != Ok("1") {
            return std::thread::scope(|s| {
                let f = &f;
                let hs: Vec<_> = (0..n).map(|k| s.spawn(move || f(k))).collect();
                hs.into_iter().map(|h| h.join().expect("worker thread panicked")).collect()
            });
        }
    }
    (0..n).map(f).collect()
}

/// Compresses a JPEG file (stripes in parallel on native targets). Never fails.
pub fn compress(data: &[u8], progress: &mut dyn FnMut(f32)) -> (Vec<u8>, JpegInfo) {
    let mut p = prepare(data);
    if p.info.mode != MODE_JPEG {
        progress(1.0);
        let out = p.header.clone();
        return (out, p.info);
    }
    let n = p.stripes.len();
    let streams: Result<Vec<Vec<u8>>, String> = if n == 1 {
        vec![encode_stripe(&p.blob, 0, &p.stripes[0], progress)].into_iter().collect()
    } else {
        let r = run_parallel(n, |k| encode_stripe(&p.blob, k, &p.stripes[k], &mut |_| {}));
        progress(1.0);
        r.into_iter().collect()
    };
    match streams {
        Ok(s) => {
            let out = assemble(&p, &s);
            p.stripes.clear();
            (out, p.info)
        }
        Err(e) => {
            let s = stored(data, e);
            (s.header.clone(), s.info)
        }
    }
}

/// A parsed .komla JPEG container.
pub struct Container<'a> {
    pub mode: u8,
    pub len: usize,
    pub crc: u32,
    /// stored modes: the payload; JPEG mode: unused
    pub payload: &'a [u8],
    pub blob: Vec<u8>,
    /// data after the end of the image: kind, coded bytes and length (decoded by `finish`)
    pub trailer: (u8, &'a [u8], usize),
    pub side: &'a [u8],
    pub stripes: Vec<&'a [u8]>,
}

pub fn parse_container(b: &[u8]) -> Result<Container<'_>, String> {
    if !is_container(b) {
        return Err("not a Komla JPEG file".into());
    }
    if b[5] != VERSION {
        return Err(format!("unsupported Komla JPEG version {}", b[5]));
    }
    let mode = *b.get(6).ok_or("truncated .komla file")?;
    let mut pos = 7;
    let len = usize::try_from(get_varint(b, &mut pos)?).map_err(|_| "corrupt .komla file")?;
    let crc = u32::from_le_bytes(take(b, &mut pos, 4)?.try_into().unwrap());
    let mut c = Container { mode, len, crc, payload: &[], blob: Vec::new(), trailer: (TRAILER_CM, &[], 0), side: &[], stripes: Vec::new() };
    match mode {
        MODE_RAW | MODE_CM => {
            c.payload = &b[pos..];
            Ok(c)
        }
        MODE_JPEG => {
            let n = get_varint(b, &mut pos)?;
            let cl = get_varint(b, &mut pos)?;
            if n > len as u64 {
                return Err("corrupt .komla file".into());
            }
            c.blob = bytecm::decode(take(b, &mut pos, cl)?, n as usize)?;
            let n = get_varint(b, &mut pos)?;
            let cl = get_varint(b, &mut pos)?;
            if n > len as u64 {
                return Err("corrupt .komla file".into());
            }
            let kind = *b.get(pos).ok_or("truncated .komla file")?;
            pos += 1;
            c.trailer = (kind, take(b, &mut pos, cl)?, n as usize);
            let sl = get_varint(b, &mut pos)?;
            c.side = take(b, &mut pos, sl)?;
            let n = get_varint(b, &mut pos)? as usize;
            if n == 0 || n > 64 {
                return Err("corrupt stripe table".into());
            }
            let mut lens = Vec::with_capacity(n);
            for _ in 0..n - 1 {
                lens.push(get_varint(b, &mut pos)?);
            }
            for l in lens {
                c.stripes.push(take(b, &mut pos, l)?);
            }
            c.stripes.push(&b[pos..]);
            Ok(c)
        }
        _ => Err("unknown Komla JPEG mode".into()),
    }
}

/// Rebuilds the original file from the decoded stripes and checks its CRC.
pub fn finish(c: &Container, stripes: Vec<Vec<i16>>) -> Result<Vec<u8>, String> {
    let out = match c.mode {
        MODE_RAW => c.payload.to_vec(),
        MODE_CM => bytecm::decode(c.payload, c.len)?,
        _ => rebuild(c, stripes)?,
    };
    if out.len() != c.len || crate::crc32(&out) != c.crc {
        return Err("checksum mismatch: the .komla file is damaged or incomplete".into());
    }
    Ok(out)
}

fn rebuild(c: &Container, stripes: Vec<Vec<i16>>) -> Result<Vec<u8>, String> {
    let l = format::parse(&c.blob, false)?;
    let f = &l.frame;
    let rows = stripe_rows(f);
    if stripes.len() != rows.len() {
        return Err("corrupt stripe table".into());
    }
    let mut coefs: Vec<Vec<i16>> = f.comps.iter().map(|c| Vec::with_capacity(c.pw * c.ph * 64)).collect();
    for (k, s) in stripes.iter().enumerate() {
        let (my0, my1) = rows[k];
        let mut off = 0;
        for (ci, comp) in f.comps.iter().enumerate() {
            let n = (my1 - my0) * comp.v * comp.pw * 64;
            coefs[ci].extend_from_slice(s.get(off..off + n).ok_or("stripe size mismatch")?);
            off += n;
        }
    }
    let mut out = Vec::with_capacity(c.len);
    let mut dec = Decoder::new(c.side);
    let mut counts = [[0u32; 2]; 8];
    let mut pos = 0;
    for s in &l.scans {
        out.extend_from_slice(&c.blob[pos..s.blob_pos]);
        pos = s.blob_pos;
        let mut sc = format::SideCoder::new(&mut dec, None, &mut counts);
        sc.limit = c.len;
        out.extend_from_slice(&format::encode_scan(f, s, &coefs, &mut sc)?);
        if out.len() > c.len {
            return Err("checksum mismatch: the .komla file is damaged or incomplete".into());
        }
    }
    out.extend_from_slice(&c.blob[pos..]);
    out.extend_from_slice(&decode_trailer(c.trailer.0, c.trailer.1, c.trailer.2)?);
    Ok(out)
}

/// Decompresses a .komla JPEG container (stripes in parallel on native targets).
pub fn decompress(b: &[u8], progress: &mut dyn FnMut(f32)) -> Result<Vec<u8>, String> {
    let c = parse_container(b)?;
    let stripes: Vec<Vec<i16>> = if c.mode == MODE_JPEG {
        let n = c.stripes.len();
        let r: Result<Vec<Vec<i16>>, String> = if n == 1 {
            vec![decode_stripe(&c.blob, 0, c.stripes[0], progress)].into_iter().collect()
        } else {
            let r = run_parallel(n, |k| decode_stripe(&c.blob, k, c.stripes[k], &mut |_| {}));
            progress(1.0);
            r.into_iter().collect()
        };
        r?
    } else {
        Vec::new()
    };
    finish(&c, stripes)
}

// ---------------------------------------------------------------------------------------------
// Fast mode
//
//   "KMLA" | 'J' | version | MODE_FAST | varint original length | CRC-32 of the original file
//   varint header-blob length | varint coded length | header blob (`encode_blob`)
//   varint trailer length | trailer kind | varint coded length | coded trailer
//   varint side-info length | side info (per scan: padding, garbage, end-of-band choices)
//   varint stripe count | per stripe: varint rANS length, varint raw length | stripe data
//
// The side info can be decoded before any scan, so scans and stripes are independent: stripes
// are coded and decoded in parallel, straight from and into the coefficient planes.

/// Stripes of the fast mode hold about this many blocks (all components).
pub const FAST_STRIPE_BLOCKS: usize = 1 << 17;
pub const MODE_FAST: u8 = 3;
/// Stored with the fast byte model.
pub const MODE_BYTES: u8 = 4;
const TRAILER_FAST: u8 = 2;

/// Worker threads: KOMLA_THREADS if set, else the machine's parallelism (1 on WebAssembly).
pub fn threads() -> usize {
    #[cfg(target_arch = "wasm32")]
    {
        1
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(n) = std::env::var("KOMLA_THREADS").ok().and_then(|s| s.parse::<usize>().ok()) {
            return n.max(1);
        }
        std::thread::available_parallelism().map_or(1, |n| n.get())
    }
}

/// MCU rows of each fast-mode stripe.
pub fn fast_stripes(f: &Frame) -> Vec<(usize, usize)> {
    let per_row: usize = f.comps.iter().map(|c| c.pw * c.v).sum();
    let n = (per_row * f.mcuy).div_ceil(FAST_STRIPE_BLOCKS).clamp(1, f.mcuy.clamp(1, 256));
    let rows = f.mcuy.div_ceil(n);
    (0..n).map(|k| (k * rows, ((k + 1) * rows).min(f.mcuy))).filter(|r| r.0 < r.1).collect()
}

/// Splits each component's plane into the block rows of every stripe.
fn split_rows<'a>(f: &Frame, rows: &[(usize, usize)], planes: &'a mut [Vec<i16>]) -> Vec<Vec<&'a mut [i16]>> {
    let mut out: Vec<Vec<&'a mut [i16]>> = (0..rows.len()).map(|_| Vec::with_capacity(f.comps.len())).collect();
    for (c, plane) in f.comps.iter().zip(planes.iter_mut()) {
        let mut rest: &'a mut [i16] = &mut plane[..];
        for (k, &(my0, my1)) in rows.iter().enumerate() {
            let n = (my1 - my0) * c.v * c.pw * 64;
            let (a, b) = std::mem::take(&mut rest).split_at_mut(n);
            out[k].push(a);
            rest = b;
        }
    }
    out
}

struct StripeGeom {
    width: Vec<usize>,
    v: Vec<usize>,
    q: Vec<[u16; 64]>,
    cov: Vec<(bool, bool, (usize, usize), (usize, usize))>,
}

impl StripeGeom {
    fn new(l: &Layout) -> StripeGeom {
        let f = &l.frame;
        StripeGeom { width: f.comps.iter().map(|c| c.pw).collect(), v: f.comps.iter().map(|c| c.v).collect(), q: f.comps.iter().map(|c| c.q).collect(), cov: coverage(l) }
    }
    fn extents(&self, my0: usize) -> Vec<fast::Extent> {
        self.cov
            .iter()
            .zip(&self.v)
            .map(|(c, &v)| {
                let r0 = my0 * v;
                fast::Extent { dc: c.0, ac: c.1, dc_w: c.2 .0, dc_h: c.2 .1.saturating_sub(r0), ac_w: c.3 .0, ac_h: c.3 .1.saturating_sub(r0) }
            })
            .collect()
    }
    fn encode(&self, rows: (usize, usize), planes: &mut [&mut [i16]], start: Option<&fast::Model>) -> (Vec<u8>, Vec<u8>, fast::Model) {
        let ext = self.extents(rows.0);
        let held: Vec<usize> = self.v.iter().map(|&v| v * (rows.1 - rows.0)).collect();
        fast::encode_stripe(planes, &fast::Geometry { rows: &held, width: &self.width, v: &self.v, ext: &ext, q: &self.q, mcu_rows: rows.1 - rows.0 }, start)
    }
    fn decode(&self, rows: (usize, usize), rans: &[u8], raw: &[u8], planes: &mut [&mut [i16]], start: Option<&fast::Model>) -> Result<fast::Model, String> {
        let ext = self.extents(rows.0);
        let held: Vec<usize> = self.v.iter().map(|&v| v * (rows.1 - rows.0)).collect();
        fast::decode_stripe(rans, raw, planes, &fast::Geometry { rows: &held, width: &self.width, v: &self.v, ext: &ext, q: &self.q, mcu_rows: rows.1 - rows.0 }, start, |_, _| Ok(()))
    }
}

/// The model every fast-mode stripe after the first starts from: the first stripe's final one
/// (see `fast::Model::into_seed`), handed from the thread that codes the first stripe to the
/// others; `None` if the first stripe failed.
struct Seed(std::sync::Mutex<Option<Option<std::sync::Arc<fast::Model>>>>, std::sync::Condvar);

impl Seed {
    fn new() -> Seed {
        Seed(std::sync::Mutex::new(None), std::sync::Condvar::new())
    }
    /// The model stripe `k` starts from: none for the first; else the first stripe's, once it
    /// is there.
    fn start(&self, k: usize) -> Result<Option<std::sync::Arc<fast::Model>>, String> {
        if k == 0 {
            return Ok(None);
        }
        let mut g = self.0.lock().unwrap();
        while g.is_none() {
            g = self.1.wait(g).unwrap();
        }
        g.clone().flatten().map(Some).ok_or_else(damaged)
    }
    /// Takes the outcome of coding stripe `k` (its final model); only the first one's is kept.
    fn offer(&self, k: usize, r: Result<fast::Model, String>) -> Result<(), String> {
        if k > 0 {
            return r.map(|_| ());
        }
        let (m, r) = match r {
            Ok(m) => (Some(std::sync::Arc::new(m.into_seed())), Ok(())),
            Err(e) => (None, Err(e)),
        };
        *self.0.lock().unwrap() = Some(m);
        self.1.notify_all();
        r
    }
}

fn zigzag(v: i32) -> u64 {
    ((v << 1) ^ (v >> 31)) as u32 as u64
}

fn unzigzag(v: u64) -> i32 {
    ((v >> 1) as u32 as i32) ^ -((v & 1) as i32)
}

/// Writes a scan's side info: the length of its entropy-coded data, the choices `s` and, for a
/// sequential scan, the encoder state at the start of every stripe after the first.
fn put_side(out: &mut Vec<u8>, s: &huff::Side, scan_len: usize, cps: &[huff::SeqState], ncomp: usize) {
    put_varint(out, scan_len as u64);
    put_varint(out, s.pads.len() as u64);
    out.extend_from_slice(&s.pads);
    put_varint(out, s.garbage.len() as u64);
    out.extend_from_slice(&s.garbage);
    put_varint(out, s.eob_flips.len() as u64);
    let mut last = 0u32;
    for &x in &s.eob_flips {
        put_varint(out, (x - last) as u64);
        last = x;
    }
    let mut at = 0u64;
    for cp in cps {
        put_varint(out, cp.bytes - at);
        at = cp.bytes;
        put_varint(out, ((cp.bits as u64) << 3) | cp.nbits as u64);
        for &p in &cp.pred[..ncomp] {
            put_varint(out, zigzag(p));
        }
    }
}

/// A scan's side info written by `put_side`, with `ncps` encoder states of `ncomp` components.
fn get_side(b: &[u8], pos: &mut usize, limit: usize, ncps: usize, ncomp: usize) -> Result<(huff::Side, u64, Vec<huff::SeqState>), String> {
    let mut s = huff::Side::default();
    let scan_len = get_varint(b, pos)?;
    if scan_len > limit as u64 {
        return Err("corrupt .komla file".into());
    }
    let n = get_varint(b, pos)?;
    s.pads = take(b, pos, n)?.to_vec();
    let n = get_varint(b, pos)?;
    if n > limit as u64 {
        return Err("corrupt .komla file".into());
    }
    s.garbage = take(b, pos, n)?.to_vec();
    let n = get_varint(b, pos)?;
    if n > limit as u64 {
        return Err("corrupt .komla file".into());
    }
    let mut last = 0u64;
    for _ in 0..n {
        last += get_varint(b, pos)?;
        s.eob_flips.push(u32::try_from(last).map_err(|_| "corrupt .komla file")?);
    }
    let mut cps = Vec::with_capacity(ncps);
    let mut at = 0u64;
    for _ in 0..ncps {
        at += get_varint(b, pos)?;
        if at > scan_len {
            return Err("corrupt .komla file".into());
        }
        let nb = get_varint(b, pos)?;
        let mut cp = huff::SeqState { bytes: at, nbits: (nb & 7) as u32, bits: ((nb >> 3) & 0x7F) as u32, pred: [0; 4] };
        for p in cp.pred.iter_mut().take(ncomp) {
            *p = unzigzag(get_varint(b, pos)?);
        }
        cps.push(cp);
    }
    Ok((s, scan_len, cps))
}

fn stored_fast(data: &[u8], note: String) -> (Vec<u8>, JpegInfo) {
    let mut h = Vec::with_capacity(data.len() + 16);
    h.extend_from_slice(crate::MAGIC);
    h.push(KIND);
    h.push(VERSION);
    let c = fast::encode_bytes(data);
    let mode = if c[0] == 1 { MODE_BYTES } else { MODE_RAW };
    h.push(mode);
    put_varint(&mut h, data.len() as u64);
    h.extend_from_slice(&crate::crc32(data).to_le_bytes());
    h.extend_from_slice(if mode == MODE_BYTES { &c } else { data });
    (h, JpegInfo { mode, note, ..Default::default() })
}

/// Compresses a JPEG file in the fast mode with up to `threads` threads. Never fails: input
/// that cannot be recompressed exactly is stored.
pub fn compress_fast(data: &[u8], threads: usize) -> (Vec<u8>, JpegInfo) {
    compress_nested(data, threads, 0)
}

/// `compress_fast` for a container nested `depth` deep (see `restore`).
fn compress_nested(data: &[u8], threads: usize, depth: u32) -> (Vec<u8>, JpegInfo) {
    match analyze_fast(data, threads, depth) {
        Ok(r) => r,
        Err(e) => stored_fast(data, e),
    }
}

/// First byte of a coded header blob in the forms of `encode_blob` (`fast::encode_bytes`
/// starts with 0 or 1).
const BLOB_PICTURES: u8 = 2;
const BLOB_LITE: u8 = 3;

/// Codes a header blob: JPEG pictures embedded in it (thumbnails in Exif or Photoshop
/// segments, often the same one twice) in the fast mode where that is smaller, the rest with
/// the light context-mixing model, or the byte model where that is not larger. Without
/// pictures and with the byte model this is `fast::encode_bytes`; otherwise
///   BLOB_PICTURES or BLOB_LITE | varint count | per picture: varint bytes of the rest before
///   it, varint length, varint coded length, coded picture (length 0: varint index of an
///   earlier picture with the same bytes) | rest (`fast::encode_bytes` or
///   `bytecm::encode_lite`)
fn encode_blob(blob: &[u8], depth: u32) -> Vec<u8> {
    // (gap, start, length, coded or earlier index)
    let mut pics: Vec<(usize, usize, usize, Result<Vec<u8>, usize>)> = Vec::new();
    let mut rest = Vec::new();
    // (the blob itself starts with a start-of-image marker; every candidate may be parsed up to
    // the end of the blob, so there are only a few)
    let (mut from, mut i) = (0, 1);
    for _ in 0..if depth < MAX_DEPTH { 16 } else { 0 } {
        let Some(k) = blob[i..].windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]) else { break };
        let s = i + k;
        i = s + 1;
        let Ok(l) = format::parse(&blob[s..], true) else { continue };
        let n = l.end;
        let pic = &blob[s..s + n];
        let coded = match pics.iter().position(|p| &blob[p.1..p.1 + p.2] == pic) {
            Some(j) => Err(j),
            None => match compress_nested(pic, 1, depth + 1) {
                (c, info) if info.mode == MODE_FAST && c.len() < n => Ok(c),
                _ => continue,
            },
        };
        rest.extend_from_slice(&blob[from..s]);
        pics.push((s - from, s, n, coded));
        from = s + n;
        i = from;
    }
    rest.extend_from_slice(&blob[from..]);
    let plain = fast::encode_bytes(&rest);
    let lite = bytecm::encode_lite(&rest);
    if pics.is_empty() && plain.len() <= lite.len() {
        return plain;
    }
    let mut out = vec![if plain.len() <= lite.len() { BLOB_PICTURES } else { BLOB_LITE }];
    put_varint(&mut out, pics.len() as u64);
    for (gap, _, n, coded) in &pics {
        put_varint(&mut out, *gap as u64);
        put_varint(&mut out, *n as u64);
        match coded {
            Ok(c) => {
                put_varint(&mut out, c.len() as u64);
                out.extend_from_slice(c);
            }
            Err(j) => {
                put_varint(&mut out, 0);
                put_varint(&mut out, *j as u64);
            }
        }
    }
    out.extend_from_slice(if plain.len() <= lite.len() { &plain } else { &lite });
    out
}

/// Inverse of `encode_blob` for a blob of `n` bytes in a container nested `depth` deep.
fn decode_blob(c: &[u8], n: usize, threads: usize, depth: u32) -> Result<Vec<u8>, String> {
    let kind = c.first().copied();
    if kind != Some(BLOB_PICTURES) && kind != Some(BLOB_LITE) {
        return fast::decode_bytes(c, n);
    }
    let mut pos = 1;
    let count = get_varint(c, &mut pos)?;
    if count > c.len() as u64 {
        return Err("corrupt .komla file".into());
    }
    let mut pics = Vec::new();
    let mut left = n as u64;
    for _ in 0..count {
        let gap = get_varint(c, &mut pos)?;
        let len = get_varint(c, &mut pos)?;
        let cl = get_varint(c, &mut pos)?;
        left = left.checked_sub(len).ok_or("corrupt .komla file")?;
        pics.push((gap, len, if cl == 0 { Err(get_varint(c, &mut pos)?) } else { Ok(take(c, &mut pos, cl)?) }));
    }
    let rest = if kind == Some(BLOB_LITE) { bytecm::decode_lite(&c[pos..], left as usize)? } else { fast::decode_bytes(&c[pos..], left as usize)? };
    let mut out = Vec::with_capacity(n);
    let mut at = 0usize;
    // where each picture went
    let mut done: Vec<(usize, u64)> = Vec::with_capacity(pics.len());
    for (gap, len, pic) in pics {
        let g = usize::try_from(gap).ok().filter(|&g| g <= rest.len() - at).ok_or("corrupt .komla file")?;
        out.extend_from_slice(&rest[at..at + g]);
        at += g;
        let start = out.len();
        match pic {
            Ok(pic) => {
                let (m, _) = restore(&pic, start as u64, &mut out, threads, depth + 1)?;
                if m != len {
                    return Err(damaged());
                }
            }
            Err(j) => {
                let &(s, m) = usize::try_from(j).ok().and_then(|j| done.get(j)).filter(|d| d.1 == len).ok_or("corrupt .komla file")?;
                out.extend_from_within(s..s + m as usize);
            }
        }
        done.push((start, len));
    }
    out.extend_from_slice(&rest[at..]);
    Ok(out)
}

/// A stripe on its way through compression: its coefficients and the choices its scans made.
struct Stripe {
    k: usize,
    bufs: Vec<Vec<i16>>,
    sides: Vec<format::ScanSide>,
}

/// Regenerates every scan from the stripes `next` yields (in order) and checks that the bytes
/// match the input, recording the side info that reproduces them; hands each checked stripe to
/// `pass`. Returns the side info of all scans.
fn verify_scans(l: &Layout, data: &[u8], rows: &[(usize, usize)], next: &mut dyn FnMut() -> Result<Option<Stripe>, String>, pass: &mut dyn FnMut(Stripe) -> Result<(), String>) -> Result<Vec<u8>, String> {
    let f = &l.frame;
    let n = l.scans.len();
    let mismatch = || "scan data cannot be reproduced exactly from its coefficients".to_string();
    let mut chs: Vec<huff::Choices> = (0..n).map(|_| huff::Choices::record()).collect();
    let mut outs: Vec<Vec<u8>> = (0..n).map(|_| Vec::new()).collect();
    let mut done = vec![0usize; n];
    let mut cps: Vec<Vec<huff::SeqState>> = (0..n).map(|_| Vec::new()).collect();
    let check = |i: usize, b: &[u8], done: &mut [usize]| -> Result<(), String> {
        let s = &l.scans[i];
        if data[s.seg.0..s.seg.1].get(done[i]..done[i] + b.len()) != Some(b) {
            return Err(mismatch());
        }
        done[i] += b.len();
        Ok(())
    };
    {
        let mut encs: Vec<ScanEnc> = Vec::with_capacity(n);
        for ((s, ch), out) in l.scans.iter().zip(chs.iter_mut()).zip(outs.iter_mut()) {
            encs.push(if f.progressive { ScanEnc::Prog(huff::ProgScan::new(s, f, ch, Vec::new())) } else { ScanEnc::Seq(huff::SeqScan::new(s, f, ch, out)?) });
        }
        while let Some(mut st) = next()? {
            let (my0, my1) = rows[st.k];
            for (i, (e, s)) in encs.iter_mut().zip(&l.scans).enumerate() {
                e.choices().feed(&mut st.sides[i]);
                let bufs = &st.bufs;
                e.rows(f, s, my0, my1, |ci, bx, by| {
                    let c = &f.comps[ci];
                    bufs[ci][((by - my0 * c.v) * c.pw + bx) * 64..][..64].try_into().unwrap()
                })?;
                let b = e.bytes();
                check(i, b, &mut done)?;
                b.clear();
                if let ScanEnc::Seq(e) = e {
                    if st.k + 1 < rows.len() {
                        cps[i].push(e.state(done[i] as u64));
                    }
                }
            }
            pass(st)?;
        }
        for (i, (e, s)) in encs.into_iter().zip(&l.scans).enumerate() {
            match e {
                ScanEnc::Prog(e) => {
                    let rest = e.finish(s)?;
                    check(i, &rest, &mut done)?;
                }
                ScanEnc::Seq(e) => e.finish(),
            }
        }
    }
    let mut side = Vec::new();
    for (i, (ch, s)) in chs.into_iter().zip(&l.scans).enumerate() {
        check(i, &outs[i], &mut done)?;
        if done[i] != s.seg.1 - s.seg.0 {
            return Err(mismatch());
        }
        put_side(&mut side, &ch.into_side(), s.seg.1 - s.seg.0, &cps[i], s.comps.len());
    }
    Ok(side)
}

fn analyze_fast(data: &[u8], threads: usize, depth: u32) -> Result<(Vec<u8>, JpegInfo), String> {
    if data.len() > u32::MAX as usize {
        return Err("file too large".into());
    }
    let l = format::parse(data, true)?;
    let f = &l.frame;
    let rows = fast_stripes(f);
    let geom = StripeGeom::new(&l);
    let trailer = &data[l.end..];
    // the header bytes and the trailer (which may be another JPEG)
    let extras = || {
        let blob = encode_blob(&l.blob, depth);
        let t = match (is_jpeg(trailer) && depth < MAX_DEPTH).then(|| compress_nested(trailer, threads, depth + 1)) {
            Some((nested, info)) if info.mode == MODE_FAST && nested.len() < trailer.len() => (TRAILER_FAST, nested),
            _ => (TRAILER_CM, fast::encode_bytes(trailer)),
        };
        (blob, t)
    };
    let mut decs: Vec<format::ScanDec> = l.scans.iter().map(|s| format::ScanDec::new(f, s, &data[s.seg.0..s.seg.1])).collect();
    // the coefficients of stripe k, from every scan
    let mut decode = |k: usize| -> Result<Stripe, String> {
        let (my0, my1) = rows[k];
        let mut bufs: Vec<Vec<i16>> = f.comps.iter().map(|c| vec![0i16; (my1 - my0) * c.v * c.pw * 64]).collect();
        let mut planes: Vec<&mut [i16]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
        for (d, s) in decs.iter_mut().zip(&l.scans) {
            d.rows(f, s, my0, my1, my0, &mut planes)?;
            if my1 == f.mcuy {
                d.finish()?;
            }
        }
        let sides = decs.iter_mut().map(|d| std::mem::take(&mut d.side)).collect();
        Ok(Stripe { k, bufs, sides })
    };
    let seed = Seed::new();
    let encode = |st: &mut Stripe| {
        let mut planes: Vec<&mut [i16]> = st.bufs.iter_mut().map(|b| &mut b[..]).collect();
        let start = seed.start(st.k).unwrap_or(None);
        let (r, w, m) = geom.encode(rows[st.k], &mut planes, start.as_deref());
        let _ = seed.offer(st.k, Ok(m));
        (r, w)
    };
    let mut streams: Vec<Option<(Vec<u8>, Vec<u8>)>> = (0..rows.len()).map(|_| None).collect();
    let (side, (blob, (kind, tc))) = if threads > 1 {
        stream_mt(&l, data, &rows, threads, &mut decode, &encode, &extras, &mut streams)?
    } else {
        let mut k = 0;
        let mut next = || -> Result<Option<Stripe>, String> {
            if k == rows.len() {
                return Ok(None);
            }
            k += 1;
            decode(k - 1).map(Some)
        };
        let side = verify_scans(&l, data, &rows, &mut next, &mut |mut st| {
            streams[st.k] = Some(encode(&mut st));
            Ok(())
        })?;
        (side, extras())
    };
    let streams: Vec<(Vec<u8>, Vec<u8>)> = streams.into_iter().collect::<Option<_>>().ok_or("worker thread failed")?;
    let mut h = Vec::with_capacity(4096 + blob.len() + tc.len() + side.len() + streams.iter().map(|s| s.0.len() + s.1.len()).sum::<usize>());
    h.extend_from_slice(crate::MAGIC);
    h.push(KIND);
    h.push(VERSION);
    h.push(MODE_FAST);
    put_varint(&mut h, data.len() as u64);
    h.extend_from_slice(&crate::crc32(data).to_le_bytes());
    put_varint(&mut h, l.blob.len() as u64);
    put_varint(&mut h, blob.len() as u64);
    h.extend_from_slice(&blob);
    put_varint(&mut h, trailer.len() as u64);
    h.push(kind);
    put_varint(&mut h, tc.len() as u64);
    h.extend_from_slice(&tc);
    put_varint(&mut h, side.len() as u64);
    h.extend_from_slice(&side);
    put_varint(&mut h, streams.len() as u64);
    for (r, w) in &streams {
        put_varint(&mut h, r.len() as u64);
        put_varint(&mut h, w.len() as u64);
    }
    for (r, w) in streams {
        h.extend_from_slice(&r);
        h.extend_from_slice(&w);
    }
    let info = JpegInfo {
        width: f.width as u32,
        height: f.height as u32,
        components: f.comps.len() as u8,
        progressive: f.progressive,
        sampling: sampling(f),
        stripes: rows.len() as u32,
        mode: MODE_FAST,
        note: String::new(),
    };
    Ok((h, info))
}

type Extras = (Vec<u8>, (u8, Vec<u8>));

/// The threaded compression pipeline: this thread decodes stripe after stripe, another checks
/// them in order, the others code them; at most `threads + 2` stripes are held at a time.
#[allow(clippy::too_many_arguments)]
fn stream_mt(
    l: &Layout,
    data: &[u8],
    rows: &[(usize, usize)],
    threads: usize,
    decode: &mut (dyn FnMut(usize) -> Result<Stripe, String> + Send),
    encode: &(dyn Fn(&mut Stripe) -> (Vec<u8>, Vec<u8>) + Sync),
    extras: &(dyn Fn() -> Extras + Sync),
    streams: &mut [Option<(Vec<u8>, Vec<u8>)>],
) -> Result<(Vec<u8>, Extras), String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Condvar, Mutex};
        let window = threads + 2;
        let live = (Mutex::new(0usize), Condvar::new());
        let stop = AtomicBool::new(false);
        let out: Vec<Mutex<Option<(Vec<u8>, Vec<u8>)>>> = (0..rows.len()).map(|_| Mutex::new(None)).collect();
        let (to_ver, from_dec) = mpsc::sync_channel::<Stripe>(2);
        let (to_work, from_ver) = mpsc::channel::<Stripe>();
        let from_ver = Mutex::new(from_ver);
        let res = std::thread::scope(|sc| -> Result<(Vec<u8>, Extras), String> {
            let ex = sc.spawn(extras);
            let (live, stop, out, from_ver) = (&live, &stop, &out, &from_ver);
            let ver = sc.spawn(move || {
                let mut next = || Ok(from_dec.recv().ok());
                let r = verify_scans(l, data, rows, &mut next, &mut |st| to_work.send(st).map_err(|_| "worker thread failed".to_string()));
                if r.is_err() {
                    stop.store(true, Ordering::Relaxed);
                    live.1.notify_all();
                }
                r
            });
            for _ in 0..threads.saturating_sub(1).max(1) {
                sc.spawn(move || loop {
                    let st = from_ver.lock().unwrap().recv();
                    let Ok(mut st) = st else { break };
                    // the first stripe is coded in any case: the others wait for its model
                    if st.k == 0 || !stop.load(Ordering::Relaxed) {
                        let r = encode(&mut st);
                        *out[st.k].lock().unwrap() = Some(r);
                    }
                    drop(st);
                    *live.0.lock().unwrap() -= 1;
                    live.1.notify_all();
                });
            }
            let mut res = Ok(());
            for k in 0..rows.len() {
                let mut g = live.0.lock().unwrap();
                while *g >= window && !stop.load(Ordering::Relaxed) {
                    g = live.1.wait(g).unwrap();
                }
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                *g += 1;
                drop(g);
                match decode(k) {
                    Ok(st) => {
                        if to_ver.send(st).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        res = Err(e);
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
            drop(to_ver);
            let side = ver.join().map_err(|_| "worker thread failed".to_string())?;
            let ex = ex.join().map_err(|_| "worker thread failed".to_string())?;
            res?;
            Ok((side?, ex))
        });
        for (s, o) in streams.iter_mut().zip(out) {
            *s = o.into_inner().unwrap();
        }
        res
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = threads;
        let mut k = 0;
        let mut next = || -> Result<Option<Stripe>, String> {
            if k == rows.len() {
                return Ok(None);
            }
            k += 1;
            decode(k - 1).map(Some)
        };
        let side = verify_scans(l, data, rows, &mut next, &mut |mut st| {
            streams[st.k] = Some(encode(&mut st));
            Ok(())
        })?;
        Ok((side, extras()))
    }
}

/// Runs `produce` on 0..n with up to `threads` threads (at most `window` items ahead of the
/// consumer) and feeds the results to `consume` in order.
fn pipeline<B: Send>(n: usize, threads: usize, window: usize, produce: impl Fn(usize) -> Result<B, String> + Sync, mut consume: impl FnMut(usize, B) -> Result<(), String>) -> Result<(), String> {
    #[cfg(not(target_arch = "wasm32"))]
    if threads > 1 && n > 1 {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{mpsc, Condvar, Mutex};
        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let done = (Mutex::new(0usize), Condvar::new());
        let (tx, rx) = mpsc::channel::<(usize, Result<B, String>)>();
        return std::thread::scope(|sc| {
            for _ in 0..threads.min(n) {
                let tx = tx.clone();
                let (next, stop, done, produce) = (&next, &stop, &done, &produce);
                sc.spawn(move || loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    if k >= n {
                        break;
                    }
                    let mut g = done.0.lock().unwrap();
                    while k >= *g + window && !stop.load(Ordering::Relaxed) {
                        g = done.1.wait(g).unwrap();
                    }
                    drop(g);
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if tx.send((k, produce(k))).is_err() {
                        break;
                    }
                });
            }
            drop(tx);
            let mut pending = std::collections::BTreeMap::new();
            let mut expect = 0;
            let mut res = Ok(());
            while expect < n {
                let Ok((k, r)) = rx.recv() else { break };
                pending.insert(k, r);
                while let Some(r) = pending.remove(&expect) {
                    if res.is_ok() {
                        res = r.and_then(|b| consume(expect, b));
                    }
                    expect += 1;
                    *done.0.lock().unwrap() = expect;
                    done.1.notify_all();
                }
                if res.is_err() {
                    stop.store(true, Ordering::Relaxed);
                    done.1.notify_all();
                }
            }
            if res.is_ok() && expect < n {
                res = Err("worker thread failed".into());
            }
            res
        });
    }
    let _ = (threads, window);
    for k in 0..n {
        consume(k, produce(k)?)?;
    }
    Ok(())
}

/// Random-access input of a .komla file.
pub trait Input: Sync {
    fn size(&self) -> u64;
    /// Fills `buf` from position `pos`.
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<(), String>;
}

impl Input for &[u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<(), String> {
        let p = usize::try_from(pos).map_err(|_| "truncated .komla file")?;
        let s = p.checked_add(buf.len()).and_then(|e| self.get(p..e)).ok_or("truncated .komla file")?;
        buf.copy_from_slice(s);
        Ok(())
    }
}

/// A file read in place.
#[cfg(unix)]
struct FileIn<'a> {
    file: &'a std::fs::File,
    size: u64,
}

#[cfg(unix)]
impl Input for FileIn<'_> {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<(), String> {
        use std::os::unix::fs::FileExt;
        if pos.checked_add(buf.len() as u64).is_none_or(|e| e > self.size) {
            return Err("truncated .komla file".into());
        }
        self.file.read_exact_at(buf, pos).map_err(|e| e.to_string())
    }
}

/// A byte range of another input (a container nested in a trailer).
struct Sub<'a> {
    inner: &'a dyn Input,
    off: u64,
    len: u64,
}

impl Input for Sub<'_> {
    fn size(&self) -> u64 {
        self.len
    }
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<(), String> {
        if pos.checked_add(buf.len() as u64).is_none_or(|e| e > self.len) {
            return Err("truncated .komla file".into());
        }
        self.inner.read_at(self.off + pos, buf)
    }
}

/// Where restored bytes go, by position in the original file.
pub trait Sink {
    fn put(&mut self, pos: u64, data: &[u8]) -> Result<(), String>;
}

impl Sink for Vec<u8> {
    fn put(&mut self, pos: u64, data: &[u8]) -> Result<(), String> {
        let p = pos as usize;
        if self.len() < p + data.len() {
            self.resize(p + data.len(), 0);
        }
        self[p..p + data.len()].copy_from_slice(data);
        Ok(())
    }
}

#[cfg(unix)]
impl Sink for std::fs::File {
    fn put(&mut self, pos: u64, data: &[u8]) -> Result<(), String> {
        use std::os::unix::fs::FileExt;
        self.write_all_at(data, pos).map_err(|e| e.to_string())
    }
}

/// Sequential reader over an `Input`.
struct Rd<'a> {
    inp: &'a dyn Input,
    pos: u64,
}

impl Rd<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let mut b = [0u8];
        self.inp.read_at(self.pos, &mut b)?;
        self.pos += 1;
        Ok(b[0])
    }
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= ((b & 0x7F) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("corrupt .komla file".into())
    }
    fn bytes(&mut self, n: u64) -> Result<Vec<u8>, String> {
        if n > self.inp.size().saturating_sub(self.pos) {
            return Err("truncated .komla file".into());
        }
        let mut v = vec![0u8; n as usize];
        self.inp.read_at(self.pos, &mut v)?;
        self.pos += n;
        Ok(v)
    }
    /// Skips `n` bytes; returns the position they start at.
    fn skip(&mut self, n: u64) -> Result<u64, String> {
        let p = self.pos;
        if n > self.inp.size().saturating_sub(p) {
            return Err("truncated .komla file".into());
        }
        self.pos += n;
        Ok(p)
    }
}

/// The parts of a fast-mode container, with positions in the input.
struct FastContainer {
    blob: Vec<u8>,
    /// kind, position and coded length in the input, restored length
    trailer: (u8, u64, u64, u64),
    side: Vec<u8>,
    /// position, rANS length and raw length of every stripe
    stripes: Vec<(u64, u64, u64)>,
}

fn parse_fast(inp: &dyn Input, len: u64, pos: u64, threads: usize, depth: u32) -> Result<FastContainer, String> {
    let mut r = Rd { inp, pos };
    let n = r.varint()?;
    let cl = r.varint()?;
    if n > len {
        return Err("corrupt .komla file".into());
    }
    let blob = decode_blob(&r.bytes(cl)?, n as usize, threads, depth)?;
    let tl = r.varint()?;
    if tl > len {
        return Err("corrupt .komla file".into());
    }
    let kind = r.byte()?;
    let cl = r.varint()?;
    let tpos = r.skip(cl)?;
    let sl = r.varint()?;
    if sl > len + 1024 {
        return Err("corrupt .komla file".into());
    }
    let side = r.bytes(sl)?;
    let n = r.varint()? as usize;
    if n == 0 || n > 256 {
        return Err("corrupt stripe table".into());
    }
    let mut lens = Vec::with_capacity(n);
    for _ in 0..n {
        lens.push((r.varint()?, r.varint()?));
    }
    let mut stripes = Vec::with_capacity(n);
    for (a, b) in lens {
        let p = r.skip(a.checked_add(b).ok_or("corrupt stripe table")?)?;
        stripes.push((p, a, b));
    }
    Ok(FastContainer { blob, trailer: (kind, tpos, cl, tl), side, stripes })
}

/// One part of the restored file (a stretch of header bytes, a scan or the trailer), written
/// in order and checksummed on the way.
struct Part {
    start: u64,
    at: u64,
    end: u64,
    crc: u32,
    /// bytes not yet passed to the sink (they end at `at`); whole chunks write faster
    buf: Vec<u8>,
}

/// Bytes a part collects before writing them.
const PART_CHUNK: usize = 1 << 15;

impl Part {
    fn new(start: u64, len: u64) -> Part {
        Part { start, at: start, end: start + len, crc: 0, buf: Vec::new() }
    }
    fn write(&mut self, sink: &mut dyn Sink, data: &[u8]) -> Result<(), String> {
        if self.at + data.len() as u64 > self.end {
            return Err("checksum mismatch: the .komla file is damaged or incomplete".into());
        }
        self.crc = crate::crc32_continue(self.crc, data);
        self.at += data.len() as u64;
        if self.buf.is_empty() && data.len() >= PART_CHUNK {
            return sink.put(self.at - data.len() as u64, data);
        }
        self.buf.extend_from_slice(data);
        if self.buf.len() >= PART_CHUNK {
            self.flush(sink)?;
        }
        Ok(())
    }
    fn flush(&mut self, sink: &mut dyn Sink) -> Result<(), String> {
        if !self.buf.is_empty() {
            sink.put(self.at - self.buf.len() as u64, &self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
}

enum ScanEnc<'a> {
    Seq(huff::SeqScan<'a>),
    Prog(huff::ProgScan<'a>),
}

impl ScanEnc<'_> {
    fn rows<'b>(&mut self, f: &Frame, s: &format::Scan, my0: usize, my1: usize, blk: impl Fn(usize, usize, usize) -> &'b [i16; 64]) -> Result<(), String> {
        match self {
            ScanEnc::Seq(e) => e.rows(f, s, my0, my1, blk),
            ScanEnc::Prog(e) => e.rows(f, s, my0, my1, my1 == f.mcuy, blk),
        }
    }
    fn bytes(&mut self) -> &mut Vec<u8> {
        match self {
            ScanEnc::Seq(e) => e.bytes(),
            ScanEnc::Prog(e) => e.bytes(),
        }
    }
    fn choices(&mut self) -> &mut huff::Choices {
        match self {
            ScanEnc::Seq(e) => e.choices(),
            ScanEnc::Prog(e) => e.choices(),
        }
    }
}

/// Nested containers (JPEGs appended to JPEGs) at most this deep.
const MAX_DEPTH: u32 = 16;

fn damaged() -> String {
    "checksum mismatch: the .komla file is damaged or incomplete".into()
}

/// Restores the fast-mode or stored container in `inp` into `sink` at position `base`, with up
/// to `threads` threads; returns the restored length and CRC-32 (already checked). Nothing is
/// held in memory beyond a few stripes: scans are written straight to their place in the file.
fn restore(inp: &dyn Input, base: u64, sink: &mut dyn Sink, threads: usize, depth: u32) -> Result<(u64, u32), String> {
    if depth > MAX_DEPTH {
        return Err("corrupt .komla file".into());
    }
    let mut r = Rd { inp, pos: 0 };
    let head = r.bytes(7.min(inp.size()))?;
    if head.len() < 7 || !is_container(&head) {
        return Err("not a Komla JPEG file".into());
    }
    if head[5] != VERSION {
        return Err(format!("unsupported Komla JPEG version {}", head[5]));
    }
    let len = r.varint()?;
    let crc = u32::from_le_bytes(r.bytes(4)?.try_into().unwrap());
    match head[6] {
        MODE_RAW if inp.size() - r.pos == len => {
            let mut part = Part::new(base, len);
            let mut buf = vec![0u8; (len as usize).min(1 << 16)];
            while part.at - base < len {
                let n = (len - (part.at - base)).min(buf.len() as u64) as usize;
                inp.read_at(r.pos + (part.at - base), &mut buf[..n])?;
                part.write(sink, &buf[..n])?;
            }
            part.flush(sink)?;
            if part.crc != crc {
                return Err(damaged());
            }
            return Ok((len, crc));
        }
        MODE_RAW | MODE_BYTES => {
            let rest = r.bytes(inp.size() - r.pos)?;
            let out = if head[6] == MODE_RAW { rest } else { fast::decode_bytes(&rest, len as usize)? };
            if out.len() as u64 != len || crate::crc32(&out) != crc {
                return Err(damaged());
            }
            sink.put(base, &out)?;
            return Ok((len, crc));
        }
        MODE_FAST => {}
        _ => return Err("not a fast-mode Komla JPEG file".into()),
    }
    let c = parse_fast(inp, len, r.pos, threads, depth)?;
    let l = format::parse(&c.blob, false)?;
    let f = &l.frame;
    let rows = fast_stripes(f);
    if rows.len() != c.stripes.len() {
        return Err("corrupt stripe table".into());
    }
    let ncps = if f.progressive { 0 } else { rows.len() - 1 };
    let mut sides = Vec::with_capacity(l.scans.len());
    let mut lens = Vec::with_capacity(l.scans.len());
    let mut cps = Vec::with_capacity(l.scans.len());
    let mut pos = 0;
    for s in &l.scans {
        let (side, n, cp) = get_side(&c.side, &mut pos, len as usize, ncps, s.comps.len())?;
        sides.push(side);
        lens.push(n);
        cps.push(cp);
    }
    // the parts in file order: the header bytes before each scan, the scan, the rest of the
    // header bytes, the trailer
    let mut at = base;
    let mut heads: Vec<(Part, usize, usize)> = Vec::with_capacity(l.scans.len() + 1);
    let mut scans: Vec<Part> = Vec::with_capacity(l.scans.len());
    let mut bpos = 0;
    for (s, &n) in l.scans.iter().zip(&lens) {
        heads.push((Part::new(at, (s.blob_pos - bpos) as u64), bpos, s.blob_pos));
        at += (s.blob_pos - bpos) as u64;
        bpos = s.blob_pos;
        scans.push(Part::new(at, n));
        at = at.checked_add(n).ok_or_else(damaged)?;
    }
    heads.push((Part::new(at, (c.blob.len() - bpos) as u64), bpos, c.blob.len()));
    at += (c.blob.len() - bpos) as u64;
    let mut trailer = Part::new(at, c.trailer.3);
    if at + c.trailer.3 - base != len {
        return Err(damaged());
    }
    for (p, a, b) in heads.iter_mut() {
        p.write(sink, &c.blob[*a..*b])?;
        p.flush(sink)?;
    }
    let geom = StripeGeom::new(&l);
    let read = |k: usize, v: &mut Vec<u8>| -> Result<usize, String> {
        let (p, a, b) = c.stripes[k];
        v.resize((a + b) as usize, 0);
        inp.read_at(p, v)?;
        Ok(a as usize)
    };
    if threads > 1 && rows.len() > 1 && !f.progressive {
        // sequential scans can be regenerated stripe by stripe from the stored encoder states,
        // so the threads that decode the stripes also turn them back into scan data
        let seed = Seed::new();
        pipeline(
            rows.len(),
            threads,
            threads + 2,
            |k| -> Result<Vec<Vec<u8>>, String> {
                let (my0, my1) = rows[k];
                let mut bufs: Vec<Vec<i16>> = f.comps.iter().map(|comp| vec![0i16; (my1 - my0) * comp.v * comp.pw * 64]).collect();
                {
                    let mut data = Vec::new();
                    let mut planes: Vec<&mut [i16]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
                    let r = seed.start(k).and_then(|start| {
                        let split = read(k, &mut data)?;
                        geom.decode(rows[k], &data[..split], &data[split..], &mut planes, start.as_deref())
                    });
                    seed.offer(k, r)?;
                }
                let mut outs = Vec::with_capacity(l.scans.len());
                for (i, s) in l.scans.iter().enumerate() {
                    let start = if k > 0 { cps[i][k - 1].bytes } else { 0 };
                    let end = if k + 1 < rows.len() { cps[i][k].bytes } else { lens[i] };
                    let mut ch = huff::Choices::replay(sides[i].clone());
                    let mut out = Vec::with_capacity(end.saturating_sub(start) as usize + 64);
                    let mut e = huff::SeqScan::new(s, f, &mut ch, &mut out)?;
                    if k > 0 {
                        e.resume(f, s, my0, &cps[i][k - 1]);
                    }
                    e.rows(f, s, my0, my1, |ci, bx, by| {
                        let comp = &f.comps[ci];
                        bufs[ci][((by - my0 * comp.v) * comp.pw + bx) * 64..][..64].try_into().unwrap()
                    })?;
                    if k + 1 == rows.len() {
                        e.finish();
                    } else {
                        drop(e);
                    }
                    if Some(out.len() as u64) != end.checked_sub(start) {
                        return Err(damaged());
                    }
                    outs.push(out);
                }
                Ok(outs)
            },
            |_, outs| {
                for (o, p) in outs.iter().zip(scans.iter_mut()) {
                    p.write(sink, o)?;
                }
                Ok(())
            },
        )?;
        for p in scans.iter_mut() {
            p.flush(sink)?;
        }
    } else {
        let mut chs: Vec<huff::Choices> = sides.into_iter().map(huff::Choices::replay).collect();
        let mut outs: Vec<Vec<u8>> = l.scans.iter().map(|_| Vec::new()).collect();
        let mut encs: Vec<ScanEnc> = Vec::with_capacity(l.scans.len());
        for ((s, ch), out) in l.scans.iter().zip(chs.iter_mut()).zip(outs.iter_mut()) {
            encs.push(if f.progressive { ScanEnc::Prog(huff::ProgScan::new(s, f, ch, Vec::new())) } else { ScanEnc::Seq(huff::SeqScan::new(s, f, ch, out)?) });
        }
        // feeds frame MCU rows my0..my1 to every scan and writes out what they produce; the
        // blocks are in `planes`, `held[c]` block rows per component, from MCU row `top` on
        let mut feed = |top: usize, my0: usize, my1: usize, held: &[usize], planes: &[&mut [i16]], sink: &mut dyn Sink| -> Result<(), String> {
            for ((e, s), p) in encs.iter_mut().zip(&l.scans).zip(scans.iter_mut()) {
                e.rows(f, s, my0, my1, |ci, bx, by| {
                    let comp = &f.comps[ci];
                    let slot = (by - top * comp.v) % held[ci];
                    planes[ci][(slot * comp.pw + bx) * 64..][..64].try_into().unwrap()
                })?;
                let b = e.bytes();
                p.write(sink, b)?;
                b.clear();
            }
            Ok(())
        };
        if threads > 1 && rows.len() > 1 {
            let seed = Seed::new();
            pipeline(
                rows.len(),
                threads,
                threads + 2,
                |k| {
                    let (my0, my1) = rows[k];
                    let mut bufs: Vec<Vec<i16>> = f.comps.iter().map(|comp| vec![0i16; (my1 - my0) * comp.v * comp.pw * 64]).collect();
                    let mut data = Vec::new();
                    let mut planes: Vec<&mut [i16]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
                    let r = seed.start(k).and_then(|start| {
                        let split = read(k, &mut data)?;
                        geom.decode(rows[k], &data[..split], &data[split..], &mut planes, start.as_deref())
                    });
                    seed.offer(k, r)?;
                    Ok(bufs)
                },
                |k, mut bufs| {
                    let (my0, my1) = rows[k];
                    let held: Vec<usize> = geom.v.iter().map(|&v| v * (my1 - my0)).collect();
                    let planes: Vec<&mut [i16]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
                    feed(my0, my0, my1, &held, &planes, &mut *sink)
                },
            )?;
        } else {
            // one thread: decode each stripe into a ring of two MCU rows per component and turn
            // every MCU row back into scan data at once
            let held: Vec<usize> = geom.v.iter().map(|&v| 2 * v).collect();
            let mut ring: Vec<Vec<i16>> = f.comps.iter().zip(&held).map(|(comp, &h)| vec![0i16; h * comp.pw * 64]).collect();
            let mut data = Vec::new();
            let mut seed = None;
            for k in 0..rows.len() {
                let (my0, my1) = rows[k];
                if k > 0 {
                    for b in ring.iter_mut() {
                        b.fill(0);
                    }
                }
                let split = read(k, &mut data)?;
                let ext = geom.extents(my0);
                let mut planes: Vec<&mut [i16]> = ring.iter_mut().map(|b| &mut b[..]).collect();
                let g = fast::Geometry { rows: &held, width: &geom.width, v: &geom.v, ext: &ext, q: &geom.q, mcu_rows: my1 - my0 };
                let m = fast::decode_stripe(&data[..split], &data[split..], &mut planes, &g, seed.as_ref(), |my, planes| feed(my0, my0 + my, my0 + my + 1, &held, planes, &mut *sink))?;
                if k == 0 {
                    seed = Some(m.into_seed());
                }
            }
        }
        for ((e, s), p) in encs.into_iter().zip(&l.scans).zip(scans.iter_mut()) {
            if let ScanEnc::Prog(e) = e {
                let rest = e.finish(s)?;
                p.write(sink, &rest)?;
            } else if let ScanEnc::Seq(e) = e {
                e.finish();
            }
        }
        for (out, p) in outs.iter().zip(scans.iter_mut()) {
            p.write(sink, out)?;
            p.flush(sink)?;
        }
    }
    let (kind, tpos, cl, tl) = c.trailer;
    match kind {
        TRAILER_CM => {
            let mut first = [0u8; 1];
            inp.read_at(tpos, &mut first)?;
            if first[0] == 0 && cl == tl + 1 {
                // stored as it is (such as a video appended to the picture): copied piece by piece
                let mut buf = vec![0u8; (tl as usize).min(1 << 16)];
                let mut at = 0u64;
                while at < tl {
                    let n = (tl - at).min(buf.len() as u64) as usize;
                    inp.read_at(tpos + 1 + at, &mut buf[..n])?;
                    trailer.write(sink, &buf[..n])?;
                    at += n as u64;
                }
            } else {
                let mut coded = vec![0u8; cl as usize];
                inp.read_at(tpos, &mut coded)?;
                let t = fast::decode_bytes(&coded, tl as usize)?;
                drop(coded);
                trailer.write(sink, &t)?;
            }
            trailer.flush(sink)?;
        }
        TRAILER_FAST => {
            let (n, tcrc) = restore(&Sub { inner: inp, off: tpos, len: cl }, trailer.at, sink, threads, depth + 1)?;
            if n != tl {
                return Err(damaged());
            }
            trailer.at += n;
            trailer.crc = tcrc;
        }
        _ => return Err("corrupt .komla file".into()),
    }
    // the CRC of the whole file from the CRCs of its parts, in file order
    let mut order: Vec<&Part> = Vec::with_capacity(2 * scans.len() + 2);
    for (h, s) in heads.iter().zip(scans.iter()) {
        order.push(&h.0);
        order.push(s);
    }
    order.push(&heads[heads.len() - 1].0);
    order.push(&trailer);
    let mut total = 0u32;
    for p in order {
        if p.at != p.end {
            return Err(damaged());
        }
        total = crate::crc32_combine(total, p.crc, p.end - p.start);
    }
    if total != crc {
        return Err(damaged());
    }
    Ok((len, crc))
}

/// Decompresses any .komla JPEG container with up to `threads` threads.
pub fn decompress_with(b: &[u8], threads: usize) -> Result<Vec<u8>, String> {
    if !is_container(b) {
        return Err("not a Komla JPEG file".into());
    }
    match b.get(6) {
        Some(&MODE_FAST) | Some(&MODE_BYTES) => {
            let mut out = Vec::new();
            restore(&b, 0, &mut out, threads, 0)?;
            Ok(out)
        }
        _ => decompress(b, &mut |_| {}),
    }
}

/// Decompresses a .komla JPEG file into `out` without holding either file in memory (fast
/// mode; other modes are read whole). On error `out` may hold partial data.
#[cfg(unix)]
pub fn decompress_file(inp: &std::fs::File, out: &mut std::fs::File, threads: usize) -> Result<(), String> {
    let size = inp.metadata().map_err(|e| e.to_string())?.len();
    let inp = FileIn { file: inp, size };
    let mut head = [0u8; 7];
    inp.read_at(0, &mut head)?;
    if !is_container(&head) {
        return Err("not a Komla JPEG file".into());
    }
    if head[6] == MODE_FAST {
        restore(&inp, 0, out, threads, 0)?;
        return Ok(());
    }
    let mut b = vec![0u8; size as usize];
    inp.read_at(0, &mut b)?;
    let data = decompress_with(&b, threads)?;
    drop(b);
    out.put(0, &data)
}

/// Training: the summed symbol statistics of the fast model over `files` and its interior
/// prediction weights (see `fast::build_priors`). A first pass fits the weights, a second one
/// collects the statistics of the model that uses them.
pub fn fast_train(files: &[Vec<u8>]) -> (Vec<(usize, Vec<u64>)>, Vec<i16>) {
    fast::TRAIN.store(true, std::sync::atomic::Ordering::Relaxed);
    *fast::TRAIN_LSW.lock().unwrap() = Vec::new();
    let pass = |each: &mut dyn FnMut(&fast::Model)| {
        for data in files {
            let Ok(l) = format::parse(data, true) else { continue };
            let Ok((mut coefs, _)) = format::decode_scans(&l, data) else { continue };
            let rows = fast_stripes(&l.frame);
            let geom = StripeGeom::new(&l);
            for (k, mut p) in split_rows(&l.frame, &rows, &mut coefs).into_iter().enumerate() {
                let mut e = fast::Enc::<fast::Base>::new();
                let held: Vec<usize> = geom.v.iter().map(|&v| v * (rows[k].1 - rows[k].0)).collect();
                let Ok(m) = fast::code_stripe(&mut e, None, &mut p, &held, &geom.width, &geom.v, &geom.extents(rows[k].0), &geom.q, rows[k].1 - rows[k].0, |_, _| Ok(())) else { continue };
                each(&m);
            }
        }
    };
    let mut sums: Vec<f64> = Vec::new();
    pass(&mut |m| {
        let s = m.ls_sums();
        if sums.is_empty() {
            sums = vec![0.0; s.len()];
        }
        for (a, b) in sums.iter_mut().zip(s) {
            *a += b;
        }
    });
    let lsw = fast::solve_weights(&sums);
    *fast::TRAIN_LSW.lock().unwrap() = lsw.clone();
    let mut totals: Vec<(usize, Vec<u64>)> = Vec::new();
    pass(&mut |m| {
        let st = m.stats();
        if totals.is_empty() {
            totals = st.iter().map(|(n, c)| (*n, vec![0u64; c.len()])).collect();
        }
        for (t, (_, c)) in totals.iter_mut().zip(st) {
            for (a, b) in t.1.iter_mut().zip(c) {
                *a += b as u64;
            }
        }
    });
    fast::TRAIN.store(false, std::sync::atomic::Ordering::Relaxed);
    (totals, lsw)
}

/// Describes a .komla JPEG container of either mode without decoding the image; also returns
/// the original length and CRC-32.
pub fn summary(b: &[u8]) -> Result<(JpegInfo, usize, u32), String> {
    match b.get(6) {
        Some(&MODE_FAST) | Some(&MODE_BYTES) if is_container(b) && b[5] == VERSION => {
            let mut pos = 7;
            let len = usize::try_from(get_varint(b, &mut pos)?).map_err(|_| "corrupt .komla file")?;
            let crc = u32::from_le_bytes(take(b, &mut pos, 4)?.try_into().unwrap());
            let mut i = JpegInfo { mode: b[6], ..Default::default() };
            if b[6] == MODE_FAST {
                let c = parse_fast(&b, len as u64, pos as u64, 1, 0)?;
                let l = format::parse(&c.blob, false)?;
                let f = &l.frame;
                i.width = f.width as u32;
                i.height = f.height as u32;
                i.components = f.comps.len() as u8;
                i.progressive = f.progressive;
                i.sampling = sampling(f);
                i.stripes = c.stripes.len() as u32;
            }
            Ok((i, len, crc))
        }
        _ => {
            let c = parse_container(b)?;
            Ok((info(b)?, c.len, c.crc))
        }
    }
}

/// Describes a .komla JPEG container without decoding it.
pub fn info(b: &[u8]) -> Result<JpegInfo, String> {
    let c = parse_container(b)?;
    let mut i = JpegInfo { mode: c.mode, ..Default::default() };
    if c.mode == MODE_JPEG {
        let l = format::parse(&c.blob, false)?;
        let f = &l.frame;
        i.width = f.width as u32;
        i.height = f.height as u32;
        i.components = f.comps.len() as u8;
        i.progressive = f.progressive;
        i.sampling = sampling(f);
        i.stripes = c.stripes.len() as u32;
    }
    Ok(i)
}
