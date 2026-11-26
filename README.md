# Kolmo — lossless image compression by context mixing

Kolmo turns a PNG or BMP into a `.kolmo` file that decodes back to exactly the same pixels,
bit for bit, and is smaller than every standard lossless format we compared against:
**6.4% smaller than JPEG XL at its slowest setting (`-e 10`), 15.7% smaller than lossless
WebP and 34.8% smaller than optimized PNG** on the held-out test set below (photos,
screenshots and graphics).

It comes as

- a **web app** that runs entirely in the browser (the codec is compiled to WebAssembly;
  images never leave your computer): **https://paatai.github.io/Compress/**, and
- a **command-line tool** (Rust) for macOS and Linux. Files are interchangeable.

## Results

Bits per pixel on the test set (lower is better). The test images were never used for tuning;
the final codec was run on them once. Every output was decoded again and compared with the
original pixels by SHA-256 (archives: byte comparison).

| Codec | Kodak photos<br>24 images, 9.4 MP | DIV2K photos<br>10 images, 28.0 MP | Screenshots<br>12 images, 9.3 MP | Charts, diagrams, maps<br>12 images, 10.7 MP | **Total**<br>57.4 MP | Kolmo is smaller by |
|---|---|---|---|---|---|---|
| ZIP -9 (of BMP) | 15.0152 | 19.8078 | 2.4528 | 0.5515 | 12.6254 | 56.5% |
| RAR -m5 (of BMP) | 15.2376 | 14.9942 | 2.0440 | 0.4764 | 10.2351 | 46.3% |
| 7-Zip -mx9 (of BMP) | 12.2634 | 15.7940 | 1.5534 | 0.3453 | 10.0323 | 45.2% |
| xz -9e (of BMP) | 12.2756 | 15.7890 | 1.5346 | 0.3205 | 10.0242 | 45.2% |
| QOI | 13.9885 | 15.1343 | 3.2234 | 0.8314 | 10.3547 | 46.9% |
| PNG (oxipng -o max) | 12.4069 | 12.4286 | 1.5557 | 0.4758 | 8.4398 | 34.9% |
| PNG (zopflipng -m) | 12.4746 | 12.3837 | 1.5461 | 0.4609 | 8.4247 | 34.8% |
| FLIF | 8.6924 | 9.3078 | 1.3684 | 0.3036 | 6.2451 | 12.0% |
| AVIF lossless | 11.2950 | 10.9702 | 2.4296 | 0.7048 | 7.7298 | 28.9% |
| WebP lossless | 9.4978 | 9.7087 | 1.0925 | 0.2547 | 6.5193 | 15.7% |
| JPEG XL e9 | 8.4851 | 8.9105 | 1.3363 | 0.3353 | 6.0180 | 8.7% |
| JPEG XL e10 | 8.4393 | 8.7340 | 1.1125 | 0.2501 | 5.8723 | 6.4% |
| **Kolmo** | **7.9843** | **8.2389** | **0.8325** | **0.1669** | **5.4954** | — |

| | Kodak | DIV2K | Screenshots | Graphics | Total |
|---|---|---|---|---|---|
| Best standard format | JPEG XL e10 | JPEG XL e10 | WebP lossless | JPEG XL e10 | JPEG XL e10 |
| Kolmo versus it | 5.4% smaller | 5.7% smaller | 23.8% smaller | 33.3% smaller | **6.4% smaller** |

So the "at least 1% better than the best standard format" target holds on the total and in
every category. On the development set (used for tuning) the margins are similar: 5.0% below
JPEG XL e10 on DIV2K photos, 30.7% and 25.7% below WebP on screenshots and graphics, 6.0% on
the total. Full tables, byte counts and speeds: [`bench/results/REPORT.md`](bench/results/REPORT.md);
per-image numbers: `bench/results/test.jsonl` and `dev.jsonl`.

