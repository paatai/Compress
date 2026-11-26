//! C-ABI WebAssembly interface (no wasm-bindgen). JS copies inputs into buffers from
//! `kolmo_alloc`, calls an operation (0 = ok, 1 = error), then reads the result buffer via
//! `kolmo_result_ptr/len` or the error message via `kolmo_error_ptr/len`.
//!
//! Stripes are independent, so the page runs `encode_stripe` / `decode_stripe` in several
//! workers at once and uses `prepare`, `parse` and `finish` around them.

use kolmo::{io, Options};

static mut RESULT: Vec<u8> = Vec::new();
static mut ERROR: Vec<u8> = Vec::new();

#[link(wasm_import_module = "env")]
extern "C" {
    fn kolmo_progress(fraction: f32);
}

fn progress(f: f32) {
    unsafe { kolmo_progress(f) }
}

#[allow(static_mut_refs)]
fn set_result(r: Result<Vec<u8>, String>) -> i32 {
    unsafe {
        match r {
            Ok(v) => {
                RESULT = v;
                ERROR.clear();
                0
            }
            Err(e) => {
                RESULT = Vec::new();
                ERROR = e.into_bytes();
                1
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn kolmo_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `ptr`/`len` must come from `kolmo_alloc`.
#[no_mangle]
pub unsafe extern "C" fn kolmo_free(ptr: *mut u8, len: usize) {
    drop(Vec::from_raw_parts(ptr, 0, len.max(1)));
}

#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn kolmo_result_ptr() -> *const u8 {
    unsafe { RESULT.as_ptr() }
}

#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn kolmo_result_len() -> usize {
    unsafe { RESULT.len() }
}

/// Frees the result buffer once JS has copied it.
#[no_mangle]
pub extern "C" fn kolmo_result_clear() {
    unsafe { RESULT = Vec::new() }
}

#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn kolmo_error_ptr() -> *const u8 {
    unsafe { ERROR.as_ptr() }
}

#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn kolmo_error_len() -> usize {
    unsafe { ERROR.len() }
}

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn info_block(v: &mut Vec<u8>, info: &kolmo::StreamInfo) {
    put32(v, info.width);
    put32(v, info.height);
    v.push(info.channels);
    v.push(info.depth);
    v.push(info.coded_channels);
    v.push(info.source as u8);
    put32(v, info.mode as u32);
    put32(v, info.palette.as_ref().map_or(0, |p| p.len() as u32));
}

unsafe fn slice<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len)
    }
}

