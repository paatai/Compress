# Komla — lossless image compression by context mixing

Komla turns a PNG or BMP into a `.komla` file that decodes back to exactly the same pixels,
bit for bit, and is smaller than every standard lossless format we compared against:
**6.4% smaller than JPEG XL at its slowest setting (`-e 10`), 15.7% smaller than lossless
WebP and 34.8% smaller than optimized PNG** on the held-out test set below (photos,
screenshots and graphics).

It also recompresses **JPEG files losslessly**: the `.komla` file decompresses to the original
JPEG, byte for byte, and is **24.1% smaller than the JPEGs** on the held-out JPEG test set below.
That is 5.2% less output than paq8px, 6.3% less than Dropbox's Lepton and 9.7% less than JPEG
XL's lossless JPEG transcoding. paq8px remains 1.5 points ahead on baseline JPEGs, but it
cannot model progressive JPEGs, which Komla handles as well as baseline ones.

A **fast JPEG mode** (`komla compress --fast`, and the web app's default) is built for speed and
little memory. On 66 JPEGs (228 MB) drawn at random for the final evaluation and run once, after
the code was frozen, it was **the fastest to compress and to decompress and used the least
memory** of all the tools compared (JPEG XL, Dropbox's Lepton and its Rust port, brunsli,
packJPG, paq8px), on one core and on eight: for example **6 MiB to decompress** (Lepton 19 MiB,
the others 266 MiB or more) and **42 MiB to compress** (the others 224 MiB or more). The JPEGs
got **19.9% smaller**, more than with JPEG XL (18.0%), brunsli (19.7%) and Lepton, but packJPG
(20.2%) and the single-threaded Rust port of Lepton (20.1%) did slightly better on these files,
and paq8px far better at a thousandth of the speed. Details and the development and test sets:
[below](#fast-mode-size-speed-and-memory).

It comes as

- a **web app** that runs entirely in the browser (the codec is compiled to WebAssembly;
  images never leave your computer): **https://paatai.github.io/Compress/**, and
- a **command-line tool** (Rust) for macOS and Linux. Files are interchangeable.

## Results: lossless images (PNG, BMP)

Bits per pixel on the test set (lower is better). The test images were never used for tuning;
the final codec was run on them once. Every output was decoded again and compared with the
original pixels by SHA-256 (archives: byte comparison).

| Codec | Kodak photos<br>24 images, 9.4 MP | DIV2K photos<br>10 images, 28.0 MP | Screenshots<br>12 images, 9.3 MP | Charts, diagrams, maps<br>12 images, 10.7 MP | **Total**<br>57.4 MP | Komla is smaller by |
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
| **Komla** | **7.9843** | **8.2389** | **0.8325** | **0.1669** | **5.4954** | — |

| | Kodak | DIV2K | Screenshots | Graphics | Total |
|---|---|---|---|---|---|
| Best standard format | JPEG XL e10 | JPEG XL e10 | WebP lossless | JPEG XL e10 | JPEG XL e10 |
| Komla versus it | 5.4% smaller | 5.7% smaller | 23.8% smaller | 33.3% smaller | **6.4% smaller** |

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

**Speed.** Komla is a symmetric context-mixing coder: it encodes and decodes at about
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
- All images are 8-bit RGB or RGBA (several graphics have alpha). Komla also supports 8- and
  16-bit grayscale, gray+alpha, RGB and RGBA; 16-bit images are not part of this benchmark
  (WebP cannot store them losslessly); on small 16-bit tests Komla was on par with JPEG XL.

## Results: lossless JPEG recompression

How much smaller than the original JPEG files (higher is better) on the held-out JPEG test set.
Komla was tuned only on the development set and run on the test files once, with the final
code. Every output, of every tool, was decompressed again and compared with the original JPEG
file byte for byte (SHA-256); a tool that rejects a file or does not restore it exactly is
counted at the original size. Komla restored all 80 test files exactly.

| Tool | Kodak q50<br>12 files | Kodak q75<br>12 files | Kodak q90 4:4:4<br>12 files | Kodak q95 progressive<br>12 files | Camera photos<br>12 files, 52 MB | Progressive<br>8 files, 25 MB | Grayscale<br>6 files, 5 MB | Screenshots<br>6 files, 2 MB | **Total**<br>80 files, 89 MB | Rejected |
|---|---|---|---|---|---|---|---|---|---|---|
| JPEG XL | 16.60% | 13.46% | 14.57% | 8.86% | 17.82% | 12.35% | 16.16% | 21.02% | 16.00% | 0 |
| brunsli | 20.64% | 16.35% | 16.89% | 11.06% | 20.54% | 14.65% | 20.70% | 22.34% | 18.67% | 0 |
| Lepton (Dropbox) | 22.14% | 17.88% | 17.62% | 11.24% | 20.78% | 15.14% | 21.34% | 21.57% | 19.00% | 0 |
| packJPG | 23.32% | 18.48% | 18.53% | 12.02% | 21.05% | 15.57% | 21.07% | 22.46% | 19.33% | 0 |
| Lepton (Rust port) | 22.07% | 17.88% | 18.15% | 11.49% | 21.30% | 15.74% | 21.96% | 20.26% | 19.49% | 1 |
| paq8px | **27.93%** | **23.21%** | **24.41%** | 0.94% | **27.43%** | 2.72% | **27.94%** | 26.62% | 19.97% | 0 |
| **Komla** | 26.97% | 22.31% | 22.98% | **15.45%** | 25.77% | **20.52%** | 26.33% | **27.73%** | **24.11%** | 0 |

| | Baseline JPEGs (62.7 MB) | Progressive JPEGs (26.5 MB) | Total (89.3 MB) |
|---|---|---|---|
| Komla | 25.78% | 20.16% | **24.11%** |
| Best other tool | paq8px 27.31% | Lepton (Rust) 15.44% | paq8px 19.97% |
| Lepton (Dropbox) | 20.75% | 14.87% | 19.00% |
| JPEG XL | 17.65% | 12.10% | 16.00% |

So Komla beats Lepton and JPEG XL on every group and on the total, and beats paq8px on the
total (its output is 5.2% smaller) because paq8px has no progressive JPEG model; on baseline
JPEGs paq8px, about 35 times slower, is still 1.5 points better. On the development set the
picture is the same (Komla 25.0% overall, Lepton 19.1%, brunsli 19.4%, JPEG XL 16.8%). Full
tables, byte counts and the development set: [`bench/results/JPEG_REPORT.md`](bench/results/JPEG_REPORT.md);
per-file numbers: `bench/results/jpeg_test.jsonl` and `jpeg_dev.jsonl`.

| Tool | Version and command |
|---|---|
| JPEG XL | libjxl 0.13.0: `cjxl --lossless_jpeg=1 -e 9`, restored with `djxl` |
| brunsli | google/brunsli c18b962: `cbrunsli`, `dbrunsli` |
| Lepton | dropbox/lepton f34c7f4: `lepton -singlethread -allowprogressive` (its default threading for the files where single-threaded mode fails its own round-trip check) |
| Lepton (Rust port) | microsoft/lepton_jpeg_rust 0.5.9: `lepton_jpeg_util --threads 1` |
| packJPG | packJPG 2.5n |
| paq8px | paq8px v217: `paq8px -8` |

**Speed.** Single-threaded, the default (maximum compression) mode compresses and
decompresses about 0.35 MB of JPEG per second (paq8px 0.01 MB/s). Files above about 5.6
megapixels are split into stripes coded in parallel: in the browser (headless Chrome, 8
cores) a 12.6-megapixel photo is compressed and verified in 13 seconds, a 24-megapixel one in
12.5 seconds. The fast mode below compresses about 39 times and decompresses about 48 times as
fast on one core (test set; the maximum mode was not timed again in the final evaluation).

### Fast mode: size, speed and memory

Three sets: the *development set* (used for all tuning), the *test set* (held out from tuning,
but run in earlier evaluations) and a *random held-out set* drawn for the final evaluation: 66
files (227.6 MB) picked with seed 20261127 from a recorded pool of 9,999 freely licensed
Wikimedia Commons JPEGs (camera originals of 16 models not used before, edited and featured
photos, progressive, black-and-white, screenshots, scans and small web images), sharing no
file with the other sets, and run exactly once, after Komla's code was frozen (commit
8430279). Every tool ran on every file one process at a time, all tools on one file before
the next, single-threaded and at its default thread count on 8 cores (x86-64 virtual machine
with AVX2), files on a RAM disk. Speeds are megabytes of original JPEG per second for the whole
process (fastest of three runs; the three runs' totals differ by 0.1 to 5.3%), memory is the
largest peak resident set of any file (`/usr/bin/time`), and every output was restored and
compared with the original byte for byte. JPEG XL runs at its default effort (7).

Random held-out set:

| Tool | Threads | Smaller | Compress | Decompress | Memory, compress | Memory, decompress | Failed |
|---|---|---|---|---|---|---|---|
| **Komla fast** | 1 | **19.88%** | **11.66 MB/s** | **14.78 MB/s** | **42 MiB** | **6 MiB** | 0 |
| JPEG XL | 1 | 17.99% | 7.35 MB/s | 13.57 MB/s | 964 MiB | 274 MiB | 0 |
| Lepton (Dropbox) | 1 | 14.82% | 3.40 MB/s | 7.74 MB/s | 224 MiB | 19 MiB | 14 |
| Lepton (Rust port) | 1 | 20.12% | 9.52 MB/s | 8.68 MB/s | 265 MiB | 505 MiB | 0 |
| brunsli | 1 | 19.67% | 8.60 MB/s | 10.46 MB/s | 788 MiB | 266 MiB | 0 |
| packJPG | 1 | 20.20% | 3.28 MB/s | 3.18 MB/s | 271 MiB | 268 MiB | 0 |
| **Komla fast** | 8 | **19.88%** | **34.01 MB/s** | **41.40 MB/s** | **80 MiB** | **77 MiB** | 0 |
| JPEG XL | 8 | 17.99% | 12.31 MB/s | 23.39 MB/s | 957 MiB | 286 MiB | 0 |
| Lepton (Dropbox) | 8 | 19.78% | 7.50 MB/s | 20.89 MB/s | 290 MiB | 291 MiB | 0 |
| Lepton (Rust port) | 8 | 19.80% | 21.37 MB/s | 24.80 MB/s | 277 MiB | 505 MiB | 0 |

brunsli and packJPG have no threads (their rows apply to both). Lepton's single-threaded mode
refuses 14 files (its own round-trip check fails), which count at their original size; no tool
crashed or restored a file wrongly, and Komla coded every file in its fast mode. paq8px is far
too slow to finish this set in the time available (0.02 MB/s): on the 14 files it finished it
made them 27.1% smaller, against 21.3% for Komla fast, 21.4% for Lepton's Rust port and 21.1%
for packJPG, with 2.3 GB of memory.

Komla fast against the other tools on all five axes (size, compression speed, decompression
speed, memory to compress, memory to decompress; speed and memory within 5% count as ties, size
differences are exact):

| Set | One thread | Eight threads | Not won |
|---|---|---|---|
| Random held-out, 66 files | 23 wins, 2 losses | 24 wins, 1 loss | size: packJPG 20.20% and Lepton's Rust port (one thread) 20.12% against 19.88%; paq8px (see above) |
| Test, 80 files | 26 wins, 1 tie, 1 loss | 27 wins, 1 loss | size: paq8px 19.97% against 19.68%; one-thread decompression ties JPEG XL (Komla 2.8% slower) |
| Development, 80 files (tuning set) | 24 wins, 1 tie | 25 wins | one-thread decompression ties JPEG XL (Komla 0.6% faster); paq8px finished 58 files: 27.1% against 22.4% |

Tuning risk: on the development set Komla fast makes the files 20.66% smaller and is ahead of
every tool but paq8px on size; on the held-out set it is 0.24 points behind the Rust port of
Lepton and 0.32 behind packJPG. Part of that gap may be the tuning (the development set shaped
the model and its learned starting distributions), part the kind of files: on the held-out set
Komla fast trails on camera originals, edited photos and scans, and leads on progressive files,
screenshots and small web images (per-group table in the report). The
narrowest speed margin is one-thread decompression against JPEG XL: a tie on the development
and test sets, 9% faster on the held-out set. All tables (with the development and test sets,
per-group sizes, run-to-run spread and failures):
[`bench/results/JPEG_SPEED_REPORT.md`](bench/results/JPEG_SPEED_REPORT.md); per-file numbers:
`bench/results/jpeg_speed_{heldout,test,dev}.jsonl` and `bench/results/jpeg_paq8px_*.jsonl`.

**Data sets** (built and hash-checked by `bench/fetch_jpeg_datasets.py`):

- *Kodak:* the 24 Kodak images encoded with libjpeg-turbo 3.2.1 `cjpeg` at four settings:
  quality 50 (4:2:0, standard tables), 75 (`-optimize`), 90 4:4:4 (`-optimize`) and 95
  `-progressive`. Odd-numbered images form the development set, even-numbered the test set.
- *Wikimedia Commons* (`bench/select_jpegs.py`, licences CC0, public domain, CC BY, CC BY-SA):
  camera originals (EXIF make and model, no editing software) from eight camera models
  including phones, progressive JPEGs from Quality and Featured images, grayscale JPEGs from
  black-and-white photographs, and JPEG screenshots. Files in a size range are sorted by SHA-1
  and the ones passing the group's check alternate between test and development set.
- *Random held-out set* (`bench/select_heldout.py`, same licences): the pool and the draw are
  recorded in `bench/datasets/jpeg_heldout_pool.json.gz` and `bench/datasets/jpeg_heldout.json`;
  each group's pool, sorted by SHA-1, is shuffled with the seed and the first files passing the
  group's check are kept. 50 baseline and 16 progressive files; 4:4:4 (29), 4:2:0 (23), 4:2:2 (4),
  1×2 (1) and grayscale (9); 14 with restart markers, 3 with data after the image; 35 KB to 13.5
  MB, up to 41.7 megapixels.

## Using it

### Web app

Drop PNG, BMP or JPEG files on the page. Each file is compressed, then the `.komla` file is
decoded again in the browser and compared with the original by SHA-256 (the pixels of a PNG or
BMP, the whole file of a JPEG) before it is offered as verified. For PNG and BMP the page also
shows the size of the original file, ZIP -9 of the same pixels, and (one click) lossless WebP
and JPEG XL computed in your browser with WebAssembly builds of libwebp and libjxl; for a JPEG
it shows how much smaller the `.komla` file is. JPEGs use the fast mode unless "Maximum
compression" is selected under the drop zone. Drop a `.komla` file to restore the original:
a JPEG comes back as the identical JPG file, a PNG or BMP as a PNG with the exact original
pixels. "Check against original…" compares the result with the file you started from.

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
cargo install --path cli          # or: cargo build --release  (binary in target/release/komla)

komla compress photo.png          # writes photo.png.komla
komla compress scan.bmp -o scan.komla
komla decompress photo.png.komla  # restores photo.png (BMP sources restore to .bmp)
komla decompress scan.komla -o scan.png
komla info photo.png.komla        # mode, stripes, checksum, bits per pixel
komla bench *.png                 # compress, decompress and verify, with timings
komla pixhash a.png b.png         # SHA-256 of the pixels, independent of the file format

komla compress photo.jpg          # writes photo.jpg.komla (maximum compression)
komla compress --fast photo.jpg   # fast mode, on all cores (KOMLA_THREADS=n to limit)
komla decompress photo.jpg.komla  # restores photo.jpg, identical to the original file (either mode)
komla bench [--fast] *.jpg        # compress, decompress, compare files by SHA-256, timings
```

Decompression always checks the CRC-32 stored in the file (of the pixels for PNG/BMP, of the
whole original file for JPEG) and fails loudly on any mismatch. The `.github/workflows/release.yml` workflow builds macOS (Apple Silicon and Intel) and
Linux binaries when a `v*` tag is pushed.

**What is preserved:** for PNG and BMP, the exact pixel values (8/16-bit gray, gray+alpha,
RGB, RGBA; palette and low-bit-depth PNGs are restored as 8-bit gray/RGB/RGBA with identical
colours, including the RGB values under fully transparent pixels). Not preserved: PNG metadata
chunks (text, ICC profile, gamma), animation frames after the first, and the original PNG/BMP
byte stream. For JPEG, everything: the restored file is the original file, byte for byte.

## Formats

| Input | What `.komla` stores | Restores |
|---|---|---|
| PNG (all colour types, 1–16 bit, interlaced), BMP (1/4/8/24/32-bit, uncompressed) | the pixels, coded by the image model | the exact pixels, as PNG (or BMP) |
| JPEG: baseline and extended sequential, progressive (spectral selection and successive approximation), Huffman coded, 8-bit, 1–4 components, any sampling factors, restart markers | the quantized DCT coefficients, coded by the JPEG model (default) or the fast JPEG model (`--fast`), plus everything needed to rebuild the file: all marker segments verbatim (EXIF, ICC, XMP, comments, tables), padding bits, bytes after the end of the image, how end-of-band runs were split | the original file, byte for byte |
| Any other JPEG (arithmetic coded, lossless or hierarchical, 12-bit, truncated or damaged) | the file itself, coded with a general-purpose byte model | the original file, byte for byte |

A JPEG is only recompressed after Komla has regenerated every scan from the decoded
coefficients and found it identical to the input; otherwise it is stored. Either way the
`.komla` file records the length and CRC-32 of the original file, which decompression checks.

## How it works

Komla predicts every sample from the pixels already coded and codes the prediction error with
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
development screenshots and graphics. Komla trades that last bit of compression for being
practical in a browser.

### Lossless JPEG recompression

1. **Exact re-encoding.** The file is split into its marker segments, which are kept as they
   are, and the entropy-coded scans, which are Huffman-decoded into the quantized DCT
   coefficients: baseline and extended sequential scans, and progressive DC/AC first and
   refinement scans with end-of-band runs, all with restart markers. Everything the
   coefficients do not determine is recorded as side information: the padding bits before
   each restart marker and at the end of each scan, bytes between scan data and the next
   marker, and, in progressive scans, where each end-of-band run was cut wherever the encoder
   had a choice (coded against libjpeg's rule, so it costs nothing for libjpeg files). An
   encoder that follows libjpeg's rules regenerates every scan from the coefficients and the
   side information; Komla only keeps the recompressed form if that reproduces the input byte
   for byte, and stores the file otherwise.
2. **Coding order.** Luma is coded before chroma, blocks in raster order. Inside a block: the
   number of nonzero coefficients among the 49 with u, v ≥ 1, those coefficients in zigzag
   order, then the first row and the first column (each with its own nonzero count), and DC
   last, the order Lepton uses: the first row/column and DC are best predicted once the rest of
   the block is known.
3. **Predictions.**
   - Every 7×7 coefficient gets a signed prediction from an online least-squares fit per
     position (exponential forgetting, refitted every 32 updates) whose inputs are the same
     column of the block above, the same row of the block to the left, the two corner blocks
     and the coefficients of this block already coded in the same row, column and diagonals.
     This alone saved 0.6–0.8 points.
   - Every first-row/first-column coefficient: a least-squares fit from the neighbour's
     column/row and this block's coefficients in that column/row (it learns the boundary
     continuity that Lepton hard-codes, per image), blended with the classic continuity
     predictions with and without gradient extrapolation and with the same coefficient in the
     neighbouring blocks, weighted by inverse squared recent error.
   - DC: from the boundary pixels of the blocks above and to the left given this block's AC
     (with and without gradient), the median of the neighbours' DC, and a least-squares
     combination of these.
   - Chroma: the co-located luma pixels are downsampled to the chroma resolution and
     transformed; each chroma coefficient is predicted from that with a scale fitted on
     neighbouring blocks, and Cr also from Cb.
4. **Context mixing.** Values are coded as zero?, exponent in unary, mantissa bits and sign.
   Each binary decision is predicted by 16 context models (neighbour values and magnitudes,
   nonzero coefficients still to come, prediction buckets, block activity, ...), by a context
   that places the prediction relative to the threshold the decision tests, and by the
   probabilities a logistic and a Laplace distribution centred on the prediction give the
   decision (scale from the tracked prediction error). Four gated logistic mixers, a second
   mixing layer and three APMs give the final probability. Nonzero counts have their own
   model, which also sees how many coefficients the least-squares fits expect to be nonzero.
5. **Stripes.** JPEGs above about 131 000 blocks (about 5.6 megapixels at 4:2:0) are split
   into bands of MCU rows that are coded independently, on several threads or browser workers.
6. **Everything else** (marker segments such as EXIF, ICC and XMP, bytes after the end of the
   image, and JPEGs that are stored) goes through a general-purpose byte coder: orders 0–6 and a
   word context, a match model, mixing and APMs.
7. **Determinism.** Integer arithmetic, except the least-squares fits, which use IEEE-754 basic
   operations in a fixed order like the image model; native and WebAssembly output are
   identical (checked in CI on x86-64 Linux and ARM macOS).

What helped, measured on the development sets while building it (Kodak re-encodes / a third
of the Commons photos; points of saving):

| Change | Effect |
|---|---|
| Edge and DC predictors blended by recent error; decision context relative to the prediction | +0.1 |
| Chroma predicted from luma (and Cr from Cb) | +0.27 |
| Neighbour activity and nonzero-tail contexts; faster mixer learning at the start | +0.12 |
| Logistic probability of each decision around the prediction, and a mixer selected by it | +0.33 |
| Least-squares prediction of the 7×7 coefficients | +0.57 / +0.75 |
| Least-squares prediction of the first row and column, used as centre of the distribution | +0.18 / +0.29 |
| Least-squares DC combination, third APM, Laplace probability input, count contexts from the fits | +0.2 / +0.1 |
| Context tables capped at 2^16 blocks (about 60 MB of tables per stripe instead of 115 MB) | −0.01 |

Tried and dropped: coding first-row/column coefficients as residuals from their prediction
instead of using the prediction as context (−1.2 / −1.6), bit-history counters (no gain, 50%
slower), linear-domain mixer inputs (−1.5), seeding new contexts with the position's overall
statistics (−0.2), coding all nonzero counts first so that blocks can see the counts of the
blocks to the right and below (0), phase-continuity sign contexts (0), least-squares inputs
from blocks two steps away (0).

### Fast JPEG mode

`komla compress --fast` (and the web app's default JPEG mode) uses the same parser, the same
libjpeg-rule re-encoder and the same verification, with a model built for speed:

1. **One decoding step per coefficient.** Whether a coefficient is zero is a 16-bit adaptive
   binary probability; a nonzero coefficient's magnitude is one symbol out of 14 classes (the
   class includes the bit below the leading one), from an adaptive cumulative-frequency
   distribution; signs and the remaining mantissa bits, which are close to random, go to a
   separate stream of raw bits. Symbols are coded with two interleaved rANS states: the decoder
   finds a symbol by comparing the state's slot with the 16 cumulative frequencies in one SIMD
   operation, with no division, and the distributions adapt with shifts.
2. **Lepton's coding order and predictions.** Per block: the number of nonzero coefficients
   among the 49 with u, v ≥ 1, those coefficients in zigzag order, then the first row and the
   first column with their own counts, and DC last. Each first-row (first-column) coefficient
   and DC is predicted by extending the block above's (left's) inverse-DCT profile across the
   block boundary linearly, at that frequency; the prediction picks the contexts and predicts
   the sign.
3. **Contexts.** An interior coefficient's context is its position, the number of nonzero
   coefficients still to come, and a running estimate of its magnitude: the same coefficient in
   the four neighbouring blocks, plus every coefficient of the block coded so far that lies to
   its left, above, diagonally, two steps away or just before it on its anti-diagonal (each
   coded coefficient adds itself to the estimates of the positions after it). Magnitudes also
   see the anti-diagonal and how dense the remaining nonzero coefficients are. Edge
   coefficients use the prediction (its class picks the magnitude distribution, and how well
   the magnitude agrees with it picks the sign probability), the adjacent interior
   coefficients and the previous edge coefficient, and the same coefficient in the neighbouring
   blocks; a chroma block's count sees the counts of the luma blocks at the same place.
4. **Learned starting point.** Every distribution starts from frequencies learned on the
   development set (`komla jtrain`; 272 KB compiled into the program) instead of a flat
   guess, which matters for small images and for every stripe.
5. **Speed and memory.** Images are cut into stripes of about 32 000 blocks, coded and decoded
   on all cores. Both directions stream. Compressing, every scan has its own incremental
   Huffman decoder, so the coefficients of one stripe at a time are decoded from all scans; one
   thread regenerates the scans from the stripes in order and checks them against the input
   while the other threads code the stripes, with at most a few stripes in memory.
   Decompressing reads the `.komla` file piece by piece and writes the JPEG straight to its
   place in the output file: with one thread, every MCU row is turned back into scan data as
   soon as it is decoded (a ring of two MCU rows); with more, the side info holds each
   sequential scan's encoder state at every stripe boundary, so the threads that decode the
   stripes also regenerate their scan data, and only the writing is in order. Scan data is
   regenerated by walking the nonzero coefficients through 64-bit masks and writing whole
   words. On x86-64 the hot loops also exist compiled for AVX2/BMI2, chosen at run time;
   WebAssembly uses SIMD128.
6. **Determinism.** Integer arithmetic only: SSE2, AVX2, scalar and WebAssembly code paths
   produce the same files.

What helped, on the development set (2-fold cross-validated, so the learned starting points
never saw the files they were scored on):

| Step | Saved |
|---|---|
| Multi-symbol model with Lepton's order, edge/DC continuity prediction, no learned priors | 18.04% |
| Learned starting distributions | 18.39% |
| Diagonal neighbours in the interior context | 18.51% |
| Coefficients above/left in the same block, combined with the neighbours into one magnitude estimate | 18.97% |
| Edge contexts: adjacent interior coefficient; chroma counts from luma | 19.27% |
| Edge contexts: the same coefficient in the neighbouring blocks | 19.39% |
| Edge prediction extrapolated linearly across the boundary instead of continued flat | 19.58% |
| The same for DC | 19.70% |
| Edge magnitudes and signs by the class of the prediction; magnitudes by anti-diagonal and the remaining count; diagonal and farther coefficients of the same block in the estimate | 20.44% |
| Running magnitude estimate, density of the remaining coefficients, wider edge neighbourhood (and fewer, larger tables: same speed) | 20.57% |

Tried and dropped: one 15-symbol distribution for zero and magnitude together (fewer steps but
much worse on small images), count-based update rates for the binary probabilities (no better
than a fixed rate once priors exist), smaller joint edge contexts (0.1 points worse, no
faster), coding the top mantissa bit with a probability (+0.15 points, but 7 to 10% slower
decoding), four times larger stripes (+0.18 points, but four times the memory with eight
threads), DC contexts from the neighbours' DC prediction errors (no gain), and other prior
strengths (no gain).

## Reproducing the benchmark

Lossless image compression (PNG/BMP):

```bash
bench/install-baselines.sh          # downloads/builds the exact baseline encoders into bench/.tools
python3 bench/fetch_datasets.py     # downloads and hash-checks all images into bench/data
cargo build --release -p komla-cli
python3 bench/run_benchmark.py --set test   # every codec, every image, with lossless checks
python3 bench/run_benchmark.py --set dev
python3 bench/make_report.py        # writes bench/results/REPORT.md and the site's table
```

`run_benchmark.py` caches results in `bench/results/<set>.jsonl`; `--force komla` re-runs one
codec.

JPEG recompression:

```bash
bench/install-baselines.sh          # cjxl/djxl (libjxl 0.13.0)
bench/install-jpeg-baselines.sh     # libjpeg-turbo 3.2.1, Lepton (C++ and Rust), packJPG, paq8px, brunsli
python3 bench/fetch_jpeg_datasets.py   # encodes the Kodak sets, downloads the Commons files, checks hashes
cargo build --release -p komla-cli
python3 bench/run_jpeg_benchmark.py --set test   # every tool, every file, byte-exact checks
python3 bench/run_jpeg_benchmark.py --set dev
python3 bench/make_jpeg_report.py   # writes bench/results/JPEG_REPORT.md and the site's table
```

Size, speed and memory of the fast mode and the other tools (one process at a time, all tools
on a file before the next; about 50 minutes on 8 cores for the three sets without Komla's
maximum mode), and paq8px separately (hours; several files at a time):

```bash
python3 bench/select_heldout.py pool    # lists the Commons pool -> bench/datasets/jpeg_heldout_pool.json
python3 bench/select_heldout.py draw    # seeded random draw -> bench/datasets/jpeg_heldout.json
python3 bench/select_heldout.py fetch   # downloads the held-out set, checks SHA-1
python3 bench/run_jpeg_speed.py --set heldout --codecs jxl,brunsli,packjpg,lepton,lepton-rust,komla-fast
python3 bench/run_jpeg_speed.py --set test ...   # and --set dev; --threads 1t,mt, --reps
python3 bench/run_paq8px.py --set heldout --jobs 5
python3 bench/make_jpeg_speed_report.py # writes bench/results/JPEG_SPEED_REPORT.md and the site's table
```

The fast model's learned starting distributions come from the development set:
`komla jtrain -o codec/src/jpeg/fast_priors.bin bench/data/jpeg/dev/*/*.jpg` (then rebuild).

`bench/select_jpegs.py` is the script that chose the Commons files (it queries Wikimedia
Commons; its output is `bench/datasets/jpeg_commons.json`). paq8px is slow (about 0.01 MB/s
per file on large photos); `PAQ8PX_JOBS` sets how many copies run at once (2.5 GB each).

Tests: `cargo test --release` (round trips, corrupt input, JPEG fixtures in both modes,
stored fallback, every single-byte corruption of a `.komla` JPEG file, fast-mode output
independent of the thread count), `node tests/cross_target.mjs` (native and WebAssembly give
identical files, for images and JPEGs in both modes), `python3 tests/make_edge_cases.py`
(1×1 images, 16-bit, palettes with transparency, interlaced PNG, BMP variants, JPEG variants
including progressive with restart markers, EXIF/ICC/comments, trailing data and a truncated
file), `node web/e2e/jpeg.mjs` (browser round trip of JPEG files).

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
codec/   the codec library (Rust): models, range coder, container, PNG/BMP I/O;
         codec/src/jpeg: JPEG parsing and bit-exact re-encoding (format.rs, and the fast
         scan encoder huff.rs), coefficient models (model.rs; fast mode: fast.rs and its
         learned starting distributions fast_priors.bin), container (mod.rs)
cli/     the komla command-line tool
wasm/    C-ABI WebAssembly bindings used by the web app
web/     Vite + React + Tailwind + shadcn/ui app (workers in web/src/workers)
bench/   baseline installers, dataset manifests and fetchers, benchmark runners, results
tests/   cross-target determinism test and edge-case generator
```

## License

MIT.
