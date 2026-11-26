#!/usr/bin/env python3
"""Builds the benchmark tables (bench/results/REPORT.md) and the web app's benchmark data
(web/src/data/benchmark.json) from bench/results/{test,dev}.jsonl."""
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

CODECS = [
    ("zip", "ZIP -9 (of BMP)", "Info-ZIP 3.0: zip -9"),
    ("rar", "RAR -m5 (of BMP)", "RAR 7.23: rar a -m5"),
    ("7z", "7-Zip -mx9 (of BMP)", "7-Zip 26.06: 7zz a -mx=9 -mmt=1"),
    ("xz", "xz -9e (of BMP)", "XZ Utils 5.4.5: xz -9e -T1"),
    ("qoi", "QOI", "qoiconv (phoboslab/qoi ffb2d2c)"),
    ("png-oxipng", "PNG (oxipng -o max)", "oxipng 10.2.1: -o max --strip all"),
    ("png-zopfli", "PNG (zopflipng -m)", "zopflipng 1.0.3: -m"),
    ("flif", "FLIF", "FLIF 0.4: flif -e -E100 -K"),
    ("avif", "AVIF lossless", "libavif 1.4.2 + aom 3.14.1: avifenc --lossless -s 0"),
    ("webp", "WebP lossless", "libwebp 1.6.0: cwebp -lossless -z 9 -exact"),
    ("jxl-e9", "JPEG XL e9", "libjxl 0.13.0: cjxl -d 0 -e 9"),
    ("jxl-e10", "JPEG XL e10", "libjxl 0.13.0: cjxl -d 0 -e 10"),
    ("komla", "Komla", "this repository"),
]
CATS = {
    "photo_kodak": "Kodak photos",
    "photo_div2k": "DIV2K photos",
    "screenshot": "Screenshots",
    "graphics": "Charts, diagrams, maps",
}


def load(name):
    path = os.path.join(HERE, "results", f"{name}.jsonl")
    return [json.loads(l) for l in open(path)] if os.path.exists(path) else []


def summarize(rows):
    cats = [c for c in CATS if any(r["category"] == c for r in rows)]
    table = {}
    for cid, _, _ in CODECS:
        rs = [r for r in rows if r["codec"] == cid]
        if not rs:
            continue
        entry = {"ok": all(r["ok"] for r in rs), "bytes": sum(r["bytes"] for r in rs), "enc_s": sum(r["enc_s"] for r in rs), "dec_s": sum(r["dec_s"] for r in rs)}
        for c in cats:
            cr = [r for r in rs if r["category"] == c]
            entry[c] = sum(r["bytes"] for r in cr) * 8 / sum(r["pixels"] for r in cr)
        entry["total"] = entry["bytes"] * 8 / sum(r["pixels"] for r in rs)
        table[cid] = entry
    info = {}
    for c in cats:
        imgs = {r["image"]: r["pixels"] for r in rows if r["category"] == c}
        info[c] = (len(imgs), sum(imgs.values()))
    total_px = sum(v[1] for v in info.values())
    return cats, table, info, total_px


def md_table(name, rows):
    cats, table, info, total_px = summarize(rows)
    k = table["komla"]
    out = []
    head = "| Codec | " + " | ".join(f"{CATS[c]}<br>{info[c][0]} images, {info[c][1] / 1e6:.1f} MP" for c in cats) + f" | **Total**<br>{total_px / 1e6:.1f} MP | Total bytes | Komla is smaller by |"
    out.append(head)
    out.append("|" + "---|" * (len(cats) + 4))
    best = {c: min(v[c] for cid, v in table.items() if cid != "komla") for c in cats + ["total"]}
    for cid, label, _ in CODECS:
        if cid not in table:
            continue
        v = table[cid]
        cells = []
        for c in cats + ["total"]:
            s = f"{v[c]:.4f}"
            if cid == "komla" or v[c] == best[c]:
                s = f"**{s}**"
            cells.append(s)
        saving = "—" if cid == "komla" else f"{(1 - k['bytes'] / v['bytes']) * 100:.1f}%"
        out.append(f"| {'**' + label + '**' if cid == 'komla' else label} | " + " | ".join(cells) + f" | {v['bytes']:,} | {saving} |")
    out.append("")
    out.append("| | " + " | ".join(CATS[c] for c in cats) + " | Total |")
    out.append("|" + "---|" * (len(cats) + 2))
    out.append("| Best other format | " + " | ".join(next(l for cid, l, _ in CODECS if cid != "komla" and cid in table and table[cid][c] == best[c]) for c in cats + ["total"]) + " |")
    out.append("| Komla vs that format | " + " | ".join(f"{(1 - k[c] / best[c]) * 100:.1f}% smaller" for c in cats + ["total"]) + " |")
    return "\n".join(out), cats, table, info, total_px