**Exact baselines.** zip and RAR, 7-Zip and xz compress an uncompressed BMP of the image (what you
get when you zip a BMP; zipping a PNG barely changes its size). Image formats start from the PNG.

| Codec | Version and command |
|---|---|
| ZIP | Info-ZIP 3.0 (as shipped with macOS): `zip -9` |
| RAR | RAR 7.23: `rar a -m5` |
| 7-Zip | 7-Zip 26.06: `7zz a -mx=9 -mmt=1` |
| xz | XZ Utils 5.4.5: `xz -9e -T1` |
| QOI | reference `qoiconv` (phoboslab/qoi ffb2d2c) |
| PNG | oxipng 10.2.1: `oxipng -o max --strip all`; zopflipng 1.0.3: `zopflipng -m` |
| FLIF | FLIF 0.4: `flif -e -E100 -K` |
| AVIF | libavif 1.4.2 with aom 3.14.1: `avifenc --lossless -s 0` |
| WebP | libwebp 1.6.0: `cwebp -lossless -z 9 -exact` |
| JPEG XL | libjxl 0.13.0: `cjxl -d 0 -e 9` and `cjxl -d 0 -e 10` |

**Speed.** Kolmo is a symmetric context-mixing coder: it encodes and decodes at about
0.14 megapixels per second per core on this machine (an 8-core Xeon), similar to `cwebp -z 9`,
2 to 3.5 times faster than `cjxl -e 10` and `avifenc -s 0` at encoding, but decoding is just as
slow, far slower than every other format here. Images above 4 megapixels are split into
independent stripes that are coded in parallel, so a 12-megapixel photo takes about 40 seconds
in the browser on 8 cores (measured in headless Chrome while other jobs were running).

**Data sets** (fetched and hash-checked by `bench/fetch_datasets.py`):

- *Test:* the 24 Kodak images; DIV2K validation images 0801–0810; 12 screenshots and 12
  charts/diagrams/maps from Wikimedia Commons.
- *Development (tuning):* DIV2K validation 0861–0870 and 24 other Commons images.
- The Commons images are chosen by a fixed rule (`bench/select_commons.py`): every PNG of
  0.25–4 megapixels in a few screenshot and graphics categories, sorted by the file's SHA-1;
  even positions go to the test set, odd positions to the dev set. One file whose title names a
  commercial product is excluded by its SHA-1.
- All images are 8-bit RGB or RGBA (several graphics have alpha). Kolmo also supports 8- and
  16-bit grayscale, gray+alpha, RGB and RGBA; 16-bit images are not part of this benchmark
  (WebP cannot store them losslessly); on small 16-bit tests Kolmo was on par with JPEG XL.

## Using it

### Web app

Drop PNG or BMP files on the page. Each file is compressed, then the `.kolmo` file is decoded
again in the browser and its pixels are compared with the original by SHA-256 before it is
offered as verified. The page also shows the size of the original file, ZIP -9 of the same
pixels, and (one click) lossless WebP and JPEG XL computed in your browser with WebAssembly
builds of libwebp and libjxl. Drop a `.kolmo` file to get a PNG with the exact original pixels;
"Check against original…" compares it with the file you started from.

Run locally:

```bash
git clone https://github.com/paatai/Compress && cd Compress
# needs Node 20+ and Rust (https://rustup.rs) with the wasm32 target
rustup target add wasm32-unknown-unknown
cd web
npm install
npm run dev      # http://127.0.0.1:47123
npm run build    # static site in web/dist (relative paths, works from any sub-path)
```

### Command line

```bash
cargo install --path cli          # or: cargo build --release  (binary in target/release/kolmo)

kolmo compress photo.png          # writes photo.png.kolmo
kolmo compress scan.bmp -o scan.kolmo
kolmo decompress photo.png.kolmo  # restores photo.png (BMP sources restore to .bmp)
kolmo decompress scan.kolmo -o scan.png
kolmo info photo.png.kolmo        # mode, stripes, checksum, bits per pixel
kolmo bench *.png                 # compress, decompress and verify, with timings
kolmo pixhash a.png b.png         # SHA-256 of the pixels, independent of the file format
```

