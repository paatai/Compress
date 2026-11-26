#!/usr/bin/env python3
"""Runs Kolmo and the baseline compressors over a benchmark set and reports bits per pixel.

Every image-format result is decoded again and compared with the original by a SHA-256 of the
pixels normalised to RGBA (`kolmo pixhash`); general-purpose archivers compress a BMP of the
image and are checked by extracting the archive and comparing bytes.

usage: run_benchmark.py --set dev|test [--codecs a,b,...] [--force a,b] [--jobs N]
"""
import argparse
import concurrent.futures as cf
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
TOOLS = os.environ.get("TOOLS", os.path.join(HERE, ".tools", "bin"))
KOLMO = os.environ.get("KOLMO", os.path.join(ROOT, "target", "release", "kolmo"))


def t(name):
    return os.path.join(TOOLS, name)


# name: (input kind, encode command, output extension, decode command or None, extract command or None)
CODECS = {
    "zip": ("bmp", [t("zip") if os.path.exists(t("zip")) else "zip", "-9", "-X", "-q", "-j", "{out}", "{inp}"], "zip", None, ["unzip", "-p", "{out}"]),
    "rar": ("bmp", [t("rar"), "a", "-m5", "-ep", "-idq", "-o+", "{out}", "{inp}"], "rar", None, [t("rar"), "p", "-inul", "{out}"]),
    "7z": ("bmp", [t("7zz"), "a", "-mx=9", "-mmt=1", "-bd", "-bso0", "-bsp0", "{out}", "{inp}"], "7z", None, [t("7zz"), "e", "-so", "{out}"]),
    "xz": ("bmp", ["sh", "-c", "xz -9e -T1 -k -c {inp} > {out}"], "xz", None, ["xz", "-dc", "{out}"]),
    "png-oxipng": ("png", [t("oxipng"), "-o", "max", "--strip", "all", "-q", "--out", "{out}", "{inp}"], "png", "copy", None),
    "png-zopfli": ("png", [t("zopflipng"), "-m", "-y", "{inp}", "{out}"], "png", "copy", None),
    "qoi": ("png", [t("qoiconv"), "{inp}", "{out}"], "qoi", [t("qoiconv"), "{out}", "{dec}"], None),
    "flif": ("png", [t("flif"), "-e", "-E100", "-K", "-o", "{inp}", "{out}"], "flif", [t("flif"), "-d", "-o", "{out}", "{dec}"], None),
    "avif": ("png", [t("avifenc"), "--lossless", "-s", "0", "-j", "1", "{inp}", "{out}"], "avif", [t("avifdec"), "-j", "1", "{out}", "{dec}"], None),
    "webp": ("png", [t("cwebp"), "-lossless", "-z", "9", "-exact", "-metadata", "none", "-quiet", "{inp}", "-o", "{out}"], "webp", [t("dwebp"), "-quiet", "{out}", "-o", "{dec}"], None),
    "jxl-e9": ("png", [t("cjxl"), "-d", "0", "-e", "9", "--num_threads=0", "--quiet", "{inp}", "{out}"], "jxl", [t("djxl"), "--quiet", "{out}", "{dec}"], None),
    "jxl-e10": ("png", [t("cjxl"), "-d", "0", "-e", "10", "--num_threads=0", "--quiet", "{inp}", "{out}"], "jxl", [t("djxl"), "--quiet", "{out}", "{dec}"], None),
    "kolmo": ("png", [KOLMO, "compress", "{inp}", "-o", "{out}"], "kolmo", [KOLMO, "decompress", "{out}", "-o", "{dec}"], None),
}
ORDER = ["zip", "rar", "7z", "xz", "qoi", "png-oxipng", "png-zopfli", "flif", "avif", "webp", "jxl-e9", "jxl-e10", "kolmo"]
CATEGORIES = ["photo_kodak", "photo_div2k", "screenshot", "graphics"]


def run(cmd, **kw):
    t0 = time.perf_counter()
    r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kw)
    dt = time.perf_counter() - t0
    if r.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd)} failed: {r.stderr.decode(errors='replace')[:400]}")
    return r.stdout, dt


def pixhash(path):
    out, _ = run([KOLMO, "pixhash", path])
    return out.split()[0].decode()


def make_bmp(png, bmp):
    from PIL import Image
    im = Image.open(png)
    if im.mode == "P":
        im = im.convert("RGBA" if "transparency" in im.info else "RGB")
    elif im.mode in ("LA", "PA"):
        im = im.convert("RGBA")
    elif im.mode not in ("L", "RGB", "RGBA"):
        im = im.convert("RGBA" if "A" in im.mode else "RGB")
    im.save(bmp)