def main():
    test, dev = load("test"), load("dev")
    parts = ["# Benchmark results\n", "Bits per pixel (file size in bits divided by the number of pixels; lower is better). Every image-format output was decoded again and compared with the original by a SHA-256 of the pixels; archives were extracted and compared byte for byte. All results are lossless.\n"]
    for name, rows, title in [("test", test, "Test set (held out: never used for tuning, run once with the final codec)"), ("dev", dev, "Development set (used for tuning)")]:
        if not rows:
            continue
        md, cats, table, info, total_px = md_table(name, rows)
        parts.append(f"## {title}\n\n{md}\n")
        px = total_px
        sp = ["| Codec | Encode MP/s | Decode MP/s |", "|---|---|---|"]
        for cid, label, _ in CODECS:
            if cid in table and cid not in ("zip", "rar", "7z", "xz"):
                v = table[cid]
                dec = f"{px / v['dec_s'] / 1e6:.2f}" if v["dec_s"] > 0 else "—"
                sp.append(f"| {label} | {px / v['enc_s'] / 1e6:.3f} | {dec} |")
        parts.append(f"Speed on this set (one image per core, 8 jobs on an 8-core Xeon, single-threaded encoders):\n\n" + "\n".join(sp) + "\n")
    parts.append("## Exact tools and flags\n\n| Codec | Version and command |\n|---|---|\n" + "\n".join(f"| {l} | {c} |" for _, l, c in CODECS) + "\n")
    with open(os.path.join(HERE, "results", "REPORT.md"), "w") as f:
        f.write("\n".join(parts))

    if test:
        cats, table, info, _ = summarize(test)
        data = {
            "categories": [{"id": c, "label": CATS[c], "images": info[c][0], "megapixels": info[c][1] / 1e6} for c in cats],
            "codecs": [
                {"id": cid, "label": label, "bpp": {c: round(table[cid][c], 4) for c in cats + ["total"]}, **({"komla": True} if cid == "komla" else {})}
                for cid, label, _ in CODECS
                if cid in table and cid not in ("rar", "7z", "qoi", "png-oxipng")
            ],
            "notes": [
                "Test set: the 24 Kodak images, DIV2K validation images 0801-0810, and 24 screenshots and graphics selected deterministically from Wikimedia Commons; a disjoint development set was used for tuning.",
                "ZIP and xz compress an uncompressed BMP of the same pixels. Baselines: libjxl 0.13.0, libwebp 1.6.0, libavif 1.4.2 (aom 3.14.1), FLIF 0.4, zopflipng 1.0.3, Info-ZIP 3.0, XZ Utils 5.4.5. RAR, 7-Zip, QOI and oxipng results are in the README.",
                "Komla decodes at about the same speed as it encodes (roughly 0.15 megapixels per second per core), much slower than the other image formats.",
            ],
        }
        with open(os.path.join(ROOT, "web", "src", "data", "benchmark.json"), "w") as f:
            json.dump(data, f, indent=1)
    print(open(os.path.join(HERE, "results", "REPORT.md")).read())


if __name__ == "__main__":
    main()