Decompression always checks the CRC-32 of the pixels stored in the file and fails loudly on any
mismatch. The `.github/workflows/release.yml` workflow builds macOS (Apple Silicon and Intel) and
Linux binaries when a `v*` tag is pushed.

**What is preserved:** the exact pixel values (8/16-bit gray, gray+alpha, RGB, RGBA; palette
and low-bit-depth PNGs are restored as 8-bit gray/RGB/RGBA with identical colours, including
the RGB values under fully transparent pixels). Not preserved: PNG metadata chunks (text, ICC
profile, gamma), animation frames after the first, and the original PNG/BMP byte stream.

## How it works

Kolmo predicts every sample from the pixels already coded and codes the prediction error with
a binary arithmetic coder whose probabilities come from mixing many adaptive models.
Everything the decoder needs is recomputed from already decoded pixels, so the file contains
only the arithmetic-coded decisions plus a small header.

1. **Channel handling.** Constant alpha and gray-only colour are detected and removed. Green is
   coded first, then red (using green), then blue (using green and red), alpha before colour.
2. **Predictors.** For each sample about two dozen predictors are evaluated: neighbours, planar
   and gradient extrapolations, MED (LOCO-I), GAP (CALIC), and cross-channel ones such as
   `W + (G − G_W)` that carry the current pixel's green edge into red and blue.
3. **Least squares on a 2-D window.** A linear predictor with 12–16 regressors (neighbours, and
   for red/blue the co-located green/red values) is refitted at every pixel on a causal window
   around it. The normal equations are accumulated with separable exponential weights
   (two-sided along previous rows, decaying upwards, plus the current row), which costs O(n²)
   per pixel instead of O(n² × window); a Cholesky solve gives the weights.
4. **Blending and stacking.** Predictors are blended with weights 1/(local error)², and an online
   least-squares stage learns the best linear combination of the strongest predictors and the
   blend. Least-squares training samples are weighted by the inverse local error variance:
   coding cost depends on the logarithm of the error, so smooth areas must count as much as
   edges. A context-based bias correction (as in CALIC) gives the final integer prediction P.
5. **Binarisation.** The residual is coded as yes/no decisions: zero?, sign, magnitude range
   [2^k, 2^(k+1)) in unary, then the bits inside the range.
6. **Context mixing.** Each decision gets probabilities from
   - 11 context tables (local error level, neighbours' residuals, green/red residual at this
     pixel, sign patterns, predictor agreement, exact neighbour values),
   - 12 *expert* tables, one per predictor, indexed by how far that predictor is from P and
     how reliable it has been nearby. This lets every predictor vote with its own confidence,
     which is the idea behind the residual maps in the image models of paq8px,
   - three gated logistic mixers (weight sets selected by local scale, fractional part of the
     prediction, and the green residual), a final mixer and two adaptive probability maps.
7. **Screenshots and graphics.** Images with at most 256 colours are coded as palette indices
   with 26 neighbour-index contexts, a 2-D match model and counters that remember whether a
   context has always seen the same bit. Images with 257–4096 colours are coded both as palette
   and as continuous tone and the smaller result is kept. For continuous-tone screenshots, a few
   flags first ask whether the pixel equals its left, upper, upper-right or upper-left neighbour
   or the continuation of a 2-D match, and extra exact-value contexts include the pixel's
   already coded channels.
8. **Determinism.** Encoder and decoder must compute identical probabilities on every platform.
   All modelling is integer arithmetic or IEEE-754 basic operations in a fixed order (no fused
   multiply-add, no platform `exp`/`log`; the few tables that need them use a portable
   implementation). The WebAssembly build produces byte-identical files to the native build,
   which CI checks.
