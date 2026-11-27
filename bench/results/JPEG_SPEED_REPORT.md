# Lossless JPEG recompression: size, speed and memory

Komla's fast JPEG mode (`komla compress --fast`) against the leading lossless JPEG recompressors on three sets of JPEG files, single-threaded and on all eight cores.

**Sets.**

- *Development set* (80 files, 97.4 MB): Kodak images (odd numbers) encoded by libjpeg-turbo at quality 50, 75, 90 (4:4:4) and 95 (progressive), and Wikimedia Commons camera originals, progressive, grayscale and screenshot JPEGs. All tuning of Komla (contexts, learned starting distributions, parameters) used this set only.
- *Test set* (80 files, 89.3 MB): the other half of the same sources. It was held out from tuning, but Komla's fast mode has been run on it before (in the previous evaluation and again here), so it is no longer untouched.
- *Held-out random set* (66 files, 227.6 MB): drawn for this evaluation by `bench/select_heldout.py` with seed 20261127 from a recorded pool (`bench/datasets/jpeg_heldout_pool.json.gz`, 9,999 entries) of freely licensed JPEG files in 30 Wikimedia Commons categories: camera originals from 16 camera and phone models not used before, quality, valued and featured pictures, progressive JPEGs, black-and-white and monochrome photographs, screenshots, scans (old photographs, paintings, postcards, documents, maps) and small web images (10–300 KB). Every file of the development and test sets was excluded from the pool by SHA-1 and by title. Each group's pool was shuffled with the seed and walked in that order; a file was kept if it passed the group's check (camera: EXIF make and model and three components; progressive: progressive frame; grayscale: one component; others: any JPEG). No file was picked or rejected by hand. The draw has 50 baseline and 16 progressive files; luma sampling 4:4:4 in 29, 4:2:0 in 23, 4:2:2 in 4, 1×2 in 1, grayscale in 9; restart markers in 14 and data after the end of the image in 3; sizes from 35 KB to 13.5 MB (median 2.6 MB), up to 41.7 megapixels (median 7.6). Komla's code and parameters were frozen at commit 8430279 (the fast model as of 8ff20d7) before the held-out set was first run; it was run exactly once, in the final evaluation below, with every tool.

**Data use.** All tuning used the development set only. During this round the test set's results from the previous evaluation were known (they showed where Komla trailed), but no test file was run until the final evaluation. Of the held-out set, only its composition (frame types, sampling, sizes and markers, read from the file headers) was looked at before the freeze; Komla first ran on it in the final evaluation, once. paq8px was started on the held-out set before the freeze because it needs hours; its results were looked at only afterwards.

**Method.** Every tool ran on every file one process at a time, all tools on one file before the next (so they saw the same machine conditions), single-threaded ("1t") and at the tool's default thread count on 8 cores ("mt"; tools without threads repeat their single-threaded figures). Each file was compressed and decompressed three times per tool and setting; the fastest run counts for the speed (wall-clock time of the whole process, from writing the input file to having the output file, on a RAM disk), all three runs are kept, and the run-to-run spread column compares the total time of each tool's slowest and fastest of the three passes. Peak memory is the largest resident set (`/usr/bin/time`) of any file. Every restored file was compared with the original byte for byte (SHA-256); a file a tool rejects, crashes on or restores wrongly counts at its original size and as a failure. MB/s is megabytes of original JPEG per second, total over the files the tool restored correctly. paq8px needs hours per set, so it ran separately, several files at a time; its times are indicative only.

**Machine.** One 8-core x86-64 virtual machine (Intel Xeon with AVX2; 48 KiB L1d and 2 MiB L2 per core), Linux 6.12, files on tmpfs. Virtual machines drift by up to about 15% in speed over hours, which is why the tools are interleaved per file; margins under 5% in speed or memory are reported as ties.

**Caveats.**

- *Tuning risk.* Komla was tuned on the development set; compare its development-set figures with the held-out set to see how much of the margin is overfitting. The learned starting distributions (272 KB compiled into the program) are the largest tuned part.
- *Robustness.* The other tools are mature and have processed very large numbers of files; Komla's fast mode is new. Its safety net: every file is fully re-encoded and compared during compression, and anything it cannot reproduce exactly is stored as it is (counted under "stored" below), so a round trip never fails, but such a file is not made smaller.
- *Settings.* JPEG XL runs at its default effort (7) with `--lossless_jpeg=1`; higher efforts make it smaller and slower. Lepton (Dropbox) in single-threaded mode fails its own round-trip check on many progressive files (counted at the original size); its multi-threaded mode restores them. brunsli, packJPG and paq8px have no threads.
- *Memory.* Peak memory is dominated by the largest images of a set, so it differs between sets with different maximum sizes.

