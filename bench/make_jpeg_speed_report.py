#!/usr/bin/env python3
"""Turns bench/results/jpeg_speed_<set>.jsonl into bench/results/JPEG_SPEED_REPORT.md and the
website's web/src/data/jpeg-speed.json.

usage: make_jpeg_speed_report.py [--set test]
"""
import argparse
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CODECS = [
    ("komla-fast", "Komla fast"),
    ("komla", "Komla (maximum)"),
    ("jxl", "JPEG XL"),
    ("lepton", "Lepton (Dropbox)"),
    ("lepton-rust", "Lepton (Rust port)"),
    ("brunsli", "brunsli"),
    ("packjpg", "packJPG"),
]
GROUPS = [
    ("kodak_q50", "Kodak q50"),
    ("kodak_q75", "Kodak q75"),
    ("kodak_q90_444", "Kodak q90 4:4:4"),
    ("kodak_q95_prog", "Kodak q95 progressive"),
    ("camera", "Camera photos"),
    ("progressive", "Progressive"),
    ("gray", "Grayscale"),
    ("screenshot", "Screenshots"),
]
TOOLS = [
    ("Komla fast", "this repository: komla compress --fast, komla decompress (KOMLA_THREADS=1 for one thread)"),
    ("Komla (maximum)", "this repository: komla compress, komla decompress"),
    ("JPEG XL", "libjxl 0.13.0: cjxl --lossless_jpeg=1 (default effort 7), djxl; --num_threads=0 for one thread"),
    ("Lepton (Dropbox)", "dropbox/lepton f34c7f4: lepton -allowprogressive; -singlethread for one thread"),
    ("Lepton (Rust port)", "microsoft/lepton_jpeg_rust 0.5.9: lepton_jpeg_util --noverify; --threads 1 for one thread"),
    ("brunsli", "google/brunsli c18b962: cbrunsli, dbrunsli (single-threaded)"),
    ("packJPG", "packJPG 2.5n (single-threaded)"),
]


def summary(rs):
    ok = [r for r in rs if r["ok"]]
    inp = sum(r["input"] for r in rs)
    out = sum(r["bytes"] if r["ok"] else r["input"] for r in rs)
    ib = sum(r["input"] for r in ok)
    es = sum(r["enc_s"] for r in ok)
    ds = sum(r["dec_s"] for r in ok)
    return {
        "saved": 100 * (1 - out / inp) if inp else None,
        "bytes": out,
        "enc": ib / es / 1e6 if es else None,
        "dec": ib / ds / 1e6 if ds else None,
        "enc_mib": max((r["enc_rss_kb"] for r in ok), default=0) / 1024,
        "dec_mib": max((r["dec_rss_kb"] for r in ok), default=0) / 1024,
        "fails": len(rs) - len(ok),
        "files": len(rs),
    }