9. **Container.** Header (size, channels, depth, reductions, mode, CRC-32 of the pixels,
   palette), a stripe table, and independently coded stripes (more than one only above
   4 megapixels). Stripes that would not shrink are stored raw.

### What helped, and what did not

Measured on development images while building the codec (changes in total size):

| Change | Effect |
|---|---|
| Cross-channel predictors (green edges carried into red and blue) | −4.9% |
| Sharper error-weighted blending (ε = 0.125 instead of 2) | −1.1% |
| 2-D windowed least-squares predictor (all channels) | −1.5% |
| Stacking with inverse-variance weighted least squares | −0.8% (about 0% without the weighting) |
| One expert table per predictor | −1.0% |
| Residual-based context tables | −1.4% |
| Palette mode, repeat flags and match model (screenshots and graphics) | −53% on those images |
| History-aware counters and more contexts for palettes, 4096-colour palettes with mode trial, exact contexts for synthetic images | a further −14% on those images |
| 64-bit mixer accumulation (fixed an overflow that hit highly predictable images) | one graphic coded without repeat flags went from 125 KB to 3.2 KB |

Tried and dropped: a parametric (logistic) probability input (−0.03%), raster-order least squares
once the 2-D version existed (−0.08% for 13% more time), exact-value contexts on photos (0%),
a learning-rate warm-up for the mixers (worse), contexts built from the coding cost of
neighbouring samples (±0.02%), expert confidence that also uses the green residual (+0.06%),
alternative final probability blends (±0.05%), stacking all predictors instead of a subset (0%),
and repeat flags for palette images (no gain once the index model was strong). Splitting every
image into stripes for parallelism costs 0.7–1% with 0.7–1.4 megapixel stripes, so stripes are
only used above 4 megapixels.

**Headroom.** paq8px (`-8`), a general context-mixing compressor that is roughly 65 times
slower, is still 4.8% smaller on 768×512 crops of the development photos and 34% smaller on the
development screenshots and graphics. Kolmo trades that last bit of compression for being
practical in a browser.

## Reproducing the benchmark

```bash
bench/install-baselines.sh          # downloads/builds the exact baseline encoders into bench/.tools
python3 bench/fetch_datasets.py     # downloads and hash-checks all images into bench/data
cargo build --release -p kolmo-cli
python3 bench/run_benchmark.py --set test   # every codec, every image, with lossless checks
python3 bench/run_benchmark.py --set dev
python3 bench/make_report.py        # writes bench/results/REPORT.md and the site's table
```

`run_benchmark.py` caches results in `bench/results/<set>.jsonl`; `--force kolmo` re-runs one
codec. Tests: `cargo test --release` (round trips, corrupt input), `node tests/cross_target.mjs`
(native and WebAssembly give identical files), `python3 tests/make_edge_cases.py` (1×1 images,
16-bit, palettes with transparency, interlaced PNG, BMP variants).

## Putting the web app online

The site is static and is published to **https://paatai.github.io/Compress/** by
`.github/workflows/pages.yml` on every push to `main`. The workflow installs Rust and Node,
builds the WebAssembly codec and the site with the Pages base path (`/Compress/`), and deploys
`web/dist`. One-time setup in the GitHub repository: Settings → Pages → Build and deployment →
Source: **GitHub Actions** (then re-run the workflow from the Actions tab if the first run
happened before that).

Any other static host works too: run `npm run build` in `web/` and upload `web/dist` (the
default build uses relative paths, so it works from any sub-path). The site needs no special
headers (it does not use SharedArrayBuffer); parallelism comes from one Web Worker per stripe.

## Repository layout

```
codec/   the codec library (Rust): models, range coder, container, PNG/BMP I/O
cli/     the kolmo command-line tool
wasm/    C-ABI WebAssembly bindings used by the web app
web/     Vite + React + Tailwind + shadcn/ui app (workers in web/src/workers)
bench/   baseline installer, dataset manifests and fetcher, benchmark runner, results
tests/   cross-target determinism test and edge-case generator
```

## License

MIT.
