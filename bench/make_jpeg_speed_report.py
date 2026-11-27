#!/usr/bin/env python3
"""Turns the results of run_jpeg_speed.py (and run_paq8px.py) into
bench/results/JPEG_SPEED_REPORT.md and the website's web/src/data/jpeg-speed.json.

usage: make_jpeg_speed_report.py [--sets heldout,dev,test] [--site heldout]
"""
import argparse
import collections
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
    ("paq8px", "paq8px"),
]
LABEL = dict(CODECS)
SETS = {
    "heldout": ("Held-out random set", "drawn at random for this evaluation and never used before it (see below); Komla's code and parameters were frozen before its first run"),
    "dev": ("Development set", "used for all tuning of Komla (contexts, priors, parameters)"),
    "test": ("Test set", "held out from tuning, but already used in earlier evaluations of Komla"),
}
GROUP_LABEL = {
    "kodak_q50": "Kodak q50", "kodak_q75": "Kodak q75", "kodak_q90_444": "Kodak q90 4:4:4", "kodak_q95_prog": "Kodak q95 progressive",
    "camera": "Camera photos", "progressive": "Progressive", "gray": "Grayscale", "screenshot": "Screenshots",
    "photo": "Edited photos", "scan": "Scans, paintings, maps", "web": "Small web images",
}
TOOLS = [
    ("Komla fast", "this repository: `komla compress --fast`, `komla decompress`; `KOMLA_THREADS=1` for one thread, else all cores"),
    ("Komla (maximum)", "this repository: `komla compress`, `komla decompress` (same thread settings)"),
    ("JPEG XL", "libjxl 0.13.0: `cjxl --lossless_jpeg=1` at its default effort (7), `djxl`; `--num_threads=0` for one thread, else its default"),
    ("Lepton (Dropbox)", "dropbox/lepton f34c7f4: `lepton -unjailed -allowprogressive -memory=4096M -threadmemory=2048M`; `-singlethread` for one thread"),
    ("Lepton (Rust port)", "microsoft/lepton_jpeg_rust 0.5.9: `lepton_jpeg_util --noverify`; `--threads 1` for one thread"),
    ("brunsli", "google/brunsli c18b962: `cbrunsli`, `dbrunsli` (no threads)"),
    ("packJPG", "packJPG 2.5n: `packJPG -np -o` (no threads)"),
    ("paq8px", "hxim/paq8px v217: `paq8px -8`, `paq8px -d` (no threads)"),
]
TIE = 0.05


def paq_rows(name):
    """paq8px results of a set as speed rows (the same for one thread and all threads)."""
    path = os.path.join(HERE, "results", f"jpeg_paq8px_{name}.jsonl")
    src = []
    if os.path.exists(path):
        src = [json.loads(line) for line in open(path)]
    elif name == "test" and os.path.exists(os.path.join(HERE, "results", "jpeg_test.jsonl")):
        src = [r for r in map(json.loads, open(os.path.join(HERE, "results", "jpeg_test.jsonl"))) if r["codec"] == "paq8px"]
    out = []
    for r in src:
        base = {k: r.get(k) for k in ("image", "group", "input", "bytes", "enc_s", "dec_s", "ok")}
        base.update(codec="paq8px", enc_rss_kb=r.get("enc_rss_kb") or 0, dec_rss_kb=r.get("dec_rss_kb") or 0, separate=True)
        for th in ("1t", "mt"):
            out.append(dict(base, threads=th))
    return out


def load(name):
    path = os.path.join(HERE, "results", f"jpeg_speed_{name}.jsonl")
    rows = [json.loads(line) for line in open(path)] if os.path.exists(path) else []
    return rows + paq_rows(name)