## Held-out random set (heldout)

66 files, 227,559,408 bytes; drawn at random for this evaluation and never used before it (see below); Komla's code and parameters were frozen before its first run.

### One thread

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla fast** | 19.88% | 11.66 | 14.78 | 42 MiB | 6 MiB | 0 | 0.5% / 1.6% |
| JPEG XL | 17.99% | 7.35 | 13.57 | 964 MiB | 274 MiB | 0 | 1.7% / 1.3% |
| Lepton (Dropbox) | 14.82% | 3.40 | 7.74 | 224 MiB | 19 MiB | 14 | 0.7% / 0.8% |
| Lepton (Rust port) | 20.12% | 9.52 | 8.68 | 265 MiB | 505 MiB | 0 | 1.0% / 0.4% |
| brunsli | 19.67% | 8.60 | 10.46 | 788 MiB | 266 MiB | 0 | 0.1% / 1.0% |
| packJPG | 20.20% | 3.28 | 3.18 | 271 MiB | 268 MiB | 0 | 2.2% / 1.2% |
| paq8px \* | 20.66% | 0.01 | 0.01 | 2791 MiB | 2791 MiB | 0 | n/a / n/a |

### All threads (8 cores)

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla fast** | 19.88% | 34.01 | 41.40 | 80 MiB | 77 MiB | 0 | 2.2% / 3.2% |
| JPEG XL | 17.99% | 12.31 | 23.39 | 957 MiB | 286 MiB | 0 | 3.7% / 2.3% |
| Lepton (Dropbox) | 19.78% | 7.50 | 20.89 | 290 MiB | 291 MiB | 0 | 2.5% / 1.0% |
| Lepton (Rust port) | 19.80% | 21.37 | 24.80 | 277 MiB | 505 MiB | 0 | 1.1% / 1.7% |
| brunsli | 19.67% | 8.60 | 10.46 | 788 MiB | 266 MiB | 0 | 0.1% / 1.0% |
| packJPG | 20.20% | 3.28 | 3.18 | 271 MiB | 268 MiB | 0 | 2.2% / 1.2% |
| paq8px \* | 20.66% | 0.01 | 0.01 | 2791 MiB | 2791 MiB | 0 | n/a / n/a |

\* paq8px takes hours per set, so it ran separately (several files at a time, alongside other work, not interleaved with the other tools); its times are indicative only.

Files restored byte for byte: Komla fast 1t: 66/66; Komla fast mt: 66/66; JPEG XL 1t: 66/66; JPEG XL mt: 66/66; Lepton (Dropbox) 1t: 52/66; Lepton (Dropbox) mt: 66/66; Lepton (Rust port) 1t: 66/66; Lepton (Rust port) mt: 66/66; brunsli 1t: 66/66; packJPG 1t: 66/66.

