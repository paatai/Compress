use komla::coder::Encoder;
use komla::jpeg::{self, format};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Compresses, checks the mode, decompresses and requires the exact original bytes.
fn roundtrip(data: &[u8], expect_mode: u8) -> Vec<u8> {
    let (packed, info) = jpeg::compress(data, &mut |_| {});
    assert_eq!(info.mode, expect_mode, "unexpected mode ({})", info.note);
    let back = jpeg::decompress(&packed, &mut |_| {}).expect("decompress");
    assert!(back == data, "restored file differs from the original");
    packed
}

#[test]
fn fixtures_restore_byte_for_byte() {
    for name in ["tiny_420.jpg", "tiny_progressive_444.jpg", "tiny_gray_restart.jpg", "tiny_meta.jpg"] {
        let data = fixture(name);
        let packed = roundtrip(&data, jpeg::MODE_JPEG);
        assert!(packed.len() < data.len(), "{name}: {} -> {}", data.len(), packed.len());
    }
}

#[test]
fn jpeg_after_the_end_of_image_is_recompressed_too() {
    let mut data = fixture("tiny_420.jpg");
    let tail = fixture("tiny_progressive_444.jpg");
    data.extend_from_slice(&tail);
    let packed = roundtrip(&data, jpeg::MODE_JPEG);
    let (alone, _) = jpeg::compress(&fixture("tiny_420.jpg"), &mut |_| {});
    let (tail_alone, _) = jpeg::compress(&tail, &mut |_| {});
    assert!(packed.len() < alone.len() + tail_alone.len() + 32, "{} vs {} + {}", packed.len(), alone.len(), tail_alone.len());
}

#[test]
fn unsupported_or_damaged_input_is_stored() {
    let data = fixture("tiny_420.jpg");
    roundtrip(&data[..data.len() * 2 / 3], jpeg::MODE_CM);
    roundtrip(&[0xFF, 0xD8, 0xFF, 0xE0], jpeg::MODE_RAW);
    let mut s = 1u32;
    let noise: Vec<u8> = [0xFF, 0xD8, 0xFF].into_iter().chain((0..5000).map(|_| {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (s >> 24) as u8
    }))
    .collect();
    roundtrip(&noise, jpeg::MODE_RAW);
}

/// Damage anywhere must either be reported or leave the restored file exact (a range coder's
/// final flush bytes may not influence any decoded decision); it must never give wrong output.
#[test]
fn damaged_container_never_restores_wrong_bytes() {
    let data = fixture("tiny_progressive_444.jpg");
    let (packed, _) = jpeg::compress(&data, &mut |_| {});
    let mut detected = 0;
    for pos in 0..packed.len() {
        let mut bad = packed.clone();
        bad[pos] ^= 0x10;
        match jpeg::decompress(&bad, &mut |_| {}) {
            Ok(back) => assert!(back == data, "flipped byte {pos} gave a different file without an error"),
            Err(_) => detected += 1,
        }
    }
    assert!(detected >= packed.len() - 16, "only {detected} of {} damaged files were rejected", packed.len());
    assert!(jpeg::decompress(&packed[..packed.len() - 3], &mut |_| {}).is_err());
    assert!(jpeg::decompress(&packed[..8], &mut |_| {}).is_err());
    assert!(jpeg::decompress(b"KMLAJ", &mut |_| {}).is_err());
}

// Standard Huffman tables (ITU-T T.81 Annex K.3).
const DC_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const AC_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1,
    0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39,
    0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75,
    0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8,
    0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];

/// Builds a baseline grayscale JPEG with smooth pseudo-random coefficients, encoded by the
/// crate's own scan encoder with the standard tables.
fn synthetic_jpeg(w: usize, h: usize, seed: u32) -> Vec<u8> {
    let mut hdr = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00];
    hdr.extend((0..64).map(|k| (2 + k / 4) as u8));
    hdr.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 8, (h >> 8) as u8, h as u8, (w >> 8) as u8, w as u8, 1, 1, 0x11, 0]);
    hdr.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x1F, 0x00]);
    hdr.extend_from_slice(&DC_BITS);
    hdr.extend(0..12u8);
    hdr.extend_from_slice(&[0xFF, 0xC4, 0x00, 0xB5, 0x10]);
    hdr.extend_from_slice(&AC_BITS);
    hdr.extend_from_slice(&AC_VALS);
    hdr.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 1, 1, 0x00, 0, 63, 0]);
    let mut blob = hdr.clone();
    blob.extend_from_slice(&[0xFF, 0xD9]);
    let l = format::parse(&blob, false).unwrap();
    let c = &l.frame.comps[0];
    let mut coefs = vec![0i16; c.pw * c.ph * 64];
    let mut s = seed;
    let mut rnd = || {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (s >> 16) as i32
    };
    for by in 0..c.ph {
        for bx in 0..c.pw {
            let b = &mut coefs[(by * c.pw + bx) * 64..][..64];
            b[0] = ((bx as i32 * 7 + by as i32 * 5) % 200 - 100) as i16;
            for (k, &n) in format::NATURAL.iter().enumerate().skip(1) {
                let r = rnd();
                if r % (k as i32 + 2) == 0 {
                    b[n] = ((r >> 4) % 9 - 4) as i16;
                }
            }
        }
    }
    let mut e = Encoder::new();
    let mut counts = [[0u32; 2]; 8];
    let side = format::ScanSide::default();
    let mut sc = format::SideCoder::new(&mut e, Some(&side), &mut counts);
    let scan = format::encode_scan(&l.frame, &l.scans[0], &[coefs], &mut sc).unwrap();
    hdr.extend_from_slice(&scan);
    hdr.extend_from_slice(&[0xFF, 0xD9]);
    hdr
}

