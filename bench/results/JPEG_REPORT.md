# JPEG benchmark results

Lossless JPEG recompression: every compressed file was decompressed again and compared with the original JPEG file byte for byte (SHA-256). Numbers are how much smaller the output is than the original JPEG files; a codec that rejects a file or does not restore it exactly is counted at the original size and listed under failed files.

## Test set (held out: run once with the final codec)

| Codec | Kodak q50<br>12 files, 0.5 MB | Kodak q75<br>12 files, 0.8 MB | Kodak q90 4:4:4<br>12 files, 1.6 MB | Kodak q95 progressive<br>12 files, 1.9 MB | Camera photos<br>12 files, 52.2 MB | Progressive<br>8 files, 24.7 MB | Grayscale<br>6 files, 5.2 MB | Screenshots<br>6 files, 2.3 MB | **Total** | Output bytes | Failed files |
|---|---|---|---|---|---|---|---|---|---|---|---|
| JPEG XL | 16.60% | 13.46% | 14.57% | 8.86% | 17.82% | 12.35% | 16.16% | 21.02% | 16.00% | 74,978,288 | 0 |
| brunsli | 20.64% | 16.35% | 16.89% | 11.06% | 20.54% | 14.65% | 20.70% | 22.34% | 18.67% | 72,599,907 | 0 |
| Lepton (Dropbox) | 22.14% | 17.88% | 17.62% | 11.24% | 20.78% | 15.14% | 21.34% | 21.57% | 19.00% | 72,300,744 | 0 |
| Lepton (Rust port) | 22.07% | 17.88% | 18.15% | 11.49% | 21.30% | 15.74% | 21.96% | 20.26% | 19.49% | 71,868,099 | 1 |
| packJPG | 23.32% | 18.48% | 18.53% | 12.02% | 21.05% | 15.57% | 21.07% | 22.46% | 19.33% | 72,007,169 | 0 |
| paq8px | **27.93%** | **23.21%** | **24.41%** | 0.94% | **27.43%** | 2.72% | **27.94%** | 26.62% | 19.97% | 71,435,214 | 0 |
| **Komla** | 26.97% | 22.31% | 22.98% | **15.45%** | 25.77% | **20.52%** | 26.33% | **27.73%** | **24.11%** | 67,740,252 | 0 |

Original JPEG files: 89,262,753 bytes. Higher is better; the best result in each column is bold; — means the tool was not run on every file of that group.

Speed (one file per core, several files in parallel on an 8-core machine; MB of JPEG input per second, single-threaded codecs):

| Codec | Compress MB/s | Decompress MB/s |
|---|---|---|
| JPEG XL | 0.40 | 9.62 |
| brunsli | 10.83 | 12.76 |
| Lepton (Dropbox) | 3.28 | 18.10 |
| Lepton (Rust port) | 10.35 | 10.45 |
| packJPG | 3.77 | 3.52 |
| paq8px | 0.01 | 0.01 |
| Komla | 0.37 | 0.37 |

## Development set (used for tuning)

| Codec | Kodak q50<br>12 files, 0.5 MB | Kodak q75<br>12 files, 0.8 MB | Kodak q90 4:4:4<br>12 files, 1.6 MB | Kodak q95 progressive<br>12 files, 1.8 MB | Camera photos<br>12 files, 58.8 MB | Progressive<br>8 files, 22.9 MB | Grayscale<br>6 files, 7.7 MB | Screenshots<br>6 files, 3.2 MB | **Total** | Output bytes | Failed files |
|---|---|---|---|---|---|---|---|---|---|---|---|
| JPEG XL | 16.24% | 13.66% | 14.83% | 9.09% | 17.22% | 15.71% | 17.54% | 21.32% | 16.80% | 81,021,013 | 0 |
| brunsli | 20.27% | 16.64% | 17.08% | 11.35% | 20.00% | 18.16% | 19.53% | 23.84% | 19.42% | 78,466,910 | 0 |
| Lepton (Dropbox) | 22.44% | 18.71% | 18.17% | 11.75% | 20.33% | 15.87% | 19.44% | 24.21% | 19.14% | 78,743,416 | 1 |
| Lepton (Rust port) | 22.37% | 18.71% | 18.63% | 11.96% | 20.75% | 12.30% | 19.96% | 25.12% | 18.63% | 79,234,781 | 4 |
| packJPG | 23.59% | 19.27% | 18.96% | 12.44% | 20.44% | 15.95% | 19.78% | 24.85% | 19.31% | 78,579,003 | 1 |
| paq8px | **28.45%** | **24.20%** | **24.91%** | 1.00% | — | — | — | — | — | — | 0 |
| **Komla** | 27.33% | 23.19% | 23.48% | **15.98%** | **25.26%** | **25.10%** | **23.57%** | **30.01%** | **25.03%** | 73,003,227 | 0 |

Original JPEG files: 97,380,012 bytes. Higher is better; the best result in each column is bold; — means the tool was not run on every file of that group.

Speed (one file per core, several files in parallel on an 8-core machine; MB of JPEG input per second, single-threaded codecs):

| Codec | Compress MB/s | Decompress MB/s |
|---|---|---|
| JPEG XL | 0.64 | 12.59 |
| brunsli | 10.11 | 12.13 |
| Lepton (Dropbox) | 2.55 | 11.59 |
| Lepton (Rust port) | 11.73 | 10.64 |
| packJPG | 3.37 | 3.09 |
| paq8px | 0.01 | 0.01 |
| Komla | 0.33 | 0.32 |

## Exact tools and flags

| Codec | Version and command |
|---|---|
| JPEG XL | libjxl 0.13.0: cjxl --lossless_jpeg=1 -e 9 (decoded with djxl) |
| brunsli | google/brunsli c18b962: cbrunsli / dbrunsli |
| Lepton (Dropbox) | dropbox/lepton f34c7f4: lepton -singlethread -allowprogressive (default threading where single-threaded mode fails) |
| Lepton (Rust port) | microsoft/lepton_jpeg_rust 0.5.9: lepton_jpeg_util --threads 1 |
| packJPG | packJPG 2.5n |
| paq8px | paq8px v217: paq8px -8 |
| Komla | this repository |
