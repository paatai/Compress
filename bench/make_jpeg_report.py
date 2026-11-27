#!/usr/bin/env python3
"""Builds the JPEG benchmark tables (bench/results/JPEG_REPORT.md) and the web app's JPEG
benchmark data (web/src/data/jpeg-benchmark.json) from bench/results/jpeg_{test,dev}.jsonl.

Sizes are "% smaller than the original JPEG files". A codec that rejects a file or does not
restore it exactly is counted at the original file size and listed as a failure."""
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

CODECS = [
    ("jxl", "JPEG XL", "libjxl 0.13.0: cjxl --lossless_jpeg=1 -e 9 (decoded with djxl)"),
    ("brunsli", "brunsli", "google/brunsli c18b962: cbrunsli / dbrunsli"),
    ("lepton", "Lepton (Dropbox)", "dropbox/lepton f34c7f4: lepton -singlethread -allowprogressive (default threading where single-threaded mode fails)"),
    ("lepton-rust", "Lepton (Rust port)", "microsoft/lepton_jpeg_rust 0.5.9: lepton_jpeg_util --threads 1"),
    ("packjpg", "packJPG", "packJPG 2.5n"),
    ("paq8px", "paq8px", "paq8px v217: paq8px -8"),
    ("komla", "Komla", "this repository"),
]
GROUPS = {
    "kodak_q50": "Kodak q50",
    "kodak_q75": "Kodak q75",
    "kodak_q90_444": "Kodak q90 4:4:4",
    "kodak_q95_prog": "Kodak q95 progressive",
    "camera": "Camera photos",
    "progressive": "Progressive",
    "gray": "Grayscale",
    "screenshot": "Screenshots",
}


def load(name):
    path = os.path.join(HERE, "results", f"jpeg_{name}.jsonl")
    return [json.loads(l) for l in open(path)] if os.path.exists(path) else []


def summarize(rows):
    groups = [g for g in GROUPS if any(r["group"] == g for r in rows)]
    table = {}
    for cid, _, _ in CODECS:
        rs = [r for r in rows if r["codec"] == cid]
        if not rs:
            continue
        e = {"fails": sum(1 for r in rs if not r["ok"])}
        for g in groups + ["total"]:
            gr = [r for r in rs if g == "total" or r["group"] == g]
            i = sum(r["input"] for r in gr)
            o = sum(r["bytes"] if r["ok"] else r["input"] for r in gr)
            e[g] = 100 * (1 - o / i) if i else float("nan")
            if g == "total":
                e["input"], e["bytes"] = i, o
        ok = [r for r in rs if r["ok"]]
        mb = sum(r["input"] for r in ok) / 1e6
        es, ds = sum(r["enc_s"] for r in ok), sum(r["dec_s"] for r in ok)
        e["enc"] = mb / es if es else 0
        e["dec"] = mb / ds if ds else 0
        table[cid] = e
    info = {g: (len({r["image"] for r in rows if r["group"] == g}), sum({r["image"]: r["input"] for r in rows if r["group"] == g}.values())) for g in groups}
    return groups, table, info


def md(rows):
    groups, table, info = summarize(rows)
    head = "| Codec | " + " | ".join(f"{GROUPS[g]}<br>{info[g][0]} files, {info[g][1] / 1e6:.1f} MB" for g in groups) + " | **Total** | Output bytes | Failed files |"
    out = [head, "|" + "---|" * (len(groups) + 4)]
    best = {g: max(v[g] for c, v in table.items() if c != "komla") for g in groups + ["total"]}
    for cid, label, _ in CODECS:
        if cid not in table:
            continue
        v = table[cid]
        cells = []
        for g in groups + ["total"]:
            s = f"{v[g]:.2f}%"
            cells.append(f"**{s}**" if cid == "komla" or v[g] == best[g] else s)
        name = f"**{label}**" if cid == "komla" else label
        out.append(f"| {name} | " + " | ".join(cells) + f" | {v['bytes']:,} | {v['fails']} |")
    total_in = next(iter(table.values()))["input"]
    out.append("")
    out.append(f"Original JPEG files: {total_in:,} bytes. Higher is better.")
    sp = ["| Codec | Compress MB/s | Decompress MB/s |", "|---|---|---|"]
    for cid, label, _ in CODECS:
        if cid in table:
            sp.append(f"| {label} | {table[cid]['enc']:.2f} | {table[cid]['dec']:.2f} |")
    return "\n".join(out), "\n".join(sp), groups, table, info


def main():
    test, dev = load("test"), load("dev")
    parts = [
        "# JPEG benchmark results\n",
        "Lossless JPEG recompression: every compressed file was decompressed again and compared "
        "with the original JPEG file byte for byte (SHA-256). Numbers are how much smaller the "
        "output is than the original JPEG files; a codec that rejects a file or does not restore "
        "it exactly is counted at the original size and listed under failed files.\n",
    ]
    for rows, title in [(test, "Test set (held out: run once with the final codec)"), (dev, "Development set (used for tuning)")]:
        if not rows:
            continue
        t, sp, *_ = md(rows)
        parts.append(f"## {title}\n\n{t}\n")
        parts.append("Speed (one file per core, several files in parallel on an 8-core machine; MB of JPEG input per second, single-threaded codecs):\n\n" + sp + "\n")
    parts.append("## Exact tools and flags\n\n| Codec | Version and command |\n|---|---|\n" + "\n".join(f"| {l} | {c} |" for _, l, c in CODECS) + "\n")
    with open(os.path.join(HERE, "results", "JPEG_REPORT.md"), "w") as f:
        f.write("\n".join(parts))
    if test:
        groups, table, info = summarize(test)
        data = {
            "groups": [{"id": g, "label": GROUPS[g], "files": info[g][0], "megabytes": round(info[g][1] / 1e6, 1)} for g in groups],
            "codecs": [
                {"id": cid, "label": label, "saved": {g: round(table[cid][g], 2) for g in groups + ["total"]}, "fails": table[cid]["fails"], **({"komla": True} if cid == "komla" else {})}
                for cid, label, _ in CODECS
                if cid in table
            ],
        }
        with open(os.path.join(ROOT, "web", "src", "data", "jpeg-benchmark.json"), "w") as f:
            json.dump(data, f, indent=1)
    print(open(os.path.join(HERE, "results", "JPEG_REPORT.md")).read())


if __name__ == "__main__":
    main()