def summary(rs):
    ok = [r for r in rs if r["ok"]]
    inp = sum(r["input"] for r in rs)
    out = sum(r["bytes"] if r["ok"] else r["input"] for r in rs)
    ib = sum(r["input"] for r in ok)
    es = sum(r["enc_s"] for r in ok)
    ds = sum(r["dec_s"] for r in ok)
    # run-to-run spread: total time of the k-th run over all files, slowest vs fastest k
    spread = {}
    for key in ("enc_all", "dec_all"):
        runs = [r[key] for r in ok if r.get(key)]
        n = min((len(x) for x in runs), default=0)
        if n >= 2 and len(runs) == len(ok):
            tot = [sum(x[k] for x in runs) for k in range(n)]
            spread[key] = max(tot) / min(tot) - 1
    rss_e = [r["enc_rss_kb"] for r in ok if r.get("enc_rss_kb")]
    rss_d = [r["dec_rss_kb"] for r in ok if r.get("dec_rss_kb")]
    return {
        "saved": 100 * (1 - out / inp) if inp else None,
        "bytes": out,
        "enc": ib / es / 1e6 if es else None,
        "dec": ib / ds / 1e6 if ds else None,
        "enc_mib": max(rss_e) / 1024 if rss_e else None,
        "dec_mib": max(rss_d) / 1024 if rss_d else None,
        "fails": len(rs) - len(ok),
        "errors": collections.Counter(kind(r.get("error") or "") for r in rs if not r["ok"]),
        "stored": sum(1 for r in rs if r.get("mode") in (0, 4)),
        "files": len(rs),
        "spread": spread,
        "separate": any(r.get("separate") for r in rs),
    }


def kind(err):
    """The class of a failure: the tool's error exit, a crash, or a wrong restored file."""
    if "differs" in err:
        return "restored file differs"
    if "crashed" in err:
        return err[err.index("crashed"):].split(":")[0]
    if "exit code" in err:
        return err[err.index("failed"):].split(":")[0].replace("failed", "error")
    return "error exit"


def fmt(x, nd=2, unit=""):
    return "n/a" if x is None else f"{x:.{nd}f}{unit}"


def verdict(k, t, axis):
    """Komla fast (k) against a tool (t) on one axis: (word, margin text)."""
    if axis == "size":
        d = k["saved"] - t["saved"]
        return ("win" if d > 0 else "loss" if d < 0 else "tie"), f"{d:+.2f} points"
    if axis in ("enc", "dec"):
        if not k[axis] or not t[axis]:
            return "n/a", ""
        r = k[axis] / t[axis] - 1
        return ("win" if r > TIE else "loss" if r < -TIE else "tie"), f"{100 * r:+.0f}% speed"
    a, b = k[axis], t[axis]
    if not a or not b:
        return "n/a", ""
    r = a / b - 1
    return ("win" if r < -TIE else "loss" if r > TIE else "tie"), f"{100 * r:+.0f}% memory"


