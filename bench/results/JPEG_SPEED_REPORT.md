# Lossless JPEG recompression: size, speed and memory

Komla's two lossless JPEG modes against the leading lossless JPEG recompressors on three sets of JPEG files, single-threaded and on all eight cores:

- **Komla strong** (`komla compress --strong`, new in this round) puts this round's model in the fast mode's container. The model adds predicted signs and mantissas, adaptive edge and DC extrapolation, chroma from luma, averaged distributions with learned weights and activity contexts.
- **Komla fast** (`komla compress --fast`) is the fast model, essentially as in the previous evaluation (slightly smaller headers).
- **Komla fast (PR #2)**, the fast mode exactly as evaluated last time (commit 8430279), is included as the reference for the strong mode's budget: within about twice its time.

## Summary

**Was there a real size breakthrough?**

- **Against the practical lossless JPEG recompressors: yes.** On the new held-out set, run once after the code freeze, Komla strong made the JPEGs 24.73% smaller. The others:
  - Lepton's Rust port: 21.05%;
  - packJPG: 20.92%;
  - Dropbox's Lepton (all threads): 20.71%;
  - brunsli: 20.29%;
  - JPEG XL: 18.26%.

  Komla strong's files are 4.7% smaller than the best of them. It leads on sequential (baseline) files by 3.5 points and on progressive files by 4.5. The previous held-out set gives the same picture: 24.00% against 20.20% for packJPG and 20.12% for Lepton's Rust port. Komla's fast mode of the previous evaluation saved 20.95% and 19.88% on these sets.
- **Against paq8px: only partly.** Komla strong wins every set's total, by 1.73 points on the new held-out set and 3.34 on the previous one. That is only because paq8px has no model for progressive JPEGs (it saves 3.5% to 4.6% on them). On sequential JPEGs paq8px stays ahead by 2.39 points on the new held-out set (27.79% against 25.40%) and by 2.26 on the previous one, at roughly 600 to 3,300 times the time (paq8px's times are indicative).

**Speed and memory.** These are the practical limits Komla strong stays within, against the fast mode of the previous evaluation:

| Set | Compression time, 1 thread | Decompression time, 1 thread | Compression time, 8 cores | Decompression time, 8 cores |
|---|---|---|---|---|
| New held-out | 1.74x | 1.92x | 1.55x | 1.59x |
| Previous held-out | 1.70x | 1.91x | 1.48x | 1.50x |
| Development | 1.71x | 1.90x | 1.69x | 1.79x |

- **Memory:** at most 55 MiB to compress and 12 MiB to decompress with one thread, and 124 to 169 MiB with eight. That is 1.2 to 2.3 times the fast mode's.
  - On the held-out sets, with their large images, that is far below every other tool: 280 MiB or more to compress, and 238 MiB or more to decompress, except Dropbox's Lepton single-threaded at 16 to 19 MiB.
  - On the development set's smaller images, with eight cores, it is close to packJPG's and the Leptons'.
- **Speed against the other tools:**
  - **One thread:** Komla strong is slower than JPEG XL, Lepton's Rust port and brunsli (11% to 49% slower). It is faster than packJPG and paq8px, and faster than or as fast as Dropbox's Lepton.
  - **Eight cores, held-out sets:** it is within 12% slower to 7% faster than Lepton's Rust port, and faster than the rest.
- **Fast mode:** its speed is unchanged, and it now also beats packJPG, both Leptons, brunsli and JPEG XL on size on both held-out sets. Only paq8px is smaller, by 1.56 and 0.26 points.

**Win, tie and loss against the six other tools** (size, compression and decompression speed, memory for compression and decompression; 30 comparisons per thread setting):

| Set | Komla strong, 1 thread | Komla strong, 8 cores | Komla fast, 1 thread | Komla fast, 8 cores |
|---|---|---|---|---|
| New held-out | 24 / 0 / 6 | 28 / 1 / 1 | 28 / 1 / 1 | 29 / 0 / 1 |
| Development | 22 / 2 / 6 | 22 / 3 / 5 | 28 / 2 / 0 | 30 / 0 / 0 |
| Previous held-out | 23 / 1 / 6 | 29 / 1 / 0 | 29 / 0 / 1 | 29 / 0 / 1 |

- **Komla strong's losses** are speed: one-threaded against JPEG XL, Lepton's Rust port and brunsli, plus decompression with eight cores against Lepton's Rust port on the new held-out set (12% slower). On the development set's smaller images, eight threads have fewer stripes to share, so it also loses some multi-threaded speed and memory comparisons.
- **Komla fast's one loss** on the held-out sets is size against paq8px.

**Sets.**

- *New held-out random set* (66 files, 317.5 MB): drawn at the start of this round, before any work on the model.
  - **How it was drawn:** `bench/select_heldout.py draw --name heldout2 --seed 20261128 --exclude datasets/jpeg_heldout.json` took the files from the same recorded pool (`bench/datasets/jpeg_heldout_pool.json.gz`) as the previous held-out set. It used the same groups and checks, and no file was picked or rejected by hand.
  - **Exclusions:** the pool already excluded every development and test file. The 66 files of the previous held-out set were excluded as well.
  - **Frozen:** the files' SHA-256 hashes (`bench/datasets/jpeg_heldout2.sha256`) were committed before any work on the model and checked again right before the final run.
  - **Composition, from the file headers:**
    - 49 baseline and 17 progressive files.
    - Luma sampling: 4:4:4 in 28, 4:2:0 in 22, 4:2:2 in 8, grayscale in 8.
    - Restart markers in 17; 4 files do not end with the end-of-image marker (data after the image).
    - Sizes from 18 KB to 13.2 MB (median 5.1 MB), up to 83.6 megapixels (median 10.0).
  - **Run once:** Komla's code and parameters were frozen at commit 1924419 before Komla first ran on this set. It was then run exactly once, in the final evaluation below, with every tool.