Failures (counted at the original size): Lepton (Dropbox) (1t): 14 — 14× error (exit code 41).
No tool crashed or restored a file wrongly: every failure was the tool refusing the file (unsupported progressive scan scripts or end-of-band runs, or Lepton's own round-trip check failing).

Komla coded every file in its fast mode (no file fell back to being stored).

### Komla fast against each tool

Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within 5% count as ties (margin: Komla relative to the tool).

| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |
|---|---|---|---|---|---|---|
| JPEG XL | 1t | win (+1.89 points) | win (+59% speed) | win (+9% speed) | win (-96% memory) | win (-98% memory) |
| JPEG XL | mt | win (+1.89 points) | win (+176% speed) | win (+77% speed) | win (-92% memory) | win (-73% memory) |
| Lepton (Dropbox) | 1t | win (+5.06 points) | win (3x speed) | win (+91% speed) | win (-81% memory) | win (-68% memory) |
| Lepton (Dropbox) | mt | win (+0.10 points) | win (5x speed) | win (+98% speed) | win (-72% memory) | win (-74% memory) |
| Lepton (Rust port) | 1t | loss (-0.24 points) | win (+22% speed) | win (+70% speed) | win (-84% memory) | win (-99% memory) |
| Lepton (Rust port) | mt | win (+0.08 points) | win (+59% speed) | win (+67% speed) | win (-71% memory) | win (-85% memory) |
| brunsli | 1t | win (+0.21 points) | win (+36% speed) | win (+41% speed) | win (-95% memory) | win (-98% memory) |
| brunsli | mt | win (+0.21 points) | win (4x speed) | win (4x speed) | win (-90% memory) | win (-71% memory) |
| packJPG | 1t | loss (-0.32 points) | win (4x speed) | win (5x speed) | win (-85% memory) | win (-98% memory) |
| packJPG | mt | loss (-0.32 points) | win (10x speed) | win (13x speed) | win (-71% memory) | win (-71% memory) |
| paq8px | 1t | loss (-0.78 points) | win (1253x speed) | win (1582x speed) | win (-99% memory) | win (-100% memory) |
| paq8px | mt | loss (-0.78 points) | win (3657x speed) | win (4431x speed) | win (-97% memory) | win (-97% memory) |

Against the other tools (Komla's own maximum mode not counted): one thread: 27 wins, 0 ties, 3 losses; all threads: 28 wins, 0 ties, 2 losses.

### Size by group

| Tool | Camera photos<br>16 files | Grayscale<br>6 files | Edited photos<br>14 files | Progressive<br>10 files | Scans, paintings, maps<br>8 files | Screenshots<br>6 files | Small web images<br>6 files | **Total** |
|---|---|---|---|---|---|---|---|---|
| **Komla fast** | 17.70% | 20.88% | 21.49% | 18.29% | 25.72% | 33.81% | 34.72% | 19.88% |
| JPEG XL | 15.71% | 18.41% | 20.60% | 14.88% | 22.33% | 27.77% | 28.40% | 17.99% |
| Lepton (Dropbox) | 16.07% | 19.27% | 20.87% | 0.00% | 15.54% | 33.20% | 33.23% | 14.82% |
| Lepton (Rust port) | 18.16% | 20.36% | 21.87% | 17.95% | 26.23% | 33.29% | 34.22% | 20.12% |
| brunsli | 17.31% | 20.47% | 21.86% | 17.43% | 24.23% | 30.47% | 31.55% | 19.67% |
| packJPG | 17.88% | 20.35% | 22.43% | 17.68% | 25.59% | 32.60% | 33.69% | 20.20% |
| paq8px | 21.15% | 24.12% | 27.76% | 4.68% | 21.18% | 42.63% | 44.60% | 20.66% |

## Development set (dev)

80 files, 97,380,012 bytes; used for all tuning of Komla (contexts, priors, parameters).

### One thread

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla fast** | 20.66% | 12.98 | 16.20 | 30 MiB | 6 MiB | 0 | 0.8% / 1.8% |
| JPEG XL | 16.57% | 9.02 | 16.10 | 567 MiB | 127 MiB | 0 | 1.6% / 1.2% |
| Lepton (Dropbox) | 15.25% | 3.68 | 8.20 | 112 MiB | 12 MiB | 16 | 0.1% / 1.6% |
| Lepton (Rust port) | 18.63% | 11.62 | 10.66 | 122 MiB | 226 MiB | 4 | 1.4% / 2.4% |
| brunsli | 19.42% | 9.82 | 11.81 | 370 MiB | 125 MiB | 0 | 1.1% / 1.7% |
| packJPG | 19.31% | 3.62 | 3.46 | 123 MiB | 121 MiB | 1 | 0.9% / 0.3% |
| paq8px \* | 20.75% | 0.01 | 0.01 | 2751 MiB | 2751 MiB | 0 | n/a / n/a |

### All threads (8 cores)

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla fast** | 20.66% | 30.30 | 37.76 | 62 MiB | 72 MiB | 0 | 5.3% / 2.0% |
| JPEG XL | 16.57% | 13.15 | 26.89 | 570 MiB | 132 MiB | 0 | 3.4% / 1.4% |
| Lepton (Dropbox) | 19.14% | 7.82 | 21.59 | 144 MiB | 144 MiB | 1 | 0.8% / 3.2% |
| Lepton (Rust port) | 18.22% | 26.26 | 31.95 | 132 MiB | 226 MiB | 4 | 2.1% / 0.8% |
| brunsli | 19.42% | 9.82 | 11.81 | 370 MiB | 125 MiB | 0 | 1.1% / 1.7% |
| packJPG | 19.31% | 3.62 | 3.46 | 123 MiB | 121 MiB | 1 | 0.9% / 0.3% |
| paq8px \* | 20.75% | 0.01 | 0.01 | 2751 MiB | 2751 MiB | 0 | n/a / n/a |

\* paq8px takes hours per set, so it ran separately (several files at a time, alongside other work, not interleaved with the other tools); its times are indicative only.

Files restored byte for byte: Komla fast 1t: 80/80; Komla fast mt: 80/80; JPEG XL 1t: 80/80; JPEG XL mt: 80/80; Lepton (Dropbox) 1t: 64/80; Lepton (Dropbox) mt: 79/80; Lepton (Rust port) 1t: 76/80; Lepton (Rust port) mt: 76/80; brunsli 1t: 80/80; packJPG 1t: 79/80.

Failures (counted at the original size): Lepton (Dropbox) (1t): 16 — 16× error (exit code 41); Lepton (Dropbox) (mt): 1 — 1× error (exit code 41); Lepton (Rust port) (1t): 4 — 4× error (exit code 42); Lepton (Rust port) (mt): 4 — 4× error (exit code 42); packJPG (1t): 1 — 1× error (exit code 1); packJPG (mt): 1 — 1× error (exit code 1).
No tool crashed or restored a file wrongly: every failure was the tool refusing the file (unsupported progressive scan scripts or end-of-band runs, or Lepton's own round-trip check failing).

Komla coded every file in its fast mode (no file fell back to being stored).

### Komla fast against each tool

Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within 5% count as ties (margin: Komla relative to the tool).

| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |
|---|---|---|---|---|---|---|
| JPEG XL | 1t | win (+4.09 points) | win (+44% speed) | tie (+1% speed) | win (-95% memory) | win (-95% memory) |
| JPEG XL | mt | win (+4.09 points) | win (+131% speed) | win (+40% speed) | win (-89% memory) | win (-46% memory) |
| Lepton (Dropbox) | 1t | win (+5.41 points) | win (4x speed) | win (+98% speed) | win (-73% memory) | win (-51% memory) |
| Lepton (Dropbox) | mt | win (+1.52 points) | win (4x speed) | win (+75% speed) | win (-57% memory) | win (-50% memory) |
| Lepton (Rust port) | 1t | win (+2.02 points) | win (+12% speed) | win (+52% speed) | win (-76% memory) | win (-97% memory) |
| Lepton (Rust port) | mt | win (+2.44 points) | win (+15% speed) | win (+18% speed) | win (-54% memory) | win (-68% memory) |
| brunsli | 1t | win (+1.24 points) | win (+32% speed) | win (+37% speed) | win (-92% memory) | win (-95% memory) |
| brunsli | mt | win (+1.24 points) | win (3x speed) | win (3x speed) | win (-83% memory) | win (-43% memory) |
| packJPG | 1t | win (+1.35 points) | win (4x speed) | win (5x speed) | win (-76% memory) | win (-95% memory) |
| packJPG | mt | win (+1.35 points) | win (8x speed) | win (11x speed) | win (-50% memory) | win (-41% memory) |
| paq8px | 1t | loss (-0.10 points) | win (1298x speed) | win (1602x speed) | win (-99% memory) | win (-100% memory) |
| paq8px | mt | loss (-0.10 points) | win (3029x speed) | win (3736x speed) | win (-98% memory) | win (-97% memory) |

Against the other tools (Komla's own maximum mode not counted): one thread: 28 wins, 1 tie, 1 loss; all threads: 29 wins, 0 ties, 1 loss.

### Size by group

| Tool | Camera photos<br>12 files | Grayscale<br>6 files | Kodak q50<br>12 files | Kodak q75<br>12 files | Kodak q90 4:4:4<br>12 files | Kodak q95 progressive<br>12 files | Progressive<br>8 files | Screenshots<br>6 files | **Total** |
|---|---|---|---|---|---|---|---|---|---|
| **Komla fast** | 21.17% | 20.12% | 23.54% | 20.35% | 20.47% | 13.80% | 19.47% | 24.62% | 20.66% |
| JPEG XL | 17.04% | 17.26% | 15.92% | 13.49% | 14.62% | 9.05% | 15.44% | 20.60% | 16.57% |
| Lepton (Dropbox) | 20.33% | 19.44% | 22.44% | 18.71% | 18.17% | 3.66% | 0.00% | 24.21% | 15.25% |
| Lepton (Rust port) | 20.75% | 19.96% | 22.37% | 18.71% | 18.63% | 11.96% | 12.30% | 25.12% | 18.63% |
| brunsli | 20.00% | 19.53% | 20.27% | 16.64% | 17.08% | 11.35% | 18.16% | 23.84% | 19.42% |
| packJPG | 20.44% | 19.78% | 23.59% | 19.27% | 18.96% | 12.44% | 15.95% | 24.85% | 19.31% |
| paq8px | 26.31% | 24.95% | 28.45% | 24.20% | 24.91% | 1.00% | 4.47% | 32.27% | 20.75% |

## Test set (test)

80 files, 89,262,753 bytes; held out from tuning, but already used in earlier evaluations of Komla.

### One thread

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla fast** | 19.68% | 13.20 | 16.33 | 29 MiB | 6 MiB | 0 | 1.5% / 3.0% |
| JPEG XL | 15.85% | 9.60 | 16.80 | 457 MiB | 91 MiB | 0 | 3.2% / 0.3% |
| Lepton (Dropbox) | 14.57% | 3.67 | 8.16 | 83 MiB | 11 MiB | 19 | 0.5% / 2.0% |
| Lepton (Rust port) | 19.49% | 11.51 | 10.61 | 85 MiB | 115 MiB | 1 | 0.3% / 1.3% |
| brunsli | 18.67% | 10.20 | 12.24 | 262 MiB | 86 MiB | 0 | 0.8% / 0.9% |
| packJPG | 19.33% | 3.65 | 3.45 | 83 MiB | 82 MiB | 0 | 0.5% / 1.5% |
| paq8px \* | 19.97% | 0.01 | 0.01 | n/a | n/a | 0 | n/a / n/a |

### All threads (8 cores)

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |
|---|---|---|---|---|---|---|---|
| **Komla fast** | 19.68% | 28.34 | 37.18 | 61 MiB | 47 MiB | 0 | 3.1% / 3.1% |
| JPEG XL | 15.85% | 13.79 | 28.33 | 457 MiB | 96 MiB | 0 | 1.9% / 1.9% |
| Lepton (Dropbox) | 19.00% | 7.75 | 20.63 | 97 MiB | 90 MiB | 0 | 0.9% / 1.3% |
| Lepton (Rust port) | 18.93% | 24.31 | 29.02 | 93 MiB | 116 MiB | 1 | 2.1% / 1.1% |
| brunsli | 18.67% | 10.20 | 12.24 | 262 MiB | 86 MiB | 0 | 0.8% / 0.9% |
| packJPG | 19.33% | 3.65 | 3.45 | 83 MiB | 82 MiB | 0 | 0.5% / 1.5% |
| paq8px \* | 19.97% | 0.01 | 0.01 | n/a | n/a | 0 | n/a / n/a |

\* paq8px takes hours per set, so it ran separately (several files at a time, alongside other work, not interleaved with the other tools); its times are indicative only. On this set its sizes and times come from an earlier run (`results/jpeg_test.jsonl`) and its memory was not measured.

Files restored byte for byte: Komla fast 1t: 80/80; Komla fast mt: 80/80; JPEG XL 1t: 80/80; JPEG XL mt: 80/80; Lepton (Dropbox) 1t: 61/80; Lepton (Dropbox) mt: 80/80; Lepton (Rust port) 1t: 79/80; Lepton (Rust port) mt: 79/80; brunsli 1t: 80/80; packJPG 1t: 80/80.

Failures (counted at the original size): Lepton (Dropbox) (1t): 19 — 19× error (exit code 41); Lepton (Rust port) (1t): 1 — 1× error (exit code 42); Lepton (Rust port) (mt): 1 — 1× error (exit code 42).
No tool crashed or restored a file wrongly: every failure was the tool refusing the file (unsupported progressive scan scripts or end-of-band runs, or Lepton's own round-trip check failing).

Komla coded every file in its fast mode (no file fell back to being stored).

### Komla fast against each tool

Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within 5% count as ties (margin: Komla relative to the tool).

| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |
|---|---|---|---|---|---|---|
| JPEG XL | 1t | win (+3.83 points) | win (+38% speed) | tie (-3% speed) | win (-94% memory) | win (-93% memory) |
| JPEG XL | mt | win (+3.83 points) | win (+105% speed) | win (+31% speed) | win (-87% memory) | win (-51% memory) |
| Lepton (Dropbox) | 1t | win (+5.12 points) | win (4x speed) | win (+100% speed) | win (-65% memory) | win (-45% memory) |
| Lepton (Dropbox) | mt | win (+0.68 points) | win (4x speed) | win (+80% speed) | win (-37% memory) | win (-48% memory) |
| Lepton (Rust port) | 1t | win (+0.20 points) | win (+15% speed) | win (+54% speed) | win (-66% memory) | win (-95% memory) |
| Lepton (Rust port) | mt | win (+0.75 points) | win (+17% speed) | win (+28% speed) | win (-35% memory) | win (-60% memory) |
| brunsli | 1t | win (+1.02 points) | win (+29% speed) | win (+33% speed) | win (-89% memory) | win (-93% memory) |
| brunsli | mt | win (+1.02 points) | win (+178% speed) | win (3x speed) | win (-77% memory) | win (-46% memory) |
| packJPG | 1t | win (+0.35 points) | win (4x speed) | win (5x speed) | win (-65% memory) | win (-93% memory) |
| packJPG | mt | win (+0.35 points) | win (8x speed) | win (11x speed) | win (-27% memory) | win (-43% memory) |
| paq8px | 1t | loss (-0.29 points) | win (1249x speed) | win (1540x speed) | n/a | n/a |
| paq8px | mt | loss (-0.29 points) | win (2683x speed) | win (3506x speed) | n/a | n/a |

Against the other tools (Komla's own maximum mode not counted): one thread: 26 wins, 1 tie, 1 loss; all threads: 27 wins, 0 ties, 1 loss.

### Size by group

| Tool | Camera photos<br>12 files | Grayscale<br>6 files | Kodak q50<br>12 files | Kodak q75<br>12 files | Kodak q90 4:4:4<br>12 files | Kodak q95 progressive<br>12 files | Progressive<br>8 files | Screenshots<br>6 files | **Total** |
|---|---|---|---|---|---|---|---|---|---|
| **Komla fast** | 21.59% | 22.37% | 23.22% | 19.51% | 19.88% | 13.18% | 15.29% | 21.65% | 19.68% |
| JPEG XL | 17.67% | 16.01% | 16.24% | 13.24% | 14.31% | 8.77% | 12.24% | 20.53% | 15.85% |
| Lepton (Dropbox) | 20.78% | 21.34% | 22.14% | 17.88% | 17.62% | 1.58% | 0.00% | 19.60% | 14.57% |
| Lepton (Rust port) | 21.30% | 21.96% | 22.07% | 17.88% | 18.15% | 11.49% | 15.74% | 20.26% | 19.49% |
| brunsli | 20.54% | 20.70% | 20.64% | 16.35% | 16.89% | 11.06% | 14.65% | 22.34% | 18.67% |
| packJPG | 21.05% | 21.07% | 23.32% | 18.48% | 18.53% | 12.02% | 15.57% | 22.46% | 19.33% |
| paq8px | 27.43% | 27.94% | 27.93% | 23.21% | 24.41% | 0.94% | 2.72% | 26.62% | 19.97% |

## Tools

| Tool | Version and command |
|---|---|
| Komla fast | this repository: `komla compress --fast`, `komla decompress`; `KOMLA_THREADS=1` for one thread, else all cores |
| Komla (maximum) | this repository: `komla compress`, `komla decompress` (same thread settings) |
| JPEG XL | libjxl 0.13.0: `cjxl --lossless_jpeg=1` at its default effort (7), `djxl`; `--num_threads=0` for one thread, else its default |
| Lepton (Dropbox) | dropbox/lepton f34c7f4: `lepton -unjailed -allowprogressive -memory=4096M -threadmemory=2048M`; `-singlethread` for one thread |
| Lepton (Rust port) | microsoft/lepton_jpeg_rust 0.5.9: `lepton_jpeg_util --noverify`; `--threads 1` for one thread |
| brunsli | google/brunsli c18b962: `cbrunsli`, `dbrunsli` (no threads) |
| packJPG | packJPG 2.5n: `packJPG -np -o` (no threads) |
| paq8px | hxim/paq8px v217: `paq8px -8`, `paq8px -d` (no threads) |