def set_section(name, rows):
    title, what = SETS[name]
    images = sorted({r["image"] for r in rows})
    total = sum({r["image"]: r["input"] for r in rows}.values())
    table = {}
    for cid, _ in CODECS:
        for th in ("1t", "mt"):
            rs = [r for r in rows if r["codec"] == cid and r["threads"] == th]
            if rs and len(rs) == len(images):
                table[(cid, th)] = summary(rs)
    out = [f"## {title} ({name})", "", f"{len(images)} files, {total:,} bytes; {what}.", ""]
    for th, head in (("1t", "One thread"), ("mt", "All threads (8 cores)")):
        out += [f"### {head}", "", "| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed | Run-to-run spread, compress / decompress |", "|---|---|---|---|---|---|---|---|"]
        for cid, label in CODECS:
            v = table.get((cid, th))
            if not v:
                continue
            shown = f"**{label}**" if cid == "komla-fast" else label + (" \\*" if v["separate"] else "")
            sp = v["spread"]
            spread = f"{fmt(100 * sp['enc_all'], 1, '%') if 'enc_all' in sp else 'n/a'} / {fmt(100 * sp['dec_all'], 1, '%') if 'dec_all' in sp else 'n/a'}"
            fails = f"{v['fails']}" + (f" (stored: {v['stored']})" if v["stored"] else "")
            out.append(f"| {shown} | {fmt(v['saved'])}% | {fmt(v['enc'])} | {fmt(v['dec'])} | {fmt(v['enc_mib'], 0, ' MiB')} | {fmt(v['dec_mib'], 0, ' MiB')} | {fails} | {spread} |")
        out.append("")
    if any(v["separate"] for v in table.values()):
        out += ["\\* paq8px takes hours per set, so it ran separately (several files at a time, alongside other work, not interleaved with the other tools); its times are indicative only." + (" On this set its sizes and times come from an earlier run (`results/jpeg_test.jsonl`) and its memory was not measured." if name == "test" else ""), ""]
    fails = [(LABEL[c], th, v) for (c, th), v in table.items() if v["fails"]]
    if fails:
        out += ["Failures (a rejected file, a crash or a restored file that differs; counted at the original size): " + "; ".join(f"{l} ({th}): {v['fails']} — " + ", ".join(f"{n}× {e}" for e, n in v["errors"].items()) for l, th, v in fails) + ".", ""]
    else:
        out += ["Every tool restored every file byte for byte.", ""]
    stored = [(LABEL[c], th, v) for (c, th), v in table.items() if v["stored"]]
    if stored:
        out += ["Komla stored some files without recompressing them (its fallback for input it cannot reproduce exactly): " + "; ".join(f"{l} ({th}): {v['stored']}" for l, th, v in stored) + ".", ""]
    out += ["### Komla fast against each tool", "", f"Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within {TIE * 100:.0f}% count as ties (margin: Komla relative to the tool).", ""]
    out += ["| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |", "|---|---|---|---|---|---|---|"]
    tally = collections.Counter()
    for cid, label in CODECS:
        if cid == "komla-fast":
            continue
        for th in ("1t", "mt"):
            k, t = table.get(("komla-fast", th)), table.get((cid, th))
            if not k or not t:
                continue
            cells = []
            for axis in ("size", "enc", "dec", "enc_mib", "dec_mib"):
                w, m = verdict(k, t, axis)
                if cid != "komla":
                    tally[(th, w)] += 1
                cells.append(f"{w} ({m})" if m else w)
            out.append(f"| {label} | {th} | " + " | ".join(cells) + " |")
    out += ["", "Against the other tools (Komla's own maximum mode not counted): " + "; ".join(f"{'one thread' if th == '1t' else 'all threads'}: {tally[(th, 'win')]} wins, {tally[(th, 'tie')]} ties, {tally[(th, 'loss')]} losses" for th in ("1t", "mt")) + ".", ""]
    groups = []
    for r in rows:
        if r["group"] not in groups:
            groups.append(r["group"])
    out += ["### Size by group", "", "| Tool | " + " | ".join(f"{GROUP_LABEL.get(g, g)}<br>{sum(1 for i in images if f'/{g}/' in i)} files" for g in groups) + " | **Total** |", "|---" * (len(groups) + 2) + "|"]
    for cid, label in CODECS:
        rs = [r for r in rows if r["codec"] == cid and r["threads"] == "1t"]
        if len(rs) != len(images):
            continue
        cells = []
        for g in groups:
            gr = [r for r in rs if r["group"] == g]
            i = sum(r["input"] for r in gr)
            o = sum(r["bytes"] if r["ok"] else r["input"] for r in gr)
            cells.append(f"{100 * (1 - o / i):.2f}%")
        shown = f"**{label}**" if cid == "komla-fast" else label
        out.append(f"| {shown} | " + " | ".join(cells) + f" | {fmt(table[(cid, '1t')]['saved'])}% |")
    out.append("")
    return out, table, len(images), total


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sets", default="heldout,dev,test")
    ap.add_argument("--site", default="heldout")
    ap.add_argument("--intro", default=os.path.join(HERE, "results", "jpeg_speed_intro.md"))
    args = ap.parse_args()
    out = ["# Lossless JPEG recompression: size, speed and memory", ""]
    if os.path.exists(args.intro):
        out += open(args.intro).read().rstrip().split("\n") + [""]
    tables = {}
    for name in args.sets.split(","):
        rows = load(name)
        if not rows:
            continue
        sec, table, n, total = set_section(name, rows)
        tables[name] = (table, n, total)
        out += sec
    out += ["## Tools", "", "| Tool | Version and command |", "|---|---|"] + [f"| {a} | {b} |" for a, b in TOOLS] + [""]
    with open(os.path.join(HERE, "results", "JPEG_SPEED_REPORT.md"), "w") as f:
        f.write("\n".join(out))
    print("\n".join(out))
    if args.site in tables:
        table, n, total = tables[args.site]
        data = {
            "set": args.site,
            "files": n,
            "bytes": total,
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
