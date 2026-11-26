# Benchmark results

Bits per pixel (file size in bits divided by the number of pixels; lower is better). Every image-format output was decoded again and compared with the original by a SHA-256 of the pixels; archives were extracted and compared byte for byte. All results are lossless.

## Test set (held out: never used for tuning, run once with the final codec)

| Codec | Kodak photos<br>24 images, 9.4 MP | DIV2K photos<br>10 images, 28.0 MP | Screenshots<br>12 images, 9.3 MP | Charts, diagrams, maps<br>12 images, 10.7 MP | **Total**<br>57.4 MP | Total bytes | Komla is smaller by |
|---|---|---|---|---|---|---|---|
| ZIP -9 (of BMP) | 15.0152 | 19.8078 | 2.4528 | 0.5515 | 12.6254 | 90,572,201 | 56.5% |
| RAR -m5 (of BMP) | 15.2376 | 14.9942 | 2.0440 | 0.4764 | 10.2351 | 73,424,516 | 46.3% |
| 7-Zip -mx9 (of BMP) | 12.2634 | 15.7940 | 1.5534 | 0.3453 | 10.0323 | 71,969,612 | 45.2% |
| xz -9e (of BMP) | 12.2756 | 15.7890 | 1.5346 | 0.3205 | 10.0242 | 71,911,408 | 45.2% |
| QOI | 13.9885 | 15.1343 | 3.2234 | 0.8314 | 10.3547 | 74,282,374 | 46.9% |
| PNG (oxipng -o max) | 12.4069 | 12.4286 | 1.5557 | 0.4758 | 8.4398 | 60,545,065 | 34.9% |
| PNG (zopflipng -m) | 12.4746 | 12.3837 | 1.5461 | 0.4609 | 8.4247 | 60,436,790 | 34.8% |
| FLIF | 8.6924 | 9.3078 | 1.3684 | 0.3036 | 6.2451 | 44,800,782 | 12.0% |
| AVIF lossless | 11.2950 | 10.9702 | 2.4296 | 0.7048 | 7.7298 | 55,451,788 | 28.9% |
| WebP lossless | 9.4978 | 9.7087 | **1.0925** | 0.2547 | 6.5193 | 46,767,932 | 15.7% |
| JPEG XL e9 | 8.4851 | 8.9105 | 1.3363 | 0.3353 | 6.0180 | 43,171,882 | 8.7% |
| JPEG XL e10 | **8.4393** | **8.7340** | 1.1125 | **0.2501** | **5.8723** | 42,126,828 | 6.4% |
| **Komla** | **7.9843** | **8.2389** | **0.8325** | **0.1669** | **5.4954** | 39,422,797 | — |

| | Kodak photos | DIV2K photos | Screenshots | Charts, diagrams, maps | Total |
|---|---|---|---|---|---|
| Best other format | JPEG XL e10 | JPEG XL e10 | WebP lossless | JPEG XL e10 | JPEG XL e10 |
| Komla vs that format | 5.4% smaller | 5.7% smaller | 23.8% smaller | 33.3% smaller | 6.4% smaller |

Speed on this set (one image per core, 8 jobs on an 8-core Xeon, single-threaded encoders):

| Codec | Encode MP/s | Decode MP/s |
|---|---|---|
| QOI | 43.445 | 8.34 |
| PNG (oxipng -o max) | 0.292 | — |
| PNG (zopflipng -m) | 0.065 | — |
| FLIF | 0.291 | 2.39 |
| AVIF lossless | 0.040 | 3.36 |
| WebP lossless | 0.145 | 4.65 |
| JPEG XL e9 | 0.299 | 9.75 |
| JPEG XL e10 | 0.066 | 8.15 |
| Komla | 0.142 | 0.14 |

## Development set (used for tuning)

