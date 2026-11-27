use komla::{ct::Tuning, io, jpeg, Options};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

const USAGE: &str = "\
Komla - lossless image compression by context mixing

USAGE:
  komla compress   <input.png|input.bmp|input.jpg> [-o output.komla]
  komla decompress <input.komla> [-o output.png|output.bmp|output.jpg]
  komla info       <input.komla>
  komla bench      [-j threads] [--no-verify] <images...>
  komla pixhash    <images...>   (SHA-256 of pixels normalised to RGBA)

compress writes <input>.komla by default; decompress restores the original
file type (PNG or BMP) unless -o names another extension. Decompression checks
the embedded CRC-32 of the pixels and fails loudly on any mismatch.

JPEG files are recompressed losslessly: decompress gives back the original file
byte for byte (checked with a CRC-32 of the whole file). JPEGs that cannot be
reproduced exactly from their coefficients are stored as they are.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        println!("{USAGE}");
        return;
    }
    let res = match args[0].as_str() {
        "compress" | "c" => cmd_compress(&args[1..]),
        "decompress" | "d" => cmd_decompress(&args[1..]),
        "info" => cmd_info(&args[1..]),
        "bench" => cmd_bench(&args[1..]),
        "pixhash" => cmd_pixhash(&args[1..]),
        "jfast" => cmd_jfast(&args[1..]),
        "--version" | "-V" => {
            println!("komla {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn options() -> Options {
    let mut t = Tuning::default();
    if let Ok(s) = std::env::var("KOMLA_TUNE") {
        for kv in s.split(',').filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once('=').expect("KOMLA_TUNE expects key=value pairs");
            let f: f64 = v.parse().expect("numeric tuning value");
            match k {
                "mixer_lr" => t.mixer_lr = f as i32,
                "mixer_lr_final" => t.mixer_lr_final = f as i32,
                "ctr_limit" => t.ctr_limit = f as u32,
                "ctr_limit_exact" => t.ctr_limit_exact = f as u32,
                "blend_eps" => t.blend_eps = f as f32,
                "apm_rate" => t.apm_rate = f as u32,
                "wls_lh" => t.wls_lh = f,
                "wls_lv" => t.wls_lv = f,
                "wls_nu" => t.wls_nu = f,
                "wls_interval" => t.wls_interval = f as u32,
                "wls_interval_ref" => t.wls_interval_ref = f as u32,
                "stack_lambda" => t.stack_lambda = f,
                "stack_nu" => t.stack_nu = f,
                "stack_interval" => t.stack_interval = f as u32,
                "vw_c" => t.vw_c = f,
                _ => panic!("unknown tuning key {k}"),
            }
        }
    }
    Options { tuning: t }
}

fn split_out(args: &[String]) -> Result<(PathBuf, Option<PathBuf>), String> {
    let mut input = None;
    let mut output = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-o" {
            output = Some(PathBuf::from(args.get(i + 1).ok_or("-o needs a path")?));
            i += 2;
        } else {
            input = Some(PathBuf::from(&args[i]));
            i += 1;
        }
    }
    Ok((input.ok_or("missing input file")?, output))
}