- *Previous held-out random set* (66 files, 227.6 MB, seed 20261127): drawn for the previous evaluation, where Komla's fast mode as of PR #2 ran on it once. In this round no Komla version ran on it before the final evaluation, and it was not used for any decision.
- *Development set* (80 files, 97.4 MB): Kodak images encoded by libjpeg-turbo at quality 50, 75, 90 (4:4:4) and 95 (progressive), plus Wikimedia Commons camera originals, progressive, grayscale and screenshot JPEGs.
  - **Tuning:** all tuning used this set, with 2-fold cross-validation for every decision (starting distributions learned on one half, scored on the other).
  - **In-sample figures:** the shipped starting distributions are learned on all 80 files, so the figures for this set below are in-sample: 24.25% for the strong mode, against 24.17% cross-validated, and 21.16% for the fast mode.

**Data use.**

- **Tuning:** all tuning used the development set only.
- **New held-out set before the freeze:** only its composition, read from the file headers, was looked at.
- **paq8px:** it needs hours per set, so it ran on the new held-out set early in the round. Its results were committed without being looked at, and read only after the final run.

**Method.**

- **Interleaving:** every tool ran on every file one process at a time, all tools on one file before the next, so they saw the same machine conditions.
- **Threads:** each tool ran single-threaded ("1t") and at its default thread count on 8 cores ("mt"). Tools without threads repeat their single-threaded figures.
- **Speed:** each file was compressed and decompressed three times per tool and setting, and the fastest run counts. The time is the wall-clock time of the whole process, from the input file to the output file, on a RAM disk. All three runs are kept: the run-to-run spread column compares the total time of each tool's slowest and fastest pass.
- **Memory:** peak memory is the largest resident set (`/usr/bin/time`) of any file.
- **Round trips:** every restored file was compared with the original byte for byte (SHA-256). A file that a tool rejects, crashes on or restores wrongly counts at its original size and as a failure.
- **Units:** MB/s is megabytes of original JPEG per second, over the files the tool restored correctly.
- **paq8px** ran separately, several files at a time, so its times are indicative only.

**Machine.** One 8-core x86-64 virtual machine (Intel Xeon with AVX2; 48 KiB L1d and 2 MiB L2 per core), Linux 6.12, files on tmpfs. Virtual machines drift by up to about 15% in speed over hours, which is why the tools are interleaved per file. Margins under 5% in speed or memory are reported as ties.

**Caveats.**

- *Tuning risk.* The strong model has many more tuned parts than the fast one, all chosen on the development set:
  - contexts and constants;
  - least-squares prediction weights;
  - 1.4 MB of learned starting distributions.

  The held-out sets show no sign of overfitting. Over the previous fast mode, Komla strong gains 3.78 points on the new held-out set and 4.12 on the previous one, against 3.59 on the development set. Over the same reference, the fast mode gains 0.49, 0.52 and 0.50.
- *paq8px and progressive JPEGs.* paq8px has no model for progressive JPEGs (it saves only a few percent on them), so part of every tool's margin over paq8px on a set's total comes from its progressive files. The tables "Sequential and progressive files" split each set by frame type.
- *Small and mid-size images.*
  - **Fixed cost per file:** the strong mode's larger model costs about 3 ms per process (the fast mode's about 1.5 ms), so on very small files it is relatively slower than the totals show.
  - **Parallelism:** a stripe holds about 65,000 blocks, so an image below about 2.8 megapixels (4:2:0) is coded by one thread, and mid-size images by fewer threads than cores.
- *Scope.* Komla's slow maximum mode was not run again. The web app (not rebuilt in this round) still compresses in the fast mode only; the strong mode is in the command-line tool and the WebAssembly library.
- *Robustness.* The other tools are mature and have processed very large numbers of files; Komla's strong mode is new. Its safety net is the fast mode's: every file is fully re-encoded and compared during compression, and anything it cannot reproduce exactly is stored as it is (counted under "stored" below). A round trip therefore never fails, but such a file is not made smaller.
- *Settings.* JPEG XL runs at its default effort (7) with `--lossless_jpeg=1`; higher efforts make it smaller and slower. Lepton (Dropbox) in single-threaded mode fails its own round-trip check on many progressive files (counted at the original size); its multi-threaded mode restores them. brunsli, packJPG and paq8px have no threads.
- *Memory.* Peak memory is dominated by the largest images of a set, so it differs between sets with different maximum sizes.

## New held-out random set (heldout2)

66 files, 317,526,260 bytes; drawn at random for this evaluation and frozen by SHA-256 before any work on the model (see above); never used for tuning; Komla's code and parameters were frozen before its first run, and it was run once.