| Codec | DIV2K photos<br>10 images, 28.8 MP | Screenshots<br>12 images, 6.9 MP | Charts, diagrams, maps<br>12 images, 8.2 MP | **Total**<br>43.9 MP | Total bytes | Komla is smaller by |
|---|---|---|---|---|---|---|
| ZIP -9 (of BMP) | 16.8011 | 0.8293 | 0.5708 | 11.2557 | 61,707,139 | 57.7% |
| RAR -m5 (of BMP) | 13.3660 | 0.5925 | 0.4324 | 8.9398 | 49,010,445 | 46.8% |
| 7-Zip -mx9 (of BMP) | 13.5306 | 0.5241 | 0.3629 | 9.0240 | 49,472,369 | 47.3% |
| xz -9e (of BMP) | 13.5230 | 0.4857 | 0.3350 | 9.0078 | 49,383,352 | 47.2% |
| QOI | 13.3414 | 2.0164 | 0.9754 | 9.2484 | 50,702,449 | 48.6% |
| PNG (oxipng -o max) | 11.2044 | 0.6814 | 0.4718 | 7.5435 | 41,355,590 | 37.0% |
| PNG (zopflipng -m) | 11.1727 | 0.6615 | 0.4602 | 7.5174 | 41,212,660 | 36.7% |
| FLIF | 7.8696 | 0.6791 | 0.3848 | 5.3397 | 29,273,739 | 10.9% |
| AVIF lossless | 9.8850 | 1.3714 | 0.8784 | 6.8624 | 37,621,924 | 30.7% |
| WebP lossless | 8.5542 | **0.4330** | **0.2933** | 5.7330 | 31,429,878 | 17.0% |
| JPEG XL e9 | 7.6717 | 0.6870 | 0.4296 | 5.2195 | 28,614,840 | 8.9% |
| JPEG XL e10 | **7.4936** | 0.5033 | 0.3454 | **5.0582** | 27,730,571 | 6.0% |
| **Komla** | **7.1174** | **0.3002** | **0.2179** | **4.7557** | 26,072,429 | — |

| | DIV2K photos | Screenshots | Charts, diagrams, maps | Total |
|---|---|---|---|---|
| Best other format | JPEG XL e10 | WebP lossless | WebP lossless | JPEG XL e10 |
| Komla vs that format | 5.0% smaller | 30.7% smaller | 25.7% smaller | 6.0% smaller |

Speed on this set (one image per core, 8 jobs on an 8-core Xeon, single-threaded encoders):

| Codec | Encode MP/s | Decode MP/s |
|---|---|---|
| QOI | 26.405 | 5.04 |
| PNG (oxipng -o max) | 0.154 | — |
| PNG (zopflipng -m) | 0.059 | — |
| FLIF | 0.288 | 2.33 |
| AVIF lossless | 0.038 | 2.89 |
| WebP lossless | 0.187 | 3.43 |
| JPEG XL e9 | 0.220 | 6.17 |
| JPEG XL e10 | 0.068 | 8.24 |
| Komla | 0.153 | 0.16 |

## Exact tools and flags

| Codec | Version and command |
|---|---|
| ZIP -9 (of BMP) | Info-ZIP 3.0: zip -9 |
| RAR -m5 (of BMP) | RAR 7.23: rar a -m5 |
| 7-Zip -mx9 (of BMP) | 7-Zip 26.06: 7zz a -mx=9 -mmt=1 |
| xz -9e (of BMP) | XZ Utils 5.4.5: xz -9e -T1 |
| QOI | qoiconv (phoboslab/qoi ffb2d2c) |
| PNG (oxipng -o max) | oxipng 10.2.1: -o max --strip all |
| PNG (zopflipng -m) | zopflipng 1.0.3: -m |
| FLIF | FLIF 0.4: flif -e -E100 -K |
| AVIF lossless | libavif 1.4.2 + aom 3.14.1: avifenc --lossless -s 0 |
| WebP lossless | libwebp 1.6.0: cwebp -lossless -z 9 -exact |
| JPEG XL e9 | libjxl 0.13.0: cjxl -d 0 -e 9 |
| JPEG XL e10 | libjxl 0.13.0: cjxl -d 0 -e 10 |
| Komla | this repository |