#[test]
fn synthetic_and_multi_stripe() {
    roundtrip(&synthetic_jpeg(100, 61, 3), jpeg::MODE_JPEG);
    let big = synthetic_jpeg(4096, 2400, 9);
    let l = format::parse(&big, true).unwrap();
    assert!(jpeg::stripe_rows(&l.frame).len() > 1);
    roundtrip(&big, jpeg::MODE_JPEG);
}

/// Fast mode: compresses with 1 and 4 threads (the output must not depend on the thread count),
/// decompresses with both and requires the exact original bytes.
fn roundtrip_fast(data: &[u8], expect_mode: u8) -> Vec<u8> {
    let (packed, info) = jpeg::compress_fast(data, 1);
    assert_eq!(info.mode, expect_mode, "unexpected mode ({})", info.note);
    let (packed4, _) = jpeg::compress_fast(data, 4);
    assert!(packed == packed4, "output depends on the number of threads");
    for threads in [1, 4] {
        let back = jpeg::decompress_with(&packed, threads).expect("decompress");
        assert!(back == data, "restored file differs from the original");
    }
    packed
}

#[test]
fn fast_fixtures_restore_byte_for_byte() {
    for name in ["tiny_420.jpg", "tiny_progressive_444.jpg", "tiny_gray_restart.jpg", "tiny_meta.jpg"] {
        let data = fixture(name);
        let packed = roundtrip_fast(&data, jpeg::MODE_FAST);
        assert!(packed.len() < data.len(), "{name}: {} -> {}", data.len(), packed.len());
    }
}

#[test]
fn fast_jpeg_after_the_end_of_image() {
    let mut data = fixture("tiny_420.jpg");
    data.extend_from_slice(&fixture("tiny_progressive_444.jpg"));
    roundtrip_fast(&data, jpeg::MODE_FAST);
    let mut data = fixture("tiny_meta.jpg");
    data.extend_from_slice(b"some bytes after the image");
    roundtrip_fast(&data, jpeg::MODE_FAST);
}

#[test]
fn fast_unsupported_or_damaged_input_is_stored() {
    let data = fixture("tiny_420.jpg");
    roundtrip_fast(&data[..data.len() * 2 / 3], jpeg::MODE_BYTES);
    roundtrip_fast(&[0xFF, 0xD8, 0xFF, 0xE0], jpeg::MODE_RAW);
    let mut s = 7u32;
    let noise: Vec<u8> = [0xFF, 0xD8, 0xFF].into_iter().chain((0..5000).map(|_| {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (s >> 24) as u8
    }))
    .collect();
    roundtrip_fast(&noise, jpeg::MODE_RAW);
}

#[test]
fn fast_damaged_container_never_restores_wrong_bytes() {
    for name in ["tiny_progressive_444.jpg", "tiny_420.jpg"] {
        let data = fixture(name);
        let (packed, _) = jpeg::compress_fast(&data, 1);
        let mut detected = 0;
        for pos in 0..packed.len() {
            for bit in [0x01, 0x10, 0x80] {
                let mut bad = packed.clone();
                bad[pos] ^= bit;
                match jpeg::decompress_with(&bad, 1) {
                    Ok(back) => assert!(back == data, "{name}: flipped byte {pos} gave a different file without an error"),
                    Err(_) => detected += 1,
                }
            }
        }
        assert!(detected >= 3 * packed.len() - 48, "{name}: only {detected} of {} damaged files were rejected", 3 * packed.len());
        for cut in [1, 3, packed.len() / 2] {
            assert!(jpeg::decompress_with(&packed[..packed.len() - cut], 1).is_err());
        }
    }
}

#[test]
fn fast_synthetic_and_multi_stripe() {
    roundtrip_fast(&synthetic_jpeg(100, 61, 3), jpeg::MODE_FAST);
    let big = synthetic_jpeg(4096, 2400, 9);
    let l = format::parse(&big, true).unwrap();
    assert!(jpeg::fast_stripes(&l.frame).len() > 4);
    roundtrip_fast(&big, jpeg::MODE_FAST);
}
