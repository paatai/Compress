#!/usr/bin/env python3
"""Turns the results of run_jpeg_speed.py (and run_paq8px.py) into
bench/results/JPEG_SPEED_REPORT.md and the website's web/src/data/jpeg-speed.json.

usage: make_jpeg_speed_report.py [--sets heldout2,heldout,dev] [--site heldout2]
"""
import argparse
import collections
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CODECS = [
    ("komla-strong", "Komla strong"),
    ("komla-fast", "Komla fast"),
    ("komla-pr2", "Komla fast (PR #2)"),
    ("komla", "Komla (maximum)"),
    ("jxl", "JPEG XL"),
    ("lepton", "Lepton (Dropbox)"),
    ("lepton-rust", "Lepton (Rust port)"),
    ("brunsli", "brunsli"),
    ("packjpg", "packJPG"),
    ("paq8px", "paq8px"),
]
LABEL = dict(CODECS)
# the Komla modes compared with every other tool; Komla's own modes are not counted as tools
KOMLA = ["komla-strong", "komla-fast"]
OWN = {"komla-strong", "komla-fast", "komla-pr2", "komla"}
SETS = {
    "heldout2": ("New held-out random set", "drawn at random for this evaluation and frozen by SHA-256 before any work on the model (see above); never used for tuning; Komla's code and parameters were frozen before its first run, and it was run once"),
    "heldout": ("Previous held-out random set", "drawn at random for the previous evaluation, where Komla's fast mode ran on it once; not used for tuning since"),
    "dev": ("Development set", "used for all tuning of Komla (contexts, priors, parameters)"),
    "test": ("Test set", "held out from tuning, but already used in earlier evaluations of Komla"),
}
GROUP_LABEL = {
    "kodak_q50": "Kodak q50", "kodak_q75": "Kodak q75", "kodak_q90_444": "Kodak q90 4:4:4", "kodak_q95_prog": "Kodak q95 progressive",
    "camera": "Camera photos", "progressive": "Progressive", "gray": "Grayscale", "screenshot": "Screenshots",
    "photo": "Edited photos", "scan": "Scans, paintings, maps", "web": "Small web images",
}
TOOLS = [
    ("Komla strong", "this repository: `komla compress --strong`, `komla decompress`; `KOMLA_THREADS=1` for one thread, else all cores"),
    ("Komla fast", "this repository: `komla compress --fast`, `komla decompress` (same thread settings)"),
    ("Komla fast (PR #2)", "the fast mode as of the previous evaluation (commit 8430279), the reference for the strong mode's speed budget (same commands)"),
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
    """A Komla mode (k) against a tool (t) on one axis: (word, margin text)."""
    if axis == "size":
        d = k["saved"] - t["saved"]
        return ("win" if d > 0 else "loss" if d < 0 else "tie"), f"{d:+.2f} points"
    if axis in ("enc", "dec"):
        if not k[axis] or not t[axis]:
            return "n/a", ""
        r = k[axis] / t[axis] - 1
        margin = f"{r + 1:.0f}x speed" if r >= 2 else f"{100 * r:+.0f}% speed"
        return ("win" if r > TIE else "loss" if r < -TIE else "tie"), margin
    a, b = k[axis], t[axis]
    if not a or not b:
        return "n/a", ""
    r = a / b - 1
    return ("win" if r < -TIE else "loss" if r > TIE else "tie"), f"{100 * r:+.0f}% memory"


def paq_subset(rows, images):
    """When paq8px finished only part of a set: every tool's size on exactly those files."""
    done = {r["image"] for r in rows if r["codec"] == "paq8px" and r["threads"] == "1t"}
    if not done or len(done) == len(images):
        return []
    inp = sum({r["image"]: r["input"] for r in rows if r["image"] in done}.values())
    out = [f"### paq8px on part of the set", "", f"paq8px finished {len(done)} of the {len(images)} files ({inp:,} bytes) in the time available (it runs at about 0.005 to 0.02 MB/s per file here). On exactly those files, one thread:", "",
           "| Tool | Smaller | Compress MB/s | Decompress MB/s | Peak memory compress | Peak memory decompress | Round trip failed |", "|---|---|---|---|---|---|---|"]
    for cid, label in CODECS:
        rs = [r for r in rows if r["codec"] == cid and r["threads"] == "1t" and r["image"] in done]
        if len(rs) != len(done):
            continue
        v = summary(rs)
        shown = f"**{label}**" if cid in KOMLA else label + (" \\*" if v["separate"] else "")
        out.append(f"| {shown} | {fmt(v['saved'])}% | {fmt(v['enc'], 3 if v['separate'] else 2)} | {fmt(v['dec'], 3 if v['separate'] else 2)} | {fmt(v['enc_mib'], 0, ' MiB')} | {fmt(v['dec_mib'], 0, ' MiB')} | {v['fails']} |")
    return out + ["", "\\* paq8px times are processor seconds of each process (it ran several files at a time, separately from the other tools); the first files it ran were timed by the clock while other work ran.", ""]


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
            shown = f"**{label}**" if cid in KOMLA else label + (" \\*" if v["separate"] else "")
            sp = v["spread"]
            spread = f"{fmt(100 * sp['enc_all'], 1, '%') if 'enc_all' in sp else 'n/a'} / {fmt(100 * sp['dec_all'], 1, '%') if 'dec_all' in sp else 'n/a'}"
            fails = f"{v['fails']}" + (f" (stored: {v['stored']})" if v["stored"] else "")
            out.append(f"| {shown} | {fmt(v['saved'])}% | {fmt(v['enc'])} | {fmt(v['dec'])} | {fmt(v['enc_mib'], 0, ' MiB')} | {fmt(v['dec_mib'], 0, ' MiB')} | {fails} | {spread} |")
        out.append("")
    if any(v["separate"] for v in table.values()):
        out += ["\\* paq8px takes hours per set, so it ran separately (several files at a time, alongside other work, not interleaved with the other tools); its times are indicative only." + (" On this set its sizes and times come from an earlier run (`results/jpeg_test.jsonl`) and its memory was not measured." if name == "test" else ""), ""]
    ok = "; ".join(f"{LABEL[c]} {th}: {v['files'] - v['fails']}/{v['files']}" for (c, th), v in table.items() if not (v["separate"] or (th == "mt" and c in ("brunsli", "packjpg", "paq8px"))))
    out += [f"Files restored byte for byte: {ok}.", ""]
    fails = [(LABEL[c], th, v) for (c, th), v in table.items() if v["fails"]]
    if fails:
        out += ["Failures (counted at the original size): " + "; ".join(f"{l} ({th}): {v['fails']} — " + ", ".join(f"{n}× {e}" for e, n in v["errors"].items()) for l, th, v in fails) + "."]
        if not any("crashed" in e for _, _, v in fails for e in v["errors"]):
            out += ["No tool crashed or restored a file wrongly: every failure was the tool refusing the file (unsupported progressive scan scripts or end-of-band runs, or Lepton's own round-trip check failing)."]
        out.append("")
    for kid in KOMLA:
        ks = [v for (c, th), v in table.items() if c == kid]
        if ks:
            n = max(v["stored"] for v in ks)
            out += [f"{LABEL[kid]} stored {n} file(s) without recompressing them (its fallback for input it cannot reproduce exactly)." if n else f"{LABEL[kid]} coded every file (no file fell back to being stored).", ""]
    ref = [(th, table.get(("komla-strong", th)), table.get(("komla-pr2", th)), table.get(("komla-fast", th))) for th in ("1t", "mt")]
    if all(s_ and p_ for _, s_, p_, _ in ref):
        out += ["### Komla strong against the fast mode", "", "Time and peak memory of the strong mode as multiples of the fast mode of the previous evaluation (PR #2, the reference of the speed budget: within about twice its time) and of this version's fast mode.", "",
                "| Threads | Compression time | Decompression time | Memory, compression | Memory, decompression |", "|---|---|---|---|---|"]
        for th, s_, p_, f_ in ref:
            for name_, b in (("PR #2 fast", p_), ("fast", f_)):
                if b:
                    out.append(f"| {th}, vs {name_} | {b['enc'] / s_['enc']:.2f}x | {b['dec'] / s_['dec']:.2f}x | {s_['enc_mib'] / b['enc_mib']:.2f}x | {s_['dec_mib'] / b['dec_mib']:.2f}x |")
        out.append("")
    def count(n, word):
        return f"{n} {word}" + ("" if n == 1 else ("es" if word.endswith("s") else "s"))
    for kid in KOMLA:
        if not any(c == kid for c, _ in table):
            continue
        out += [f"### {LABEL[kid]} against each tool", "", f"Size is exact, so any difference counts (margin in percentage points of saving); speed and memory within {TIE * 100:.0f}% count as ties (margin: Komla relative to the tool).", ""]
        out += ["| Tool | Threads | Size | Compression speed | Decompression speed | Memory, compression | Memory, decompression |", "|---|---|---|---|---|---|---|"]
        tally = collections.Counter()
        for cid, label in CODECS:
            if cid in OWN:
                continue
            for th in ("1t", "mt"):
                k, t = table.get((kid, th)), table.get((cid, th))
                if not k or not t:
                    continue
                cells = []
                for axis in ("size", "enc", "dec", "enc_mib", "dec_mib"):
                    w, m = verdict(k, t, axis)
                    tally[(th, w)] += 1
                    cells.append(f"{w} ({m})" if m else w)
                out.append(f"| {label} | {th} | " + " | ".join(cells) + " |")
        out += ["", f"{LABEL[kid]} against the other tools (Komla's own modes not counted): " + "; ".join(f"{'one thread' if th == '1t' else 'all threads'}: {count(tally[(th, 'win')], 'win')}, {count(tally[(th, 'tie')], 'tie')}, {count(tally[(th, 'loss')], 'loss')}" for th in ("1t", "mt")) + ".", ""]
    out += paq_subset(rows, images)
    groups = []
    for r in rows:
        if r["group"] not in groups:
            groups.append(r["group"])
    out += ["### Size by group", "", "| Tool | " + " | ".join(f"{GROUP_LABEL.get(g, g)}<br>{(n := sum(1 for i in images if f'/{g}/' in i))} file{'' if n == 1 else 's'}" for g in groups) + " | **Total** |", "|---" * (len(groups) + 2) + "|"]
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
        shown = f"**{label}**" if cid in KOMLA else label
        out.append(f"| {shown} | " + " | ".join(cells) + f" | {fmt(table[(cid, '1t')]['saved'])}% |")
    out.append("")
    return out, table, len(images), total


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sets", default="heldout2,heldout,dev")
    ap.add_argument("--site", default="", help="also write the website's table for this set")
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
    shown = {LABEL[r["codec"]] for name in args.sets.split(",") for r in load(name)}
    out += ["## Tools", "", "| Tool | Version and command |", "|---|---|"] + [f"| {a} | {b} |" for a, b in TOOLS if a in shown] + [""]
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