def fmt(x, nd=2):
    return "—" if x is None else f"{x:.{nd}f}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", default="test")
    args = ap.parse_args()
    path = os.path.join(HERE, "results", f"jpeg_speed_{args.set}.jsonl")
    rows = [json.loads(line) for line in open(path)]
    images = sorted({r["image"] for r in rows})
    total_in = sum({r["image"]: r["input"] for r in rows}.values())
    table = {}
    for cid, _ in CODECS:
        for th in ("1t", "mt"):
            rs = [r for r in rows if r["codec"] == cid and r["threads"] == th]
            if len(rs) == len(images):
                table[(cid, th)] = summary(rs)
    out = [
        f"# Lossless JPEG recompression: size, speed and memory ({args.set} set)",
        "",
        f"{len(images)} JPEG files, {total_in:,} bytes. Each tool ran on each file one process at a time, all tools on one file before the next, so they saw the same machine conditions; "
        "times are wall-clock seconds of the whole process (fastest of three runs; one run for Komla's maximum mode), peak memory is the largest resident set size of any file, from /usr/bin/time. "
        "Every output was decompressed again and compared with the original file byte for byte (SHA-256); a file a tool rejects counts at its original size. "
        "MB/s is megabytes of original JPEG per second (total over all files). Machine: 8-core x86-64 (AVX2) virtual machine at 2.4 GHz.",
        "",
    ]
    for th, title in (("1t", "One thread"), ("mt", "Default threads (8 cores)")):
        out += [f"## {title}", "", "| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Failed files |", "|---|---|---|---|---|---|---|"]
        for cid, label in CODECS:
            v = table.get((cid, th))
            if not v:
                continue
            name = f"**{label}**" if cid == "komla-fast" else label
            out.append(f"| {name} | {fmt(v['saved'])}% | {fmt(v['enc'])} | {fmt(v['dec'])} | {v['enc_mib']:.0f} MiB | {v['dec_mib']:.0f} MiB | {v['fails']} |")
        out.append("")
    f1, j1, fm, jm = (table.get(k) for k in (("komla-fast", "1t"), ("jxl", "1t"), ("komla-fast", "mt"), ("jxl", "mt")))
    if f1 and j1 and fm and jm:
        out += ["## Komla fast compared with JPEG XL", "", "| | One thread | Default threads |", "|---|---|---|"]
        out.append(f"| Size (smaller is better) | {f1['bytes']:,} vs {j1['bytes']:,} bytes ({100 * (1 - f1['bytes'] / j1['bytes']):.1f}% smaller) | same |")
        out.append(f"| Compression speed | {f1['enc'] / j1['enc']:.2f}x | {fm['enc'] / jm['enc']:.2f}x |")
        out.append(f"| Decompression speed | {f1['dec'] / j1['dec']:.2f}x | {fm['dec'] / jm['dec']:.2f}x |")
        out.append(f"| Peak memory, compression | {f1['enc_mib']:.0f} vs {j1['enc_mib']:.0f} MiB | {fm['enc_mib']:.0f} vs {jm['enc_mib']:.0f} MiB |")
        out.append(f"| Peak memory, decompression | {f1['dec_mib']:.0f} vs {j1['dec_mib']:.0f} MiB | {fm['dec_mib']:.0f} vs {jm['dec_mib']:.0f} MiB |")
        out.append("")
    groups = [(g, label) for g, label in GROUPS if any(r["group"] == g for r in rows)]
    out += ["## Size by group", "", "| Tool | " + " | ".join(f"{label}<br>{sum(1 for i in images if f'/{g}/' in i)} files" for g, label in groups) + " | **Total** |", "|---" * (len(groups) + 2) + "|"]
    for cid, label in CODECS:
        rs = [r for r in rows if r["codec"] == cid and r["threads"] == "1t"]
        if len(rs) != len(images):
            continue
        cells = []
        for g, _ in groups:
            gr = [r for r in rs if r["group"] == g]
            i = sum(r["input"] for r in gr)
            o = sum(r["bytes"] if r["ok"] else r["input"] for r in gr)
            cells.append(f"{100 * (1 - o / i):.2f}%")
        name = f"**{label}**" if cid == "komla-fast" else label
        out.append(f"| {name} | " + " | ".join(cells) + f" | {fmt(table[(cid, '1t')]['saved'])}% |")
    out += [
        "",
        "Notes: Dropbox Lepton's single-threaded mode (`-singlethread`) fails its own round-trip check on many progressive files, which therefore count at their original size in its one-thread row; its default (multi-threaded) mode restores them. The Rust port of Lepton rejects one file (a progressive JPEG with two DC first scans). Komla's maximum mode is the default `komla compress`; its stripes are large, so it gains little from threads.",
        "",
        "## Tools",
        "",
        "| Tool | Version and command |",
        "|---|---|",
    ] + [f"| {a} | {b} |" for a, b in TOOLS] + [""]
    rpath = os.path.join(HERE, "results", "JPEG_SPEED_REPORT.md" if args.set == "test" else f"JPEG_SPEED_REPORT_{args.set}.md")
    with open(rpath, "w") as f:
        f.write("\n".join(out))
    print("\n".join(out))
    if args.set == "test":
        data = {
            "files": len(images),
            "bytes": total_in,
            "rows": [
                {"id": cid, "label": label, "threads": th, **{k: (round(v[k], 2) if isinstance(v[k], float) else v[k]) for k in ("saved", "enc", "dec", "enc_mib", "dec_mib", "fails")}}
                for cid, label in CODECS
                for th in ("1t", "mt")
                if (v := table.get((cid, th)))
            ],
        }
        with open(os.path.join(ROOT, "web", "src", "data", "jpeg-speed.json"), "w") as f:
            json.dump(data, f, indent=1)
            f.write("\n")


if __name__ == "__main__":
    main()
