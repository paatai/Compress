# Lossless JPEG recompression: size, speed and memory (test set)

80 JPEG files, 89,262,753 bytes. Each tool ran on each file one process at a time, all tools on one file before the next, so they saw the same machine conditions; times are wall-clock seconds of the whole process (fastest of three runs; one run for Komla's maximum mode), peak memory is the largest resident set size of any file, from /usr/bin/time. Every output was decompressed again and compared with the original file byte for byte (SHA-256); a file a tool rejects counts at its original size. MB/s is megabytes of original JPEG per second (total over all files). Machine: 8-core x86-64 (AVX2) virtual machine at 2.4 GHz.

## One thread

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Failed files |
|---|---|---|---|---|---|---|
| **Komla fast** | 18.78% | 12.36 | 17.41 | 92 MiB | 21 MiB | 0 |
| Komla (maximum) | 24.11% | 0.34 | 0.34 | 189 MiB | 207 MiB | 0 |
| JPEG XL | 15.85% | 9.22 | 16.67 | 457 MiB | 91 MiB | 0 |
| Lepton (Dropbox) | 14.57% | 3.61 | 8.04 | 83 MiB | 11 MiB | 19 |
| Lepton (Rust port) | 19.49% | 11.16 | 10.41 | 85 MiB | 115 MiB | 1 |
| brunsli | 18.67% | 9.97 | 12.04 | 262 MiB | 86 MiB | 0 |
| packJPG | 19.33% | 3.58 | 3.36 | 83 MiB | 82 MiB | 0 |

## Default threads (8 cores)

| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Failed files |
|---|---|---|---|---|---|---|
| **Komla fast** | 18.78% | 22.19 | 34.43 | 207 MiB | 48 MiB | 0 |
| Komla (maximum) | 24.11% | 0.59 | 0.58 | 499 MiB | 389 MiB | 0 |
| JPEG XL | 15.85% | 13.77 | 28.06 | 457 MiB | 96 MiB | 0 |
| Lepton (Dropbox) | 19.00% | 8.08 | 22.29 | 97 MiB | 89 MiB | 0 |
| Lepton (Rust port) | 18.93% | 25.17 | 30.56 | 94 MiB | 116 MiB | 1 |
| brunsli | 18.67% | 9.97 | 12.04 | 262 MiB | 86 MiB | 0 |
| packJPG | 19.33% | 3.58 | 3.36 | 83 MiB | 82 MiB | 0 |

## Komla fast compared with JPEG XL

| | One thread | Default threads |
|---|---|---|
| Size (smaller is better) | 72,495,504 vs 75,113,776 bytes (3.5% smaller) | same |
| Compression speed | 1.34x | 1.61x |
| Decompression speed | 1.04x | 1.23x |
| Peak memory, compression | 92 vs 457 MiB | 207 vs 457 MiB |
| Peak memory, decompression | 21 vs 91 MiB | 48 vs 96 MiB |

## Size by group

| Tool | Kodak q50<br>12 files | Kodak q75<br>12 files | Kodak q90 4:4:4<br>12 files | Kodak q95 progressive<br>12 files | Camera photos<br>12 files | Progressive<br>8 files | Grayscale<br>6 files | Screenshots<br>6 files | **Total** |
|---|---|---|---|---|---|---|---|---|---|
| **Komla fast** | 22.79% | 18.84% | 19.12% | 12.13% | 20.60% | 14.54% | 21.41% | 21.12% | 18.78% |
| Komla (maximum) | 26.97% | 22.31% | 22.98% | 15.45% | 25.77% | 20.52% | 26.33% | 27.73% | 24.11% |
| JPEG XL | 16.24% | 13.24% | 14.31% | 8.77% | 17.67% | 12.24% | 16.01% | 20.53% | 15.85% |
| Lepton (Dropbox) | 22.14% | 17.88% | 17.62% | 1.58% | 20.78% | 0.00% | 21.34% | 19.60% | 14.57% |
| Lepton (Rust port) | 22.07% | 17.88% | 18.15% | 11.49% | 21.30% | 15.74% | 21.96% | 20.26% | 19.49% |
| brunsli | 20.64% | 16.35% | 16.89% | 11.06% | 20.54% | 14.65% | 20.70% | 22.34% | 18.67% |
| packJPG | 23.32% | 18.48% | 18.53% | 12.02% | 21.05% | 15.57% | 21.07% | 22.46% | 19.33% |

Notes: Dropbox Lepton's single-threaded mode (`-singlethread`) fails its own round-trip check on many progressive files, which therefore count at their original size in its one-thread row; its default (multi-threaded) mode restores them. The Rust port of Lepton rejects one file (a progressive JPEG with two DC first scans). Komla's maximum mode is the default `komla compress`; its stripes are large, so it gains little from threads.

## Tools

| Tool | Version and command |
|---|---|
| Komla fast | this repository: komla compress --fast, komla decompress (KOMLA_THREADS=1 for one thread) |
| Komla (maximum) | this repository: komla compress, komla decompress |
| JPEG XL | libjxl 0.13.0: cjxl --lossless_jpeg=1 (default effort 7), djxl; --num_threads=0 for one thread |
| Lepton (Dropbox) | dropbox/lepton f34c7f4: lepton -allowprogressive; -singlethread for one thread |
| Lepton (Rust port) | microsoft/lepton_jpeg_rust 0.5.9: lepton_jpeg_util --noverify; --threads 1 for one thread |
| brunsli | google/brunsli c18b962: cbrunsli, dbrunsli (single-threaded) |
| packJPG | packJPG 2.5n (single-threaded) |