fn read(p: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn jpeg_mode(info: &jpeg::JpegInfo) -> String {
    match info.mode {
        jpeg::MODE_JPEG => format!(
            "{} x {} {} {}JPEG, {} stripe(s)",
            info.width,
            info.height,
            info.sampling,
            if info.progressive { "progressive " } else { "" },
            info.stripes
        ),
        _ => format!("stored ({})", if info.note.is_empty() { "not recompressed" } else { &info.note }),
    }
}

fn compress_jpeg(input: &Path, output: Option<PathBuf>, bytes: &[u8]) -> Result<(), String> {
    let t0 = Instant::now();
    let (out, info) = jpeg::compress(bytes, &mut |_| {});
    let dt = t0.elapsed().as_secs_f64();
    let output = output.unwrap_or_else(|| {
        let mut s = input.as_os_str().to_owned();
        s.push(".komla");
        PathBuf::from(s)
    });
    std::fs::write(&output, &out).map_err(|e| format!("{}: {e}", output.display()))?;
    println!(
        "{} -> {}: {}, {} -> {} bytes ({:.2}% smaller) in {:.2}s",
        input.display(),
        output.display(),
        jpeg_mode(&info),
        bytes.len(),
        out.len(),
        100.0 * (1.0 - out.len() as f64 / bytes.len() as f64),
        dt
    );
    Ok(())
}

fn decompress_jpeg(input: &Path, output: Option<PathBuf>, bytes: &[u8]) -> Result<(), String> {
    let t0 = Instant::now();
    let out = jpeg::decompress(bytes, &mut |_| {})?;
    let dt = t0.elapsed().as_secs_f64();
    let output = output.unwrap_or_else(|| {
        let stem = input.to_string_lossy();
        PathBuf::from(stem.strip_suffix(".komla").map(str::to_string).unwrap_or_else(|| format!("{stem}.jpg")))
    });
    std::fs::write(&output, &out).map_err(|e| format!("{}: {e}", output.display()))?;
    println!("{} -> {}: {} bytes, checksum OK, in {:.2}s", input.display(), output.display(), out.len(), dt);
    Ok(())
}

fn cmd_compress(args: &[String]) -> Result<(), String> {
    let (input, output) = split_out(args)?;
    let bytes = read(&input)?;
    if jpeg::is_jpeg(&bytes) {
        return compress_jpeg(&input, output, &bytes);
    }
    let (img, src) = io::read_image(&bytes)?;
    let t0 = Instant::now();
    let out = komla::compress(&img, src, &options(), &mut |_| {})?;
    let dt = t0.elapsed().as_secs_f64();
    let output = output.unwrap_or_else(|| {
        let mut s = input.as_os_str().to_owned();
        s.push(".komla");
        PathBuf::from(s)
    });
    std::fs::write(&output, &out).map_err(|e| format!("{}: {e}", output.display()))?;
    println!(
        "{} -> {}: {} x {} x {}ch {}-bit, {} -> {} bytes ({:.3} bpp, {:.1}% of input) in {:.2}s",
        input.display(),
        output.display(),
        img.width,
        img.height,
        img.channels,
        img.depth,
        bytes.len(),
        out.len(),
        out.len() as f64 * 8.0 / img.pixels() as f64,
        100.0 * out.len() as f64 / bytes.len() as f64,
        dt
    );
    Ok(())
}

fn cmd_decompress(args: &[String]) -> Result<(), String> {
    let (input, output) = split_out(args)?;
    let bytes = read(&input)?;
    if jpeg::is_container(&bytes) {
        return decompress_jpeg(&input, output, &bytes);
    }
    let t0 = Instant::now();
    let (img, src) = komla::decompress(&bytes, &options(), &mut |_| {})?;
    let dt = t0.elapsed().as_secs_f64();
    let output = output.unwrap_or_else(|| {
        let stem = input.to_string_lossy();
        let stem = stem.strip_suffix(".komla").map(str::to_string).unwrap_or_else(|| {
            format!("{}.{}", stem, if src == io::SourceFormat::Bmp { "bmp" } else { "png" })
        });
        PathBuf::from(stem)
    });
    let ext = output.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let data = if ext == "bmp" { io::write_bmp(&img)? } else { io::write_png(&img)? };
    std::fs::write(&output, &data).map_err(|e| format!("{}: {e}", output.display()))?;
    println!(
        "{} -> {}: {} x {} x {}ch {}-bit, checksum OK, in {:.2}s",
        input.display(),
        output.display(),
        img.width,
        img.height,
        img.channels,
        img.depth,
        dt
    );
    Ok(())
}

/// Development aid: fast-model size and stage timings for JPEG files.
fn cmd_jfast(args: &[String]) -> Result<(), String> {
    let (mut tin, mut tout, mut th, mut te, mut td, mut traw) = (0usize, 0usize, 0f64, 0f64, 0f64, 0usize);
    for a in args {
        let bytes = read(Path::new(a))?;
        let blob = jpeg::format::parse(&bytes, true).map(|l| jpeg::bytecm::encode(&l.blob).len()).unwrap_or(0);
        match jpeg::fast_experiment(&bytes) {
            Ok((ar, raw, h, e, d)) => {
                let total = ar + raw + blob;
                println!("{a}\t{}\t{ar}\t{raw}\t{blob}\t{:.2}%\thuff {:.1} ms\tenc {:.1} ms\tdec {:.1} ms", bytes.len(), 100.0 * (1.0 - total as f64 / bytes.len() as f64), h * 1e3, e * 1e3, d * 1e3);
                tin += bytes.len();
                tout += total;
                traw += raw;
                th += h;
                te += e;
                td += d;
            }
            Err(e) => println!("{a}\terror: {e}"),
        }
    }
    let mb = tin as f64 / 1e6;
    println!(
        "TOTAL\t{tin}\t{tout}\t{:.3}% smaller\traw {:.1}% of output\thuffman decode {:.1} MB/s\tmodel enc {:.1} MB/s\tmodel dec {:.1} MB/s",
        100.0 * (1.0 - tout as f64 / tin as f64),
        100.0 * traw as f64 / tout as f64,
        mb / th,
        mb / te,
        mb / td
    );
    Ok(())
}

fn cmd_info(args: &[String]) -> Result<(), String> {
    let (input, _) = split_out(args)?;
    let bytes = read(&input)?;
    if jpeg::is_container(&bytes) {
        let c = jpeg::parse_container(&bytes)?;
        let info = jpeg::info(&bytes)?;
        println!(
            "{}: Komla JPEG v{}, {}, original {} bytes, crc32 {:08x}, {} bytes ({:.2}% smaller)",
            input.display(),
            jpeg::VERSION,
            jpeg_mode(&info),
            c.len,
            c.crc,
            bytes.len(),
            100.0 * (1.0 - bytes.len() as f64 / c.len.max(1) as f64)
        );
        return Ok(());
    }
    let (h, hl) = komla::parse_header(&bytes)?;
    let stripes = komla::split_stripes(&bytes, &h, hl)?;
    let mode = if h.mode & komla::MODE_PALETTE != 0 {
        format!("palette ({} colours)", h.palette.as_ref().map_or(0, |p| p.len()))
    } else if h.mode & komla::MODE_PIXFLAGS != 0 {
        "continuous tone + pixel flags".to_string()
    } else {
        "continuous tone".to_string()
    };
    println!(
        "{}: Komla v{} {} x {} x {}ch {}-bit (coded as {}ch), source {:?}, {mode}, {} stripe(s), crc32 {:08x}, {} bytes ({:.3} bpp)",
        input.display(),
        komla::VERSION,
        h.width,
        h.height,
        h.channels,
        h.depth,
        h.coded_channels,
        h.source,
        stripes.len(),
        h.crc,
        bytes.len(),
        bytes.len() as f64 * 8.0 / (h.width as f64 * h.height as f64)
    );
    Ok(())
}

/// SHA-256 of the pixels expanded to RGBA at the image's bit depth, so that files with
/// different colour types (palette, gray, RGB) but identical pixels hash the same.
fn canonical_hash(img: &komla::Image) -> String {
    use sha2::{Digest, Sha256};
    let nc = img.channels as usize;
    let maxv = img.maxv();
    let mut h = Sha256::new();
    h.update(format!("{}x{}x{};", img.width, img.height, img.depth).as_bytes());
    let mut buf = Vec::with_capacity(img.pixels() * 8);
    for p in img.data.chunks_exact(nc) {
        let rgba = match nc {
            1 => [p[0], p[0], p[0], maxv],
            2 => [p[0], p[0], p[0], p[1]],
            3 => [p[0], p[1], p[2], maxv],
            _ => [p[0], p[1], p[2], p[3]],
        };
        for v in rgba {
            if img.depth == 16 {
                buf.extend_from_slice(&v.to_be_bytes());
            } else {
                buf.push(v as u8);
            }
        }
    }
    h.update(&buf);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn cmd_pixhash(args: &[String]) -> Result<(), String> {
    for a in args {
        let bytes = read(Path::new(a))?;
        let img = if bytes.starts_with(komla::MAGIC) {
            komla::decompress(&bytes, &options(), &mut |_| {})?.0
        } else {
            io::read_image(&bytes)?.0
        };
        println!("{}  {}x{}x{}/{}  {}", canonical_hash(&img), img.width, img.height, img.channels, img.depth, a);
    }
    Ok(())
}

struct JpegRow {
    name: String,
    input: usize,
    size: usize,
    enc_s: f64,
    dec_s: f64,
    ok: Option<bool>,
    mode: String,
}

/// Compresses, decompresses and compares JPEG files byte for byte (SHA-256).
fn bench_jpeg(files: &[PathBuf], threads: usize, verify: bool) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let next = AtomicUsize::new(0);
    let rows: Mutex<Vec<Option<JpegRow>>> = Mutex::new((0..files.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::SeqCst);
                if k >= files.len() {
                    break;
                }
                let f = &files[k];
                let row = (|| -> Result<JpegRow, String> {
                    let bytes = read(f)?;
                    if !jpeg::is_jpeg(&bytes) {
                        return Err("not a JPEG file".into());
                    }
                    let t0 = Instant::now();
                    let (c, info) = jpeg::compress(&bytes, &mut |_| {});
                    let enc_s = t0.elapsed().as_secs_f64();
                    let (dec_s, ok) = if verify {
                        let t1 = Instant::now();
                        let d = jpeg::decompress(&c, &mut |_| {});
                        let dec_s = t1.elapsed().as_secs_f64();
                        (dec_s, Some(d.is_ok_and(|d| Sha256::digest(&d) == Sha256::digest(&bytes))))
                    } else {
                        (0.0, None)
                    };
                    Ok(JpegRow { name: f.file_name().unwrap().to_string_lossy().into_owned(), input: bytes.len(), size: c.len(), enc_s, dec_s, ok, mode: jpeg_mode(&info) })
                })();
                match row {
                    Ok(r) => {
                        eprintln!(
                            "{:<32} {:>10} -> {:>10} {:>6.2}% {:>7.2}s {:>7.2}s {:<8} {}",
                            r.name,
                            r.input,
                            r.size,
                            100.0 * (1.0 - r.size as f64 / r.input as f64),
                            r.enc_s,
                            r.dec_s,
                            match r.ok {
                                Some(true) => "OK",
                                Some(false) => "MISMATCH",
                                None => "-",
                            },
                            r.mode
                        );
                        rows.lock().unwrap()[k] = Some(r);
                    }
                    Err(e) => eprintln!("{}: {e}", f.display()),
                }
            });
        }
    });
    let rows = rows.into_inner().unwrap();
    let (mut ti, mut tb, mut te, mut td, mut fails) = (0usize, 0usize, 0f64, 0f64, 0);
    for r in rows.iter().flatten() {
        println!("{}\t{}\t{}\t{:.3}\t{:.3}", r.name, r.input, r.size, r.enc_s, r.dec_s);
        ti += r.input;
        tb += r.size;
        te += r.enc_s;
        td += r.dec_s;
        if r.ok == Some(false) {
            fails += 1;
        }
    }
    println!(
        "TOTAL\t{}\t{}\t{:.3}% smaller\t{:.1}s enc ({:.2} MB/s)\t{:.1}s dec\t{} failures",
        ti,
        tb,
        100.0 * (1.0 - tb as f64 / ti.max(1) as f64),
        te,
        ti as f64 / te.max(1e-9) / 1e6,
        td,
        fails
    );
    if fails > 0 {
        return Err(format!("{fails} round-trip failures"));
    }
    Ok(())
}