def job(args):
    codec, img, category, pixels, ref_hash = args
    kind, enc, ext, dec, extract = CODECS[codec]
    with tempfile.TemporaryDirectory() as td:
        inp = img
        if kind == "bmp":
            inp = os.path.join(td, "image.bmp")
            make_bmp(img, inp)
        out = os.path.join(td, "out." + ext)
        dec_path = os.path.join(td, "dec.png")
        fill = lambda c: [x.format(inp=inp, out=out, dec=dec_path) for x in c]
        _, enc_s = run(fill(enc))
        size = os.path.getsize(out)
        dec_s = 0.0
        if extract:
            data, dec_s = run(fill(extract))
            ok = data == open(inp, "rb").read()
        else:
            if dec == "copy":
                dec_path = out
            else:
                _, dec_s = run(fill(dec))
            ok = pixhash(dec_path) == ref_hash
    return {"codec": codec, "image": os.path.relpath(img, HERE), "category": category, "pixels": pixels,
            "bytes": size, "enc_s": round(enc_s, 3), "dec_s": round(dec_s, 3), "ok": ok}


def load(path):
    res = {}
    if os.path.exists(path):
        for line in open(path):
            r = json.loads(line)
            res[(r["codec"], r["image"])] = r
    return res


def summarize(results, codecs, set_name):
    cats = [c for c in CATEGORIES if any(r["category"] == c for r in results.values())]
    lines = []
    header = f"{'codec':<12}" + "".join(f"{c:>14}" for c in cats) + f"{'TOTAL':>14}{'bytes':>14}{'lossless':>10}"
    lines.append(f"set={set_name}  bits per pixel (lower is better)")
    lines.append(header)
    totals = {}
    for codec in codecs:
        rs = [r for r in results.values() if r["codec"] == codec]
        if not rs:
            continue
        row = f"{codec:<12}"
        for c in cats:
            cr = [r for r in rs if r["category"] == c]
            px = sum(r["pixels"] for r in cr)
            row += f"{(sum(r['bytes'] for r in cr) * 8 / px if px else float('nan')):>14.4f}"
        px = sum(r["pixels"] for r in rs)
        b = sum(r["bytes"] for r in rs)
        totals[codec] = b
        row += f"{b * 8 / px:>14.4f}{b:>14}{'yes' if all(r['ok'] for r in rs) else 'NO':>10}"
        lines.append(row)
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", default="dev")
    ap.add_argument("--codecs", default=",".join(ORDER))
    ap.add_argument("--force", default="")
    ap.add_argument("--jobs", type=int, default=os.cpu_count())
    ap.add_argument("--categories", default=",".join(CATEGORIES))
    args = ap.parse_args()
    codecs = [c for c in args.codecs.split(",") if c]
    force = set(c for c in args.force.split(",") if c)
    cats = args.categories.split(",")
    base = os.path.join(HERE, "data", args.set)
    imgs = []
    for c in cats:
        d = os.path.join(base, c)
        if os.path.isdir(d):
            imgs += [(os.path.join(d, f), c) for f in sorted(os.listdir(d)) if f.endswith(".png")]
    os.makedirs(os.path.join(HERE, "results"), exist_ok=True)
    rpath = os.path.join(HERE, "results", f"{args.set}.jsonl")
    results = load(rpath)
    meta = {}
    for img, c in imgs:
        out, _ = run([KOLMO, "pixhash", img])
        h, dims = out.split()[:2]
        w, rest = dims.decode().split("x", 1)
        hh = rest.split("x")[0]
        meta[img] = (c, int(w) * int(hh), h.decode())
    todo = [(codec, img, meta[img][0], meta[img][1], meta[img][2]) for codec in codecs for img, _ in imgs
            if codec in force or (codec, os.path.relpath(img, HERE)) not in results]
    print(f"{len(todo)} jobs", file=sys.stderr)
    with cf.ThreadPoolExecutor(max_workers=args.jobs) as ex:
        futs = {ex.submit(job, j): j for j in todo}
        for i, f in enumerate(cf.as_completed(futs)):
            try:
                r = f.result()
            except Exception as e:
                print(f"FAILED {futs[f][0]} {futs[f][1]}: {e}", file=sys.stderr)
                continue
            results[(r["codec"], r["image"])] = r
            if not r["ok"]:
                print(f"NOT LOSSLESS: {r['codec']} {r['image']}", file=sys.stderr)
            if (i + 1) % 25 == 0:
                print(f"  {i + 1}/{len(todo)}", file=sys.stderr)
    with open(rpath, "w") as fo:
        for k in sorted(results):
            fo.write(json.dumps(results[k]) + "\n")
    sel = {k: v for k, v in results.items() if v["category"] in cats and any(os.path.relpath(i, HERE) == k[1] for i, _ in imgs)}
    print(summarize(sel, [c for c in ORDER if c in codecs] + [c for c in codecs if c not in ORDER], args.set))


if __name__ == "__main__":
    main()