### One thread

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla strong** | 24.73% | 8.04 | 8.82 | 55 MiB | 12 MiB | 0 | 0.3% / 0.3% |
| **Komla fast** | 21.44% | 13.37 | 17.18 | 43 MiB | 7 MiB | 0 | 0.2% / 0.8% |
| Komla fast (PR #2) | 20.95% | 14.03 | 16.97 | 43 MiB | 7 MiB | 0 | 0.4% / 0.4% |
| JPEG XL | 18.26% | 9.24 | 17.27 | 1471 MiB | 295 MiB | 0 | 5.5% / 1.7% |
| Lepton (Dropbox) | 17.34% | 3.64 | 8.15 | 280 MiB | 16 MiB | 14 | 0.5% / 0.4% |
| Lepton (Rust port) | 21.05% | 11.70 | 10.75 | 281 MiB | 399 MiB | 2 | 0.3% / 0.5% |
| brunsli | 20.29% | 10.24 | 12.27 | 844 MiB | 286 MiB | 1 | 2.4% / 1.2% |
| packJPG | 20.92% | 3.75 | 3.58 | 283 MiB | 278 MiB | 1 | 0.4% / 0.2% |
| paq8px \* | 23.00% | 0.01 | 0.01 | 2553 MiB | 2553 MiB | 0 | n/a / n/a |

### All threads (8 cores)

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla strong** | 24.73% | 29.45 | 37.60 | 156 MiB | 150 MiB | 0 | 4.2% / 3.2% |
| **Komla fast** | 21.44% | 43.40 | 68.17 | 73 MiB | 78 MiB | 0 | 5.9% / 1.8% |
| Komla fast (PR #2) | 20.95% | 45.57 | 59.76 | 69 MiB | 77 MiB | 0 | 2.0% / 4.0% |
| JPEG XL | 18.26% | 16.11 | 33.35 | 1467 MiB | 307 MiB | 0 | 4.6% / 1.9% |
| Lepton (Dropbox) | 20.71% | 10.24 | 33.92 | 294 MiB | 238 MiB | 1 | 2.2% / 0.5% |
| Lepton (Rust port) | 20.73% | 30.88 | 42.73 | 291 MiB | 399 MiB | 2 | 1.1% / 0.3% |
| brunsli | 20.29% | 10.24 | 12.27 | 844 MiB | 286 MiB | 1 | 2.4% / 1.2% |
| packJPG | 20.92% | 3.75 | 3.58 | 283 MiB | 278 MiB | 1 | 0.4% / 0.2% |
| paq8px \* | 23.00% | 0.01 | 0.01 | 2553 MiB | 2553 MiB | 0 | n/a / n/a |

\* paq8px takes hours per set, so it ran separately (several files at a time, alongside other work, not interleaved with the other tools); its times are indicative only.

Files restored byte for byte: Komla strong 1t: 66/66; Komla strong mt: 66/66; Komla fast 1t: 66/66; Komla fast mt: 66/66; Komla fast (PR #2) 1t: 66/66; Komla fast (PR #2) mt: 66/66; JPEG XL 1t: 66/66; JPEG XL mt: 66/66; Lepton (Dropbox) 1t: 52/66; Lepton (Dropbox) mt: 65/66; Lepton (Rust port) 1t: 64/66; Lepton (Rust port) mt: 64/66; brunsli 1t: 65/66; packJPG 1t: 65/66.

Failures (counted at the original size): Lepton (Dropbox) (1t): 14 — 14× error (exit code 41); Lepton (Dropbox) (mt): 1 — 1× error (exit code 41); Lepton (Rust port) (1t): 2 — 2× error (exit code 42); Lepton (Rust port) (mt): 2 — 2× error (exit code 42); brunsli (1t): 1 — 1× error (exit code 1); brunsli (mt): 1 — 1× error (exit code 1); packJPG (1t): 1 — 1× error (exit code 1); packJPG (mt): 1 — 1× error (exit code 1).
No tool crashed or restored a file wrongly: every failure was the tool refusing the file (unsupported progressive scan scripts or end-of-band runs, or Lepton's own round-trip check failing).

Komla strong coded every file (no file fell back to being stored).

Komla fast coded every file (no file fell back to being stored).

### Komla strong against the fast mode

Time and peak memory of the strong mode as multiples of the fast mode of the previous evaluation (PR #2, the reference of the speed budget: within about twice its time) and of this version's fast mode.

| Threads | Compression time | Decompression time | Memory, compression | Memory, decompression |
|---|---|---|---|---|
| 1t, vs PR #2 fast | 1.74x | 1.92x | 1.29x | 1.74x |
| 1t, vs fast | 1.66x | 1.95x | 1.29x | 1.77x |
| mt, vs PR #2 fast | 1.55x | 1.59x | 2.25x | 1.93x |
| mt, vs fast | 1.47x | 1.81x | 2.13x | 1.92x |

### Komla strong against each tool

Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within 5% count as ties (margin: Komla relative to the tool).

| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |
|---|---|---|---|---|---|---|
| JPEG XL | 1t | win (+6.47 points) | loss (-13% speed) | loss (-49% speed) | win (-96% memory) | win (-96% memory) |
| JPEG XL | mt | win (+6.47 points) | win (+83% speed) | win (+13% speed) | win (-89% memory) | win (-51% memory) |
| Lepton (Dropbox) | 1t | win (+7.39 points) | win (+121% speed) | win (+8% speed) | win (-80% memory) | win (-25% memory) |
| Lepton (Dropbox) | mt | win (+4.01 points) | win (+188% speed) | win (+11% speed) | win (-47% memory) | win (-37% memory) |
| Lepton (Rust port) | 1t | win (+3.68 points) | loss (-31% speed) | loss (-18% speed) | win (-80% memory) | win (-97% memory) |
| Lepton (Rust port) | mt | win (+4.00 points) | tie (-5% speed) | loss (-12% speed) | win (-46% memory) | win (-63% memory) |
| brunsli | 1t | win (+4.44 points) | loss (-21% speed) | loss (-28% speed) | win (-93% memory) | win (-96% memory) |
| brunsli | mt | win (+4.44 points) | win (+188% speed) | win (3x speed) | win (-82% memory) | win (-48% memory) |
| packJPG | 1t | win (+3.80 points) | win (+115% speed) | win (+146% speed) | win (-81% memory) | win (-96% memory) |
| packJPG | mt | win (+3.80 points) | win (8x speed) | win (10x speed) | win (-45% memory) | win (-46% memory) |
| paq8px | 1t | win (+1.73 points) | win (602x speed) | win (667x speed) | win (-98% memory) | win (-100% memory) |
| paq8px | mt | win (+1.73 points) | win (2207x speed) | win (2842x speed) | win (-94% memory) | win (-94% memory) |

Komla strong against the other tools (Komla's own modes not counted): one thread: 24 wins, 0 ties, 6 losses; all threads: 28 wins, 1 tie, 1 loss.

### Komla fast against each tool

Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within 5% count as ties (margin: Komla relative to the tool).

| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |
|---|---|---|---|---|---|---|
| JPEG XL | 1t | win (+3.19 points) | win (+45% speed) | tie (-1% speed) | win (-97% memory) | win (-98% memory) |
| JPEG XL | mt | win (+3.19 points) | win (+169% speed) | win (+104% speed) | win (-95% memory) | win (-75% memory) |
| Lepton (Dropbox) | 1t | win (+4.11 points) | win (4x speed) | win (+111% speed) | win (-85% memory) | win (-57% memory) |
| Lepton (Dropbox) | mt | win (+0.73 points) | win (4x speed) | win (+101% speed) | win (-75% memory) | win (-67% memory) |
| Lepton (Rust port) | 1t | win (+0.40 points) | win (+14% speed) | win (+60% speed) | win (-85% memory) | win (-98% memory) |
| Lepton (Rust port) | mt | win (+0.71 points) | win (+41% speed) | win (+60% speed) | win (-75% memory) | win (-80% memory) |
| brunsli | 1t | win (+1.16 points) | win (+31% speed) | win (+40% speed) | win (-95% memory) | win (-98% memory) |
| brunsli | mt | win (+1.16 points) | win (4x speed) | win (6x speed) | win (-91% memory) | win (-73% memory) |
| packJPG | 1t | win (+0.52 points) | win (4x speed) | win (5x speed) | win (-85% memory) | win (-98% memory) |
| packJPG | mt | win (+0.52 points) | win (12x speed) | win (19x speed) | win (-74% memory) | win (-72% memory) |
| paq8px | 1t | loss (-1.56 points) | win (1002x speed) | win (1299x speed) | win (-98% memory) | win (-100% memory) |
| paq8px | mt | loss (-1.56 points) | win (3252x speed) | win (5154x speed) | win (-97% memory) | win (-97% memory) |

Komla fast against the other tools (Komla's own modes not counted): one thread: 28 wins, 1 tie, 1 loss; all threads: 29 wins, 0 ties, 1 loss.

### Size by group

| Tool | Camera photos<br>16 files | Grayscale<br>6 files | Edited photos<br>14 files | Progressive<br>10 files | Scans, paintings, maps<br>8 files | Screenshots<br>6 files | Small web images<br>6 files | **Total** |
|---|---|---|---|---|---|---|---|---|
| **Komla strong** | 24.28% | 29.07% | 26.29% | 21.88% | 24.22% | 40.10% | 32.78% | 24.73% |
| **Komla fast** | 21.12% | 25.87% | 23.22% | 17.92% | 21.28% | 33.08% | 28.75% | 21.44% |
| Komla fast (PR #2) | 20.66% | 25.11% | 22.73% | 17.43% | 20.67% | 32.17% | 27.80% | 20.95% |
| JPEG XL | 18.02% | 16.68% | 20.83% | 14.18% | 17.95% | 26.39% | 22.06% | 18.26% |
| Lepton (Dropbox) | 20.56% | 24.23% | 22.44% | 0.00% | 20.22% | 18.64% | 25.90% | 17.34% |
| Lepton (Rust port) | 20.89% | 24.94% | 22.78% | 17.52% | 20.88% | 24.61% | 24.53% | 21.05% |
| brunsli | 20.11% | 22.73% | 22.37% | 16.34% | 20.01% | 27.86% | 24.96% | 20.29% |
| packJPG | 20.74% | 23.71% | 22.83% | 17.32% | 20.62% | 24.46% | 26.43% | 20.92% |
| paq8px | 26.86% | 31.28% | 28.54% | 3.46% | 25.60% | 30.04% | 32.53% | 23.00% |

### Sequential and progressive files

paq8px has no model for progressive JPEGs, so its total depends on how many a set has. Sizes on the 49 sequential (baseline) and 17 progressive files, and on the 63 files every tool restored (failures count at the original size elsewhere). One thread, except Dropbox's Lepton (all threads: single-threaded, it refuses many progressive files).

| Tool | Sequential | Progressive | Total | Files every tool restored |
|---|---|---|---|---|
| **Komla strong** | 25.40% | 22.00% | 24.73% | 24.72% |
| **Komla fast** | 22.29% | 18.02% | 21.44% | 21.44% |
| JPEG XL | 19.26% | 14.20% | 18.26% | 18.26% |
| Lepton (Dropbox, all threads) | 21.58% | 17.18% | 20.71% | 20.73% |
| Lepton (Rust port) | 21.92% | 17.49% | 21.05% | 21.07% |
| brunsli | 21.26% | 16.32% | 20.29% | 20.29% |
| packJPG | 21.81% | 17.31% | 20.92% | 20.94% |
| paq8px | 27.79% | 3.51% | 23.00% | 23.02% |

## Previous held-out random set (heldout)

66 files, 227,559,408 bytes; drawn at random for the previous evaluation, where Komla's fast mode ran on it once; not used for tuning since.

### One thread

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla strong** | 24.00% | 7.14 | 8.03 | 50 MiB | 12 MiB | 0 | 0.5% / 0.5% |
| **Komla fast** | 20.40% | 11.60 | 15.39 | 42 MiB | 7 MiB | 0 | 0.4% / 1.0% |
| Komla fast (PR #2) | 19.88% | 12.15 | 15.35 | 42 MiB | 6 MiB | 0 | 0.2% / 0.1% |
| JPEG XL | 17.99% | 7.99 | 14.34 | 964 MiB | 274 MiB | 0 | 7.0% / 1.3% |
| Lepton (Dropbox) | 14.82% | 3.54 | 8.00 | 224 MiB | 19 MiB | 14 | 0.2% / 1.2% |
| Lepton (Rust port) | 20.12% | 10.02 | 9.14 | 265 MiB | 505 MiB | 0 | 0.3% / 0.5% |
| brunsli | 19.67% | 9.22 | 11.03 | 788 MiB | 266 MiB | 0 | 0.8% / 0.7% |
| packJPG | 20.20% | 3.43 | 3.32 | 271 MiB | 268 MiB | 0 | 0.2% / 0.4% |
| paq8px \* | 20.66% | 0.01 | 0.01 | 2791 MiB | 2791 MiB | 0 | n/a / n/a |

### All threads (8 cores)

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla strong** | 24.00% | 24.65 | 30.70 | 148 MiB | 169 MiB | 0 | 3.4% / 2.2% |
| **Komla fast** | 20.40% | 35.57 | 52.07 | 81 MiB | 78 MiB | 0 | 2.0% / 2.4% |
| Komla fast (PR #2) | 19.88% | 36.58 | 46.20 | 71 MiB | 77 MiB | 0 | 2.5% / 0.9% |
| JPEG XL | 17.99% | 13.87 | 25.84 | 958 MiB | 284 MiB | 0 | 0.0% / 0.6% |
| Lepton (Dropbox) | 19.78% | 8.51 | 24.38 | 290 MiB | 291 MiB | 0 | 1.6% / 0.2% |
| Lepton (Rust port) | 19.80% | 24.45 | 28.62 | 277 MiB | 505 MiB | 0 | 4.2% / 2.6% |
| brunsli | 19.67% | 9.22 | 11.03 | 788 MiB | 266 MiB | 0 | 0.8% / 0.7% |
| packJPG | 20.20% | 3.43 | 3.32 | 271 MiB | 268 MiB | 0 | 0.2% / 0.4% |
| paq8px \* | 20.66% | 0.01 | 0.01 | 2791 MiB | 2791 MiB | 0 | n/a / n/a |

\* paq8px takes hours per set, so it ran separately (several files at a time, alongside other work, not interleaved with the other tools); its times are indicative only.

Files restored byte for byte: Komla strong 1t: 66/66; Komla strong mt: 66/66; Komla fast 1t: 66/66; Komla fast mt: 66/66; Komla fast (PR #2) 1t: 66/66; Komla fast (PR #2) mt: 66/66; JPEG XL 1t: 66/66; JPEG XL mt: 66/66; Lepton (Dropbox) 1t: 52/66; Lepton (Dropbox) mt: 66/66; Lepton (Rust port) 1t: 66/66; Lepton (Rust port) mt: 66/66; brunsli 1t: 66/66; packJPG 1t: 66/66.

Failures (counted at the original size): Lepton (Dropbox) (1t): 14 — 14× error (exit code 41).
No tool crashed or restored a file wrongly: every failure was the tool refusing the file (unsupported progressive scan scripts or end-of-band runs, or Lepton's own round-trip check failing).

Komla strong coded every file (no file fell back to being stored).

Komla fast coded every file (no file fell back to being stored).

### Komla strong against the fast mode

Time and peak memory of the strong mode as multiples of the fast mode of the previous evaluation (PR #2, the reference of the speed budget: within about twice its time) and of this version's fast mode.

| Threads | Compression time | Decompression time | Memory, compression | Memory, decompression |
|---|---|---|---|---|
| 1t, vs PR #2 fast | 1.70x | 1.91x | 1.20x | 1.92x |
| 1t, vs fast | 1.62x | 1.92x | 1.18x | 1.73x |
| mt, vs PR #2 fast | 1.48x | 1.50x | 2.10x | 2.20x |
| mt, vs fast | 1.44x | 1.70x | 1.84x | 2.17x |

### Komla strong against each tool

Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within 5% count as ties (margin: Komla relative to the tool).

| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |
|---|---|---|---|---|---|---|
| JPEG XL | 1t | win (+6.01 points) | loss (-11% speed) | loss (-44% speed) | win (-95% memory) | win (-96% memory) |
| JPEG XL | mt | win (+6.01 points) | win (+78% speed) | win (+19% speed) | win (-84% memory) | win (-40% memory) |
| Lepton (Dropbox) | 1t | win (+9.18 points) | win (+102% speed) | tie (+0% speed) | win (-78% memory) | win (-37% memory) |
| Lepton (Dropbox) | mt | win (+4.22 points) | win (+190% speed) | win (+26% speed) | win (-49% memory) | win (-42% memory) |
| Lepton (Rust port) | 1t | win (+3.88 points) | loss (-29% speed) | loss (-12% speed) | win (-81% memory) | win (-98% memory) |
| Lepton (Rust port) | mt | win (+4.20 points) | tie (+1% speed) | win (+7% speed) | win (-46% memory) | win (-66% memory) |
| brunsli | 1t | win (+4.33 points) | loss (-23% speed) | loss (-27% speed) | win (-94% memory) | win (-96% memory) |
| brunsli | mt | win (+4.33 points) | win (+167% speed) | win (+178% speed) | win (-81% memory) | win (-36% memory) |
| packJPG | 1t | win (+3.81 points) | win (+108% speed) | win (+142% speed) | win (-82% memory) | win (-96% memory) |
| packJPG | mt | win (+3.81 points) | win (7x speed) | win (9x speed) | win (-45% memory) | win (-37% memory) |
| paq8px | 1t | win (+3.34 points) | win (768x speed) | win (859x speed) | win (-98% memory) | win (-100% memory) |
| paq8px | mt | win (+3.34 points) | win (2651x speed) | win (3286x speed) | win (-95% memory) | win (-94% memory) |

Komla strong against the other tools (Komla's own modes not counted): one thread: 23 wins, 1 tie, 6 losses; all threads: 29 wins, 1 tie, 0 losses.

### Komla fast against each tool

Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within 5% count as ties (margin: Komla relative to the tool).

| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |
|---|---|---|---|---|---|---|
| JPEG XL | 1t | win (+2.41 points) | win (+45% speed) | win (+7% speed) | win (-96% memory) | win (-98% memory) |
| JPEG XL | mt | win (+2.41 points) | win (+157% speed) | win (+102% speed) | win (-92% memory) | win (-73% memory) |
| Lepton (Dropbox) | 1t | win (+5.58 points) | win (3x speed) | win (+92% speed) | win (-81% memory) | win (-64% memory) |
| Lepton (Dropbox) | mt | win (+0.62 points) | win (4x speed) | win (+114% speed) | win (-72% memory) | win (-73% memory) |
| Lepton (Rust port) | 1t | win (+0.28 points) | win (+16% speed) | win (+68% speed) | win (-84% memory) | win (-99% memory) |
| Lepton (Rust port) | mt | win (+0.60 points) | win (+45% speed) | win (+82% speed) | win (-71% memory) | win (-85% memory) |
| brunsli | 1t | win (+0.73 points) | win (+26% speed) | win (+39% speed) | win (-95% memory) | win (-97% memory) |
| brunsli | mt | win (+0.73 points) | win (4x speed) | win (5x speed) | win (-90% memory) | win (-71% memory) |
| packJPG | 1t | win (+0.20 points) | win (3x speed) | win (5x speed) | win (-84% memory) | win (-97% memory) |
| packJPG | mt | win (+0.20 points) | win (10x speed) | win (16x speed) | win (-70% memory) | win (-71% memory) |
| paq8px | 1t | loss (-0.26 points) | win (1248x speed) | win (1647x speed) | win (-98% memory) | win (-100% memory) |
| paq8px | mt | loss (-0.26 points) | win (3825x speed) | win (5573x speed) | win (-97% memory) | win (-97% memory) |

Komla fast against the other tools (Komla's own modes not counted): one thread: 29 wins, 0 ties, 1 loss; all threads: 29 wins, 0 ties, 1 loss.

### Size by group

| Tool | Camera photos<br>16 files | Grayscale<br>6 files | Edited photos<br>14 files | Progressive<br>10 files | Scans, paintings, maps<br>8 files | Screenshots<br>6 files | Small web images<br>6 files | **Total** |
|---|---|---|---|---|---|---|---|---|
| **Komla strong** | 21.32% | 23.85% | 25.89% | 22.36% | 30.93% | 41.93% | 40.58% | 24.00% |
| **Komla fast** | 18.18% | 21.57% | 22.01% | 18.80% | 26.49% | 35.09% | 35.72% | 20.40% |
| Komla fast (PR #2) | 17.70% | 20.88% | 21.49% | 18.29% | 25.72% | 33.81% | 34.72% | 19.88% |
| JPEG XL | 15.71% | 18.41% | 20.60% | 14.88% | 22.33% | 27.77% | 28.40% | 17.99% |
| Lepton (Dropbox) | 16.07% | 19.27% | 20.87% | 0.00% | 15.54% | 33.20% | 33.23% | 14.82% |
| Lepton (Rust port) | 18.16% | 20.36% | 21.87% | 17.95% | 26.23% | 33.29% | 34.22% | 20.12% |
| brunsli | 17.31% | 20.47% | 21.86% | 17.43% | 24.23% | 30.47% | 31.55% | 19.67% |
| packJPG | 17.88% | 20.35% | 22.43% | 17.68% | 25.59% | 32.60% | 33.69% | 20.20% |
| paq8px | 21.15% | 24.12% | 27.76% | 4.68% | 21.18% | 42.63% | 44.60% | 20.66% |

### Sequential and progressive files

paq8px has no model for progressive JPEGs, so its total depends on how many a set has. Sizes on the 50 sequential (baseline) and 16 progressive files, and on the 66 files every tool restored (failures count at the original size elsewhere). One thread, except Dropbox's Lepton (all threads: single-threaded, it refuses many progressive files).

| Tool | Sequential | Progressive | Total | Files every tool restored |
|---|---|---|---|---|
| **Komla strong** | 24.87% | 21.84% | 24.00% | 24.00% |
| **Komla fast** | 21.38% | 17.96% | 20.40% | 20.40% |
| JPEG XL | 19.11% | 15.20% | 17.99% | 17.99% |
| Lepton (Dropbox, all threads) | 20.76% | 17.34% | 19.78% | 19.78% |
| Lepton (Rust port) | 21.10% | 17.68% | 20.12% | 20.12% |
| brunsli | 20.74% | 17.00% | 19.67% | 19.67% |
| packJPG | 21.28% | 17.51% | 20.20% | 20.20% |
| paq8px | 27.13% | 4.58% | 20.66% | 20.66% |

## Development set (dev)

80 files, 97,380,012 bytes; used for all tuning of Komla (contexts, priors, parameters).

### One thread

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla strong** | 24.25% | 7.70 | 8.64 | 35 MiB | 12 MiB | 0 | 0.5% / 0.6% |
| **Komla fast** | 21.16% | 12.42 | 16.27 | 30 MiB | 7 MiB | 0 | 0.6% / 2.4% |
| Komla fast (PR #2) | 20.66% | 13.16 | 16.37 | 30 MiB | 6 MiB | 0 | 0.6% / 1.0% |
| JPEG XL | 16.57% | 9.31 | 16.33 | 568 MiB | 127 MiB | 0 | 12.1% / 1.1% |
| Lepton (Dropbox) | 15.25% | 3.71 | 8.29 | 112 MiB | 12 MiB | 16 | 0.3% / 1.2% |
| Lepton (Rust port) | 18.63% | 11.83 | 10.85 | 122 MiB | 226 MiB | 4 | 0.2% / 1.4% |
| brunsli | 19.42% | 10.01 | 12.07 | 370 MiB | 125 MiB | 0 | 1.4% / 0.7% |
| packJPG | 19.31% | 3.67 | 3.50 | 123 MiB | 121 MiB | 1 | 0.4% / 0.2% |
| paq8px \* | 20.75% | 0.01 | 0.01 | 2751 MiB | 2751 MiB | 0 | n/a / n/a |

### All threads (8 cores)

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla strong** | 24.25% | 18.49 | 22.59 | 130 MiB | 124 MiB | 0 | 0.9% / 2.7% |
| **Komla fast** | 21.16% | 30.19 | 43.78 | 62 MiB | 75 MiB | 0 | 4.9% / 3.2% |
| Komla fast (PR #2) | 20.66% | 31.29 | 40.34 | 63 MiB | 74 MiB | 0 | 1.2% / 1.0% |
| JPEG XL | 16.57% | 13.69 | 28.10 | 570 MiB | 132 MiB | 0 | 3.2% / 2.2% |
| Lepton (Dropbox) | 19.14% | 8.51 | 24.11 | 143 MiB | 144 MiB | 1 | 0.1% / 2.0% |
| Lepton (Rust port) | 18.22% | 28.49 | 36.31 | 132 MiB | 226 MiB | 4 | 1.0% / 2.9% |
| brunsli | 19.42% | 10.01 | 12.07 | 370 MiB | 125 MiB | 0 | 1.4% / 0.7% |
| packJPG | 19.31% | 3.67 | 3.50 | 123 MiB | 121 MiB | 1 | 0.4% / 0.2% |
| paq8px \* | 20.75% | 0.01 | 0.01 | 2751 MiB | 2751 MiB | 0 | n/a / n/a |

\* paq8px takes hours per set, so it ran separately (several files at a time, alongside other work, not interleaved with the other tools); its times are indicative only.

Files restored byte for byte: Komla strong 1t: 80/80; Komla strong mt: 80/80; Komla fast 1t: 80/80; Komla fast mt: 80/80; Komla fast (PR #2) 1t: 80/80; Komla fast (PR #2) mt: 80/80; JPEG XL 1t: 80/80; JPEG XL mt: 80/80; Lepton (Dropbox) 1t: 64/80; Lepton (Dropbox) mt: 79/80; Lepton (Rust port) 1t: 76/80; Lepton (Rust port) mt: 76/80; brunsli 1t: 80/80; packJPG 1t: 79/80.

Failures (counted at the original size): Lepton (Dropbox) (1t): 16 — 16× error (exit code 41); Lepton (Dropbox) (mt): 1 — 1× error (exit code 41); Lepton (Rust port) (1t): 4 — 4× error (exit code 42); Lepton (Rust port) (mt): 4 — 4× error (exit code 42); packJPG (1t): 1 — 1× error (exit code 1); packJPG (mt): 1 — 1× error (exit code 1).
No tool crashed or restored a file wrongly: every failure was the tool refusing the file (unsupported progressive scan scripts or end-of-band runs, or Lepton's own round-trip check failing).

Komla strong coded every file (no file fell back to being stored).

Komla fast coded every file (no file fell back to being stored).

### Komla strong against the fast mode

Time and peak memory of the strong mode as multiples of the fast mode of the previous evaluation (PR #2, the reference of the speed budget: within about twice its time) and of this version's fast mode.

| Threads | Compression time | Decompression time | Memory, compression | Memory, decompression |
|---|---|---|---|---|
| 1t, vs PR #2 fast | 1.71x | 1.90x | 1.19x | 2.04x |
| 1t, vs fast | 1.61x | 1.88x | 1.18x | 1.69x |
| mt, vs PR #2 fast | 1.69x | 1.79x | 2.06x | 1.68x |
| mt, vs fast | 1.63x | 1.94x | 2.08x | 1.65x |

### Komla strong against each tool

Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within 5% count as ties (margin: Komla relative to the tool).

| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |
|---|---|---|---|---|---|---|
| JPEG XL | 1t | win (+7.68 points) | loss (-17% speed) | loss (-47% speed) | win (-94% memory) | win (-91% memory) |
| JPEG XL | mt | win (+7.68 points) | win (+35% speed) | loss (-20% speed) | win (-77% memory) | win (-7% memory) |
| Lepton (Dropbox) | 1t | win (+9.00 points) | win (+108% speed) | tie (+4% speed) | win (-69% memory) | tie (-1% memory) |
| Lepton (Dropbox) | mt | win (+5.12 points) | win (+117% speed) | loss (-6% speed) | win (-10% memory) | win (-14% memory) |
| Lepton (Rust port) | 1t | win (+5.62 points) | loss (-35% speed) | loss (-20% speed) | win (-71% memory) | win (-95% memory) |
| Lepton (Rust port) | mt | win (+6.04 points) | loss (-35% speed) | loss (-38% speed) | tie (-2% memory) | win (-45% memory) |
| brunsli | 1t | win (+4.83 points) | loss (-23% speed) | loss (-28% speed) | win (-90% memory) | win (-90% memory) |
| brunsli | mt | win (+4.83 points) | win (+85% speed) | win (+87% speed) | win (-65% memory) | tie (-1% memory) |
| packJPG | 1t | win (+4.95 points) | win (+110% speed) | win (+147% speed) | win (-71% memory) | win (-90% memory) |
| packJPG | mt | win (+4.95 points) | win (5x speed) | win (6x speed) | loss (+5% memory) | tie (+2% memory) |
| paq8px | 1t | win (+3.50 points) | win (769x speed) | win (854x speed) | win (-99% memory) | win (-100% memory) |
| paq8px | mt | win (+3.50 points) | win (1848x speed) | win (2235x speed) | win (-95% memory) | win (-96% memory) |

Komla strong against the other tools (Komla's own modes not counted): one thread: 22 wins, 2 ties, 6 losses; all threads: 22 wins, 3 ties, 5 losses.

### Komla fast against each tool

Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within 5% count as ties (margin: Komla relative to the tool).

| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |
|---|---|---|---|---|---|---|
| JPEG XL | 1t | win (+4.59 points) | win (+33% speed) | tie (-0% speed) | win (-95% memory) | win (-94% memory) |
| JPEG XL | mt | win (+4.59 points) | win (+120% speed) | win (+56% speed) | win (-89% memory) | win (-43% memory) |
| Lepton (Dropbox) | 1t | win (+5.91 points) | win (3x speed) | win (+96% speed) | win (-73% memory) | win (-42% memory) |
| Lepton (Dropbox) | mt | win (+2.02 points) | win (4x speed) | win (+82% speed) | win (-56% memory) | win (-48% memory) |
| Lepton (Rust port) | 1t | win (+2.53 points) | tie (+5% speed) | win (+50% speed) | win (-76% memory) | win (-97% memory) |
| Lepton (Rust port) | mt | win (+2.94 points) | win (+6% speed) | win (+21% speed) | win (-53% memory) | win (-67% memory) |
| brunsli | 1t | win (+1.74 points) | win (+24% speed) | win (+35% speed) | win (-92% memory) | win (-94% memory) |
| brunsli | mt | win (+1.74 points) | win (3x speed) | win (4x speed) | win (-83% memory) | win (-40% memory) |
| packJPG | 1t | win (+1.85 points) | win (3x speed) | win (5x speed) | win (-76% memory) | win (-94% memory) |
| packJPG | mt | win (+1.85 points) | win (8x speed) | win (13x speed) | win (-49% memory) | win (-38% memory) |
| paq8px | 1t | win (+0.40 points) | win (1241x speed) | win (1610x speed) | win (-99% memory) | win (-100% memory) |
| paq8px | mt | win (+0.40 points) | win (3017x speed) | win (4332x speed) | win (-98% memory) | win (-97% memory) |

Komla fast against the other tools (Komla's own modes not counted): one thread: 28 wins, 2 ties, 0 losses; all threads: 30 wins, 0 ties, 0 losses.

### Size by group

| Tool | Camera photos<br>12 files | Grayscale<br>6 files | Kodak q50<br>12 files | Kodak q75<br>12 files | Kodak q90 4:4:4<br>12 files | Kodak q95 progressive<br>12 files | Progressive<br>8 files | Screenshots<br>6 files | **Total** |
|---|---|---|---|---|---|---|---|---|---|
| **Komla strong** | 24.51% | 23.05% | 28.04% | 24.15% | 24.18% | 16.72% | 23.80% | 29.36% | 24.25% |
| **Komla fast** | 21.61% | 20.58% | 24.55% | 20.99% | 20.88% | 14.24% | 20.12% | 25.34% | 21.16% |
| Komla fast (PR #2) | 21.17% | 20.12% | 23.54% | 20.35% | 20.47% | 13.80% | 19.47% | 24.62% | 20.66% |
| JPEG XL | 17.04% | 17.26% | 15.92% | 13.49% | 14.62% | 9.05% | 15.44% | 20.60% | 16.57% |
| Lepton (Dropbox) | 20.33% | 19.44% | 22.44% | 18.71% | 18.17% | 3.66% | 0.00% | 24.21% | 15.25% |
| Lepton (Rust port) | 20.75% | 19.96% | 22.37% | 18.71% | 18.63% | 11.96% | 12.30% | 25.12% | 18.63% |
| brunsli | 20.00% | 19.53% | 20.27% | 16.64% | 17.08% | 11.35% | 18.16% | 23.84% | 19.42% |
| packJPG | 20.44% | 19.78% | 23.59% | 19.27% | 18.96% | 12.44% | 15.95% | 24.85% | 19.31% |
| paq8px | 26.31% | 24.95% | 28.45% | 24.20% | 24.91% | 1.00% | 4.47% | 32.27% | 20.75% |

### Sequential and progressive files

paq8px has no model for progressive JPEGs, so its total depends on how many a set has. Sizes on the 60 sequential (baseline) and 20 progressive files, and on the 76 files every tool restored (failures count at the original size elsewhere). One thread, except Dropbox's Lepton (all threads: single-threaded, it refuses many progressive files).

| Tool | Sequential | Progressive | Total | Files every tool restored |
|---|---|---|---|---|
| **Komla strong** | 24.59% | 23.28% | 24.25% | 24.51% |
| **Komla fast** | 21.66% | 19.68% | 21.16% | 21.52% |
| JPEG XL | 17.12% | 14.96% | 16.57% | 17.02% |
| Lepton (Dropbox, all threads) | 20.36% | 15.56% | 19.14% | 20.22% |
| Lepton (Rust port) | 20.80% | 12.28% | 18.63% | 20.67% |
| brunsli | 20.02% | 17.66% | 19.42% | 19.85% |
| packJPG | 20.54% | 15.69% | 19.31% | 20.42% |
| paq8px | 26.39% | 4.21% | 20.75% | 22.73% |

## Tools

| Tool | Version and command |
|---|---|
| Komla strong | this repository: `komla compress --strong`, `komla decompress`; `KOMLA_THREADS=1` for one thread, else all cores |
| Komla fast | this repository: `komla compress --fast`, `komla decompress` (same thread settings) |
| Komla fast (PR #2) | the fast mode as of the previous evaluation (commit 8430279), the reference for the strong mode's speed budget (same commands) |
| JPEG XL | libjxl 0.13.0: `cjxl --lossless_jpeg=1` at its default effort (7), `djxl`; `--num_threads=0` for one thread, else its default |
| Lepton (Dropbox) | dropbox/lepton f34c7f4: `lepton -unjailed -allowprogressive -memory=4096M -threadmemory=2048M`; `-singlethread` for one thread |
| Lepton (Rust port) | microsoft/lepton_jpeg_rust 0.5.9: `lepton_jpeg_util --noverify`; `--threads 1` for one thread |
| brunsli | google/brunsli c18b962: `cbrunsli`, `dbrunsli` (no threads) |
| packJPG | packJPG 2.5n: `packJPG -np -o` (no threads) |
| paq8px | hxim/paq8px v217: `paq8px -8`, `paq8px -d` (no threads) |
