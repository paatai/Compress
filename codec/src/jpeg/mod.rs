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
//   varint header-blob length | varint coded length | header blob (`fast::encode_bytes`)
//   varint trailer length | trailer kind | varint coded length | coded trailer
//   varint side-info length | side info (per scan: padding, garbage, end-of-band choices)
//   varint stripe count | per stripe: varint rANS length, varint raw length | stripe data
//
// The side info can be decoded before any scan, so scans and stripes are independent: stripes
// are coded and decoded in parallel, straight from and into the coefficient planes.

/// Stripes of the fast mode hold about this many blocks (all components).
pub const FAST_STRIPE_BLOCKS: usize = 1 << 15;
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

/// Runs `f` on every item with up to `threads` threads; results in item order.
fn par_map<T: Send, R: Send>(items: Vec<T>, threads: usize, f: impl Fn(usize, T) -> R + Sync) -> Vec<R> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = items.len();
        if threads > 1 && n > 1 {
            use std::sync::atomic::{AtomicUsize, Ordering};
            use std::sync::Mutex;
            let slots: Vec<Mutex<Option<T>>> = items.into_iter().map(|t| Mutex::new(Some(t))).collect();
            let out: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
            let next = AtomicUsize::new(0);
            std::thread::scope(|s| {
                for _ in 0..threads.min(n) {
                    s.spawn(|| loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        let t = slots[i].lock().unwrap().take().unwrap();
                        let r = f(i, t);
                        *out[i].lock().unwrap() = Some(r);
                    });
                }
            });
            return out.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect();
        }
    }
    let _ = threads;
    items.into_iter().enumerate().map(|(i, t)| f(i, t)).collect()
}

/// MCU rows of each fast-mode stripe.
pub fn fast_stripes(f: &Frame) -> Vec<(usize, usize)> {
    let per_row: usize = f.comps.iter().map(|c| c.pw * c.v).sum();
    let target = std::env::var("KOMLA_STRIPE").ok().and_then(|s| s.parse().ok()).unwrap_or(FAST_STRIPE_BLOCKS);
    let n = (per_row * f.mcuy).div_ceil(target).clamp(1, f.mcuy.clamp(1, 256));
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
    fn encode(&self, rows: (usize, usize), planes: &mut [&mut [i16]]) -> (Vec<u8>, Vec<u8>) {
        let mut e = fast::Enc::new();
        fast::code_stripe(&mut e, planes, &self.width, &self.v, &self.extents(rows.0), &self.q, rows.1 - rows.0);
        e.finish()
    }
    fn decode(&self, rows: (usize, usize), rans: &[u8], raw: &[u8], planes: &mut [&mut [i16]]) -> Result<(), String> {
        let mut d = fast::Dec::new(rans, raw);
        fast::code_stripe(&mut d, planes, &self.width, &self.v, &self.extents(rows.0), &self.q, rows.1 - rows.0);
        if d.overrun() {
            return Err("corrupt .komla file".into());
        }
        Ok(())
    }
}

fn put_side(out: &mut Vec<u8>, s: &huff::Side) {
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
}

fn get_side(b: &[u8], pos: &mut usize, limit: usize) -> Result<huff::Side, String> {
    let mut s = huff::Side::default();
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
    Ok(s)
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
    match analyze_fast(data, threads) {
        Ok(r) => r,
        Err(e) => stored_fast(data, e),
    }
}