fn u16s(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

/// Input: PNG/BMP file. Result:
///   u32 header_len, header, u32 n_stripes, n x (u32 y0, u32 y1), info block (20 bytes),
///   u32 sample_bytes_len, original sample bytes, coded samples (u16 LE).
/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[no_mangle]
pub unsafe extern "C" fn kolmo_prepare(ptr: *const u8, len: usize) -> i32 {
    let r = io::read_image(slice(ptr, len)).and_then(|(img, src)| {
        let prep = kolmo::prepare(&img, src)?;
        let stripes = prep.info.stripes();
        let samples = img.sample_bytes();
        drop(img);
        let mut v = Vec::with_capacity(64 + prep.header.len() + samples.len() + prep.coded.data.len() * 2);
        put32(&mut v, prep.header.len() as u32);
        v.extend_from_slice(&prep.header);
        put32(&mut v, stripes.len() as u32);
        for (a, b) in &stripes {
            put32(&mut v, *a as u32);
            put32(&mut v, *b as u32);
        }
        info_block(&mut v, &prep.info);
        put32(&mut v, samples.len() as u32);
        v.extend_from_slice(&samples);
        for s in &prep.coded.data {
            v.extend_from_slice(&s.to_le_bytes());
        }
        Ok(v)
    });
    set_result(r)
}

/// Codes one stripe: `header` is the fixed header from `kolmo_prepare`, `rows` the coded
/// samples (u16 LE) of the stripe. Result: the stripe stream.
/// # Safety
/// Pointers must describe readable buffers.
#[no_mangle]
pub unsafe extern "C" fn kolmo_encode_stripe(hp: *const u8, hl: usize, rp: *const u8, rl: usize, height: u32) -> i32 {
    let r = kolmo::parse_header(slice(hp, hl)).map(|(info, _)| {
        let rows = u16s(slice(rp, rl));
        kolmo::encode_stripe(&info, &rows, height as usize, &Options::default().tuning, &mut progress)
    });
    set_result(r)
}

/// Input: .kolmo file. Result: u32 header_len, u32 n_stripes, n x (u32 offset, u32 len, u32 y0, u32 y1), info block.
/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[no_mangle]
pub unsafe extern "C" fn kolmo_parse(ptr: *const u8, len: usize) -> i32 {
    let b = slice(ptr, len);
    let r = kolmo::parse_header(b).and_then(|(info, hl)| {
        let parts = kolmo::split_stripes(b, &info, hl)?;
        let rows = info.stripes();
        let mut v = Vec::new();
        put32(&mut v, hl as u32);
        put32(&mut v, parts.len() as u32);
        for (p, (y0, y1)) in parts.iter().zip(rows.iter()) {
            put32(&mut v, (p.as_ptr() as usize - b.as_ptr() as usize) as u32);
            put32(&mut v, p.len() as u32);
            put32(&mut v, *y0 as u32);
            put32(&mut v, *y1 as u32);
        }
        info_block(&mut v, &info);
        Ok(v)
    });
    set_result(r)
}

/// Decodes one stripe stream. Result: coded samples (u16 LE).
/// # Safety
/// Pointers must describe readable buffers.
#[no_mangle]
pub unsafe extern "C" fn kolmo_decode_stripe(hp: *const u8, hl: usize, dp: *const u8, dl: usize, height: u32) -> i32 {
    let r = kolmo::parse_header(slice(hp, hl)).map(|(info, _)| {
        let rows = kolmo::decode_stripe(&info, slice(dp, dl), height as usize, &Options::default().tuning, &mut progress);
        rows.iter().flat_map(|v| v.to_le_bytes()).collect()
    });
    set_result(r)
}

/// Rebuilds the image from all decoded coded samples and verifies its CRC.
/// Result: u32 png_len, PNG file (empty unless `want_png`), original sample bytes.
/// # Safety
/// Pointers must describe readable buffers.
#[no_mangle]
pub unsafe extern "C" fn kolmo_finish(hp: *const u8, hl: usize, cp: *const u8, cl: usize, want_png: u32) -> i32 {
    let r = kolmo::parse_header(slice(hp, hl)).and_then(|(info, _)| {
        let img = kolmo::finish(&info, u16s(slice(cp, cl)))?;
        let png = if want_png != 0 { io::write_png(&img)? } else { Vec::new() };
        let samples = img.sample_bytes();
        let mut v = Vec::with_capacity(4 + png.len() + samples.len());
        put32(&mut v, png.len() as u32);
        v.extend_from_slice(&png);
        v.extend_from_slice(&samples);
        Ok(v)
    });
    set_result(r)
}

/// Decodes a PNG/BMP file. Result: info (u32 w, u32 h, u8 channels, u8 depth, u8 src, u8 0), sample bytes.
/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[no_mangle]
pub unsafe extern "C" fn kolmo_read_image(ptr: *const u8, len: usize) -> i32 {
    let r = io::read_image(slice(ptr, len)).map(|(img, src)| {
        let mut v = Vec::new();
        put32(&mut v, img.width);
        put32(&mut v, img.height);
        v.extend_from_slice(&[img.channels, img.depth, src as u8, 0]);
        v.extend_from_slice(&img.sample_bytes());
        v
    });
    set_result(r)
}

/// Whole-file convenience entry points (single-threaded).
/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[no_mangle]
pub unsafe extern "C" fn kolmo_compress(ptr: *const u8, len: usize) -> i32 {
    let bytes = slice(ptr, len);
    set_result(io::read_image(bytes).and_then(|(img, src)| kolmo::compress(&img, src, &Options::default(), &mut progress)))
}

/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[no_mangle]
pub unsafe extern "C" fn kolmo_decompress_png(ptr: *const u8, len: usize) -> i32 {
    let bytes = slice(ptr, len);
    set_result(kolmo::decompress(bytes, &Options::default(), &mut progress).and_then(|(img, _)| io::write_png(&img)))
}

/// Decodes a PNG/BMP or .kolmo file and returns u32 w, u32 h, u8 channels, u8 depth, u8 src, u8 0, sample bytes.
/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[no_mangle]
pub unsafe extern "C" fn kolmo_decode_pixels(ptr: *const u8, len: usize) -> i32 {
    let bytes = slice(ptr, len);
    let r = if bytes.starts_with(kolmo::MAGIC) {
        kolmo::decompress(bytes, &Options::default(), &mut progress)
    } else {
        io::read_image(bytes)
    };
    set_result(r.map(|(img, src)| {
        let mut v = Vec::new();
        put32(&mut v, img.width);
        put32(&mut v, img.height);
        v.extend_from_slice(&[img.channels, img.depth, src as u8, 0]);
        v.extend_from_slice(&img.sample_bytes());
        v
    }))
}
