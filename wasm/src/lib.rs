//! Minimal C-ABI WebAssembly interface (no wasm-bindgen): JS allocates input
//! buffers with `kolmo_alloc`, calls an operation, then reads the result buffer
//! described by `kolmo_result_ptr` / `kolmo_result_len`.

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
                RESULT.clear();
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

/// Result layout: 16-byte info (width u32, height u32, channels u8, depth u8, source u8, pad u8, crc u32)
/// followed by the raw interleaved samples (8-bit: 1 byte, 16-bit: 2 bytes big-endian).
fn image_info(img: &kolmo::Image, src: io::SourceFormat) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + img.data.len() * 2);
    v.extend_from_slice(&img.width.to_le_bytes());
    v.extend_from_slice(&img.height.to_le_bytes());
    v.push(img.channels);
    v.push(img.depth);
    v.push(src as u8);
    v.push(0);
    let samples = img.sample_bytes();
    v.extend_from_slice(&kolmo::crc32(&samples).to_le_bytes());
    v.extend_from_slice(&samples);
    v
}

/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[no_mangle]
pub unsafe extern "C" fn kolmo_compress(ptr: *const u8, len: usize) -> i32 {
    let bytes = std::slice::from_raw_parts(ptr, len);
    set_result(io::read_image(bytes).and_then(|(img, src)| kolmo::compress(&img, src, &Options::default(), &mut progress)))
}

/// Decompresses a .kolmo file into a PNG file.
/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[no_mangle]
pub unsafe extern "C" fn kolmo_decompress_png(ptr: *const u8, len: usize) -> i32 {
    let bytes = std::slice::from_raw_parts(ptr, len);
    set_result(kolmo::decompress(bytes, &Options::default(), &mut progress).and_then(|(img, _)| io::write_png(&img)))
}

/// Decodes a PNG/BMP (or .kolmo) file to raw samples with an info header, for verification and hashing.
/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[no_mangle]
pub unsafe extern "C" fn kolmo_decode_pixels(ptr: *const u8, len: usize) -> i32 {
    let bytes = std::slice::from_raw_parts(ptr, len);
    let r = if bytes.starts_with(kolmo::MAGIC) {
        kolmo::decompress(bytes, &Options::default(), &mut progress).map(|(img, src)| image_info(&img, src))
    } else {
        io::read_image(bytes).map(|(img, src)| image_info(&img, src))
    };
    set_result(r)
}