fn analyze_fast(data: &[u8], threads: usize) -> Result<(Vec<u8>, JpegInfo), String> {
    if data.len() > u32::MAX as usize {
        return Err("file too large".into());
    }
    let l = format::parse(data, true)?;
    let (mut coefs, sides) = format::decode_scans(&l, data)?;
    let f = &l.frame;
    // regenerate every scan from the coefficients, recording the side info that reproduces it
    let side = {
        let planes: Vec<&[i16]> = coefs.iter().map(|p| &p[..]).collect();
        let jobs: Vec<usize> = (0..l.scans.len()).collect();
        let res = par_map(jobs, threads, |_, i| -> Result<huff::Side, String> {
            let s = &l.scans[i];
            let mut ch = huff::Choices::record(&sides[i]);
            let mut out = Vec::with_capacity(s.seg.1 - s.seg.0 + 64);
            huff::encode_scan(f, s, &planes, &mut ch, &mut out)?;
            if out[..] != data[s.seg.0..s.seg.1] {
                return Err("scan data cannot be reproduced exactly from its coefficients".into());
            }
            Ok(ch.side)
        });
        let mut side = Vec::new();
        for r in res {
            put_side(&mut side, &r?);
        }
        side
    };
    let rows = fast_stripes(f);
    let geom = StripeGeom::new(&l);
    let streams = {
        let parts = split_rows(f, &rows, &mut coefs);
        par_map(parts, threads, |k, mut p| geom.encode(rows[k], &mut p))
    };
    drop(coefs);
    let trailer = &data[l.end..];
    let mut h = Vec::with_capacity(4096 + streams.iter().map(|s| s.0.len() + s.1.len()).sum::<usize>());
    h.extend_from_slice(crate::MAGIC);
    h.push(KIND);
    h.push(VERSION);
    h.push(MODE_FAST);
    put_varint(&mut h, data.len() as u64);
    h.extend_from_slice(&crate::crc32(data).to_le_bytes());
    let c = fast::encode_bytes(&l.blob);
    put_varint(&mut h, l.blob.len() as u64);
    put_varint(&mut h, c.len() as u64);
    h.extend_from_slice(&c);
    put_varint(&mut h, trailer.len() as u64);
    let (kind, c) = match is_jpeg(trailer).then(|| compress_fast(trailer, threads)) {
        Some((nested, info)) if info.mode == MODE_FAST && nested.len() < trailer.len() => (TRAILER_FAST, nested),
        _ => (TRAILER_CM, fast::encode_bytes(trailer)),
    };
    h.push(kind);
    put_varint(&mut h, c.len() as u64);
    h.extend_from_slice(&c);
    put_varint(&mut h, side.len() as u64);
    h.extend_from_slice(&side);
    put_varint(&mut h, streams.len() as u64);
    for (r, w) in &streams {
        put_varint(&mut h, r.len() as u64);
        put_varint(&mut h, w.len() as u64);
    }
    for (r, w) in &streams {
        h.extend_from_slice(r);
        h.extend_from_slice(w);
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

/// The parts of a fast-mode container.
struct FastContainer<'a> {
    len: usize,
    crc: u32,
    blob: Vec<u8>,
    trailer: (u8, &'a [u8], usize),
    side: &'a [u8],
    stripes: Vec<(&'a [u8], &'a [u8])>,
}

fn parse_fast(b: &[u8], len: usize, crc: u32, mut pos: usize) -> Result<FastContainer<'_>, String> {
    let n = get_varint(b, &mut pos)?;
    let cl = get_varint(b, &mut pos)?;
    if n > len as u64 {
        return Err("corrupt .komla file".into());
    }
    let blob = fast::decode_bytes(take(b, &mut pos, cl)?, n as usize)?;
    let tl = get_varint(b, &mut pos)?;
    if tl > len as u64 {
        return Err("corrupt .komla file".into());
    }
    let kind = *b.get(pos).ok_or("truncated .komla file")?;
    pos += 1;
    let cl = get_varint(b, &mut pos)?;
    let trailer = (kind, take(b, &mut pos, cl)?, tl as usize);
    let sl = get_varint(b, &mut pos)?;
    let side = take(b, &mut pos, sl)?;
    let n = get_varint(b, &mut pos)? as usize;
    if n == 0 || n > 256 {
        return Err("corrupt stripe table".into());
    }
    let mut lens = Vec::with_capacity(n);
    for _ in 0..n {
        lens.push((get_varint(b, &mut pos)?, get_varint(b, &mut pos)?));
    }
    let mut stripes = Vec::with_capacity(n);
    for (r, w) in lens {
        let r = take(b, &mut pos, r)?;
        let w = take(b, &mut pos, w)?;
        stripes.push((r, w));
    }
    Ok(FastContainer { len, crc, blob, trailer, side, stripes })
}

fn decompress_fast(c: FastContainer, threads: usize) -> Result<Vec<u8>, String> {
    let l = format::parse(&c.blob, false)?;
    let f = &l.frame;
    let mut pos = 0;
    let mut sides = Vec::with_capacity(l.scans.len());
    for _ in &l.scans {
        sides.push(get_side(c.side, &mut pos, c.len)?);
    }
    let rows = fast_stripes(f);
    if rows.len() != c.stripes.len() {
        return Err("corrupt stripe table".into());
    }
    let geom = StripeGeom::new(&l);
    let mut coefs: Vec<Vec<i16>> = f.comps.iter().map(|comp| vec![0i16; comp.pw * comp.ph * 64]).collect();
    {
        let parts = split_rows(f, &rows, &mut coefs);
        let res = par_map(parts, threads, |k, mut p| geom.decode(rows[k], c.stripes[k].0, c.stripes[k].1, &mut p));
        for r in res {
            r?;
        }
    }
    let planes: Vec<&[i16]> = coefs.iter().map(|p| &p[..]).collect();
    let mut out = Vec::with_capacity(c.len + 64);
    let mut bpos = 0;
    for (s, side) in l.scans.iter().zip(sides) {
        out.extend_from_slice(&c.blob[bpos..s.blob_pos]);
        bpos = s.blob_pos;
        let mut ch = huff::Choices::replay(side);
        huff::encode_scan(f, s, &planes, &mut ch, &mut out)?;
        if out.len() > c.len {
            return Err("checksum mismatch: the .komla file is damaged or incomplete".into());
        }
    }
    out.extend_from_slice(&c.blob[bpos..]);
    drop(coefs);
    let (kind, coded, n) = c.trailer;
    let t = match kind {
        TRAILER_CM => fast::decode_bytes(coded, n)?,
        TRAILER_FAST => decompress_with(coded, threads)?,
        _ => return Err("corrupt .komla file".into()),
    };
    if t.len() != n {
        return Err("checksum mismatch: the .komla file is damaged or incomplete".into());
    }
    out.extend_from_slice(&t);
    if out.len() != c.len || crate::crc32(&out) != c.crc {
        return Err("checksum mismatch: the .komla file is damaged or incomplete".into());
    }
    Ok(out)
}

/// Decompresses any .komla JPEG container with up to `threads` threads.
pub fn decompress_with(b: &[u8], threads: usize) -> Result<Vec<u8>, String> {
    if !is_container(b) {
        return Err("not a Komla JPEG file".into());
    }
    if b[5] != VERSION {
        return Err(format!("unsupported Komla JPEG version {}", b[5]));
    }
    match b.get(6) {
        Some(&MODE_FAST) | Some(&MODE_BYTES) => {
            let mut pos = 7;
            let len = usize::try_from(get_varint(b, &mut pos)?).map_err(|_| "corrupt .komla file")?;
            let crc = u32::from_le_bytes(take(b, &mut pos, 4)?.try_into().unwrap());
            if b[6] == MODE_BYTES {
                let out = fast::decode_bytes(&b[pos..], len)?;
                if out.len() != len || crate::crc32(&out) != crc {
                    return Err("checksum mismatch: the .komla file is damaged or incomplete".into());
                }
                return Ok(out);
            }
            decompress_fast(parse_fast(b, len, crc, pos)?, threads)
        }
        _ => decompress(b, &mut |_| {}),
    }
}

/// Experiment: codes all coefficients of a JPEG with the fast model in one stripe, decodes them
/// again and checks they match. Returns (arithmetic bytes, raw bytes, seconds for Huffman
/// decoding, model encoding, model decoding).
pub fn fast_experiment(data: &[u8]) -> Result<(usize, usize, f64, f64, f64), String> {
    let t0 = std::time::Instant::now();
    let l = format::parse(data, true)?;
    let (mut coefs, _) = format::decode_scans(&l, data)?;
    let t_huff = t0.elapsed().as_secs_f64();
    let f = &l.frame;
    let cov = coverage(&l);
    let ext: Vec<fast::Extent> = cov.iter().map(|c| fast::Extent { dc: c.0, ac: c.1, dc_w: c.2 .0, dc_h: c.2 .1, ac_w: c.3 .0, ac_h: c.3 .1 }).collect();
    let width: Vec<usize> = f.comps.iter().map(|c| c.pw).collect();
    let v: Vec<usize> = f.comps.iter().map(|c| c.v).collect();
    let q: Vec<[u16; 64]> = f.comps.iter().map(|c| c.q).collect();
    let t1 = std::time::Instant::now();
    let mut e = fast::Enc::new();
    {
        let mut planes: Vec<&mut [i16]> = coefs.iter_mut().map(|p| &mut p[..]).collect();
        fast::code_stripe(&mut e, &mut planes, &width, &v, &ext, &q, f.mcuy);
    }
    if std::env::var_os("KOMLA_COUNT").is_some() {
        let c = unsafe { fast::COST };
        let t: f64 = c.iter().sum();
        eprintln!("symbols {} blocks {} | nz {:.1}% zero {:.1}% mag {:.1}% rawac {:.1}% dc {:.1}% rawdc {:.1}% edges {:.1}% | total {:.0} bytes", e.symbols(), coefs.iter().map(|p| p.len() / 64).sum::<usize>(), 100.0*c[0]/t, 100.0*c[1]/t, 100.0*c[2]/t, 100.0*c[3]/t, 100.0*c[4]/t, 100.0*c[5]/t, 100.0*c[6]/t, t/8.0);
        unsafe { fast::COST = [0.0; 8] };
    }
    let (arith, raw) = e.finish();
    let t_enc = t1.elapsed().as_secs_f64();
    let t2 = std::time::Instant::now();
    let mut dec: Vec<Vec<i16>> = coefs.iter().map(|p| vec![0i16; p.len()]).collect();
    {
        let mut d = fast::Dec::new(&arith, &raw);
        let mut planes: Vec<&mut [i16]> = dec.iter_mut().map(|p| &mut p[..]).collect();
        fast::code_stripe(&mut d, &mut planes, &width, &v, &ext, &q, f.mcuy);
    }
    let t_dec = t2.elapsed().as_secs_f64();
    if dec != coefs {
        return Err("fast model round trip mismatch".into());
    }
    Ok((arith.len(), raw.len(), t_huff, t_enc, t_dec))
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
