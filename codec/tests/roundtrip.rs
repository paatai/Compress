use komla::{compress, decompress, Image, Options, SourceFormat};

fn lcg(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state >> 8
}

fn synth(w: u32, h: u32, channels: u8, depth: u8, levels: u32, seed: u32) -> Image {
    let maxv = if depth == 16 { 65535u32 } else { 255 };
    let mut s = seed;
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            for c in 0..channels as u32 {
                let base = (x * 3 + y * 5 + c * 40) % (levels.max(2) * 37);
                let noise = lcg(&mut s) % 5;
                let v = if levels > 0 { (base / 37) * (maxv / levels.max(1)) } else { (base * maxv / 400 + noise * (maxv / 255)).min(maxv) };
                data.push(v.min(maxv) as u16);
            }
        }
    }
    Image { width: w, height: h, channels, depth, data }
}

fn roundtrip(img: &Image) -> usize {
    let opts = Options::default();
    let packed = compress(img, SourceFormat::Png, &opts, &mut |_| {}).expect("compress");
    let (back, _) = decompress(&packed, &opts, &mut |_| {}).expect("decompress");
    assert_eq!(&back, img, "round trip changed pixels");
    packed.len()
}

#[test]
fn continuous_tone_rgb_and_gray() {
    roundtrip(&synth(61, 47, 3, 8, 0, 1));
    roundtrip(&synth(33, 29, 1, 8, 0, 2));
    roundtrip(&synth(40, 31, 2, 8, 0, 3));
    roundtrip(&synth(25, 19, 4, 8, 0, 4));
}

#[test]
fn sixteen_bit() {
    roundtrip(&synth(37, 23, 3, 16, 0, 5));
    roundtrip(&synth(21, 17, 1, 16, 0, 6));
    roundtrip(&synth(19, 13, 4, 16, 0, 7));
}

#[test]
fn palette_and_flags() {
    roundtrip(&synth(70, 50, 3, 8, 6, 8));
    roundtrip(&synth(64, 40, 4, 8, 3, 9));
    roundtrip(&synth(90, 30, 3, 16, 5, 10));
}

#[test]
fn tiny_images() {
    for (w, h) in [(1, 1), (1, 7), (9, 1), (2, 2)] {
        roundtrip(&synth(w, h, 3, 8, 0, w * 31 + h));
        roundtrip(&synth(w, h, 4, 8, 2, w * 17 + h));
    }
}

#[test]
fn stripes() {
    // tall enough that the stripe rule splits it
    let img = synth(8, (komla::STRIPE_PIXELS / 8 + 100) as u32, 1, 8, 4, 11);
    assert!(komla::stripe_rows(img.width as usize, img.height as usize).len() > 1);
    roundtrip(&img);
}

#[test]
fn corrupt_input_is_rejected() {
    let img = synth(30, 20, 3, 8, 0, 12);
    let opts = Options::default();
    let mut packed = compress(&img, SourceFormat::Png, &opts, &mut |_| {}).unwrap();
    let n = packed.len();
    packed[n / 2] ^= 0x55;
    assert!(decompress(&packed, &opts, &mut |_| {}).is_err());
    assert!(decompress(b"KMLA", &opts, &mut |_| {}).is_err());
    assert!(decompress(b"not a komla file", &opts, &mut |_| {}).is_err());
}
