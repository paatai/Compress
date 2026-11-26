use kolmo::{ct::Tuning, io, Options};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

const USAGE: &str = "\
Kolmo - lossless image compression by context mixing

USAGE:
  kolmo compress   <input.png|input.bmp> [-o output.kolmo]
  kolmo decompress <input.kolmo> [-o output.png|output.bmp]
  kolmo info       <input.kolmo>
  kolmo bench      [-j threads] [--no-verify] <images...>
  kolmo pixhash    <images...>   (SHA-256 of pixels normalised to RGBA)

compress writes <input>.kolmo by default; decompress restores the original
file type (PNG or BMP) unless -o names another extension. Decompression checks
the embedded CRC-32 of the pixels and fails loudly on any mismatch.";

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
        "--version" | "-V" => {
            println!("kolmo {}", env!("CARGO_PKG_VERSION"));
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
    if let Ok(s) = std::env::var("KOLMO_TUNE") {
        for kv in s.split(',').filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once('=').expect("KOLMO_TUNE expects key=value pairs");
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

fn cmd_compress(args: &[String]) -> Result<(), String> {
    let (input, output) = split_out(args)?;
    let bytes = read(&input)?;
    let (img, src) = io::read_image(&bytes)?;
    let t0 = Instant::now();
    let out = kolmo::compress(&img, src, &options(), &mut |_| {})?;
    let dt = t0.elapsed().as_secs_f64();
    let output = output.unwrap_or_else(|| {
        let mut s = input.as_os_str().to_owned();
        s.push(".kolmo");
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
    let t0 = Instant::now();
    let (img, src) = kolmo::decompress(&bytes, &options(), &mut |_| {})?;
    let dt = t0.elapsed().as_secs_f64();
    let output = output.unwrap_or_else(|| {
        let stem = input.to_string_lossy();
        let stem = stem.strip_suffix(".kolmo").map(str::to_string).unwrap_or_else(|| {
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

fn cmd_info(args: &[String]) -> Result<(), String> {
    let (input, _) = split_out(args)?;
    let bytes = read(&input)?;
    let (h, hl) = kolmo::parse_header(&bytes)?;
    let stripes = kolmo::split_stripes(&bytes, &h, hl)?;
    let mode = if h.mode & kolmo::MODE_PALETTE != 0 {
        format!("palette ({} colours)", h.palette.as_ref().map_or(0, |p| p.len()))
    } else if h.mode & kolmo::MODE_PIXFLAGS != 0 {
        "continuous tone + pixel flags".to_string()
    } else {
        "continuous tone".to_string()
    };
    println!(
        "{}: Kolmo v{} {} x {} x {}ch {}-bit (coded as {}ch), source {:?}, {mode}, {} stripe(s), crc32 {:08x}, {} bytes ({:.3} bpp)",
        input.display(),
        kolmo::VERSION,
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
fn canonical_hash(img: &kolmo::Image) -> String {
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
        let img = if bytes.starts_with(kolmo::MAGIC) {
            kolmo::decompress(&bytes, &options(), &mut |_| {})?.0
        } else {
            io::read_image(&bytes)?.0
        };
        println!("{}  {}x{}x{}/{}  {}", canonical_hash(&img), img.width, img.height, img.channels, img.depth, a);
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
                    let c = kolmo::compress(&img, src, &opts, &mut |_| {})?;
                    let enc_s = t0.elapsed().as_secs_f64();
                    let (dec_s, ok) = if verify {
                        let t1 = Instant::now();
                        let d = kolmo::decompress(&c, &opts, &mut |_| {});
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