struct BenchRow {
    name: String,
    pixels: usize,
    desc: String,
    size: usize,
    enc_s: f64,
    dec_s: f64,
    ok: Option<bool>,
}

fn cmd_bench(args: &[String]) -> Result<(), String> {
    let mut threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let mut verify = true;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-j" => {
                threads = args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("-j needs a number")?;
                i += 1;
            }
            "--no-verify" => verify = false,
            f => files.push(PathBuf::from(f)),
        }
        i += 1;
    }
    if files.is_empty() {
        return Err("no input files".into());
    }
    let (jpegs, files): (Vec<PathBuf>, Vec<PathBuf>) = files.into_iter().partition(|f| read(f).is_ok_and(|b| jpeg::is_jpeg(&b)));
    if !jpegs.is_empty() {
        bench_jpeg(&jpegs, threads, verify)?;
        if files.is_empty() {
            return Ok(());
        }
    }
    let opts = options();
    let next = AtomicUsize::new(0);
    let rows: Mutex<Vec<Option<BenchRow>>> = Mutex::new((0..files.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::SeqCst);
                if k >= files.len() {
                    break;
                }
                let f = &files[k];
                let row = (|| -> Result<BenchRow, String> {
                    let bytes = read(f)?;
                    let (img, src) = io::read_image(&bytes)?;
                    let t0 = Instant::now();
                    let c = komla::compress(&img, src, &opts, &mut |_| {})?;
                    let enc_s = t0.elapsed().as_secs_f64();
                    let (dec_s, ok) = if verify {
                        let t1 = Instant::now();
                        let d = komla::decompress(&c, &opts, &mut |_| {});
                        let dec_s = t1.elapsed().as_secs_f64();
                        (dec_s, Some(matches!(d, Ok((ref di, _)) if *di == img)))
                    } else {
                        (0.0, None)
                    };
                    Ok(BenchRow {
                        name: f.file_name().unwrap().to_string_lossy().into_owned(),
                        pixels: img.pixels(),
                        desc: format!("{}x{}x{}/{}", img.width, img.height, img.channels, img.depth),
                        size: c.len(),
                        enc_s,
                        dec_s,
                        ok,
                    })
                })();
                match row {
                    Ok(r) => {
                        eprintln!(
                            "{:<28} {:>16} {:>10} {:>8.4} bpp {:>7.2}s {:>7.2}s {}",
                            r.name,
                            r.desc,
                            r.size,
                            r.size as f64 * 8.0 / r.pixels as f64,
                            r.enc_s,
                            r.dec_s,
                            match r.ok {
                                Some(true) => "OK",
                                Some(false) => "MISMATCH",
                                None => "-",
                            }
                        );
                        rows.lock().unwrap()[k] = Some(r);
                    }
                    Err(e) => eprintln!("{}: {e}", f.display()),
                }
            });
        }
    });
    let rows = rows.into_inner().unwrap();
    let (mut tb, mut tp, mut te, mut fails) = (0usize, 0usize, 0f64, 0);
    for r in rows.iter().flatten() {
        println!("{}\t{}\t{}\t{:.3}\t{:.3}", r.name, r.pixels, r.size, r.enc_s, r.dec_s);
        tb += r.size;
        tp += r.pixels;
        te += r.enc_s;
        if r.ok == Some(false) {
            fails += 1;
        }
    }
    println!(
        "TOTAL\t{}\t{}\t{:.4} bpp\t{:.1}s enc\t{:.2} MP/s\t{} failures",
        tp,
        tb,
        tb as f64 * 8.0 / tp as f64,
        te,
        tp as f64 / te / 1e6,
        fails
    );
    if fails > 0 {
        return Err(format!("{fails} round-trip failures"));
    }
    Ok(())
}
