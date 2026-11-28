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
