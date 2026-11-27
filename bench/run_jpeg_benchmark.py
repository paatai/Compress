#!/usr/bin/env python3
"""Runs Komla and the lossless JPEG recompressors over a JPEG benchmark set.

Every output is decompressed again and compared with the original file byte for byte (SHA-256).
A codec that rejects a file or does not reproduce it exactly is counted at the original file
size (as if the file had been stored) and reported as a failure.

usage: run_jpeg_benchmark.py --set dev|test [--codecs a,b,...] [--force a,b] [--jobs N] [--dir DIR]
"""
import argparse
import concurrent.futures as cf
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
TOOLS = os.environ.get("TOOLS", os.path.join(HERE, ".tools", "bin"))
KOMLA = os.environ.get("KOMLA", os.path.join(ROOT, "target", "release", "komla"))


def t(name):
    return os.path.join(TOOLS, name)


# name: (encode command, output file name, decode command) run in a scratch directory that holds
# the input as in.jpg; {out} is the compressed file, the decoder must write dec.jpg.
CODECS = {
    "komla": ([KOMLA, "compress", "in.jpg", "-o", "{out}"], "out.komla", [KOMLA, "decompress", "{out}", "-o", "dec.jpg"]),
    # single-threaded Lepton fails its own round-trip check on some progressive files; those are
    # retried in Lepton's default (multi-threaded) mode
    "lepton": (["sh", "-c", f"{t('lepton')} -unjailed -singlethread -allowprogressive -memory=4096M -threadmemory=2048M in.jpg {{out}} >/dev/null 2>&1"
                f" || {t('lepton')} -unjailed -allowprogressive -memory=4096M -threadmemory=2048M in.jpg {{out}}"], "out.lep",
               [t("lepton"), "-unjailed", "-memory=4096M", "-threadmemory=2048M", "{out}", "dec.jpg"]),
    "lepton-rust": ([t("lepton-rust"), "--threads", "1", "--quiet", "--noverify", "in.jpg", "{out}"], "out.lep", [t("lepton-rust"), "--threads", "1", "--quiet", "{out}", "dec.jpg"]),
    "jxl": ([t("cjxl"), "--lossless_jpeg=1", "-e", "9", "--num_threads=0", "--quiet", "in.jpg", "{out}"], "out.jxl", [t("djxl"), "--num_threads=0", "--quiet", "{out}", "dec.jpg"]),
    "packjpg": ([t("packJPG"), "-np", "-o", "in.jpg"], "in.pjg", ["sh", "-c", f"mv in.jpg orig.jpg && {t('packJPG')} -np -o in.pjg && mv in.jpg dec.jpg && mv orig.jpg in.jpg"]),
    "paq8px": ([t("paq8px"), "-8", "in.jpg", "{out}"], "out.paq8px217", ["sh", "-c", f"mkdir -p d && {t('paq8px')} -d {{out}} d/x.jpg >/dev/null && mv d/x.jpg dec.jpg"]),
    "brunsli": ([t("cbrunsli"), "in.jpg", "{out}"], "out.brn", [t("dbrunsli"), "{out}", "dec.jpg"]),
}
ORDER = ["brunsli", "packjpg", "jxl", "lepton", "lepton-rust", "paq8px", "komla"]
# memory-hungry codecs get fewer simultaneous jobs
LIMITS = {"paq8px": int(os.environ.get("PAQ8PX_JOBS", "3")), "lepton": 4}
GROUPS = ["kodak_q50", "kodak_q75", "kodak_q90_444", "kodak_q95_prog", "camera", "progressive", "gray", "screenshot"]


def run(cmd, cwd):
    t0 = time.perf_counter()
    r = subprocess.run(cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=dict(os.environ, KOMLA_THREADS="1"))
    dt = time.perf_counter() - t0
    if r.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd)} failed: {r.stderr.decode(errors='replace')[:300]}")
    return dt


sems = {c: threading.Semaphore(n) for c, n in LIMITS.items()}


def job(args):
    codec, img, group, size = args
    enc, outname, dec = CODECS[codec]
    sem = sems.get(codec)
    if sem:
        sem.acquire()
    try:
        with tempfile.TemporaryDirectory() as td:
            shutil.copyfile(img, os.path.join(td, "in.jpg"))
            fill = lambda c: [x.format(out=outname) for x in c]
            res = {"codec": codec, "image": os.path.relpath(img, HERE), "group": group, "input": size}
            try:
                enc_s = run(fill(enc), td)
                out = os.path.getsize(os.path.join(td, outname))
                dec_s = run(fill(dec), td)
                ok = hashlib.sha256(open(os.path.join(td, "dec.jpg"), "rb").read()).digest() == hashlib.sha256(open(img, "rb").read()).digest()
                res.update(bytes=out, enc_s=round(enc_s, 3), dec_s=round(dec_s, 3), ok=ok)
            except Exception as e:
                res.update(bytes=None, enc_s=0, dec_s=0, ok=False, error=str(e)[:200])
            return res
    finally:
        if sem:
            sem.release()


def load(path):
    res = {}
    if os.path.exists(path):
        for line in open(path):
            r = json.loads(line)
            res[(r["codec"], r["image"])] = r
    return res


def summarize(results, codecs, title):
    groups = [g for g in GROUPS if any(r["group"] == g for r in results.values())]
    groups += sorted({r["group"] for r in results.values()} - set(groups))
    lines = [f"{title}: % smaller than the original JPEG files (higher is better); failed files count at their original size",
             f"{'codec':<12}" + "".join(f"{g[:13]:>14}" for g in groups) + f"{'TOTAL':>10}{'bytes':>13}{'fails':>7}{'enc MB/s':>10}{'dec MB/s':>10}"]
    for codec in codecs:
        rs = [r for r in results.values() if r["codec"] == codec]
        if not rs:
            continue
        row = f"{codec:<12}"
        for g in groups:
            gr = [r for r in rs if r["group"] == g]
            i = sum(r["input"] for r in gr)
            o = sum(r["bytes"] if r["ok"] else r["input"] for r in gr)
            row += f"{(100 * (1 - o / i) if i else float('nan')):>14.2f}"
        i = sum(r["input"] for r in rs)
        o = sum(r["bytes"] if r["ok"] else r["input"] for r in rs)
        okr = [r for r in rs if r["ok"]]
        es = sum(r["enc_s"] for r in okr)
        ds = sum(r["dec_s"] for r in okr)
        ib = sum(r["input"] for r in okr)
        row += f"{100 * (1 - o / i):>10.2f}{o:>13}{len(rs) - len(okr):>7}{(ib / es / 1e6 if es else 0):>10.2f}{(ib / ds / 1e6 if ds else 0):>10.2f}"
        lines.append(row)
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", default="dev")
    ap.add_argument("--dir", help="benchmark every .jpg under DIR instead of a named set (results cached in bench/out)")
    ap.add_argument("--codecs", default=",".join(ORDER))
    ap.add_argument("--force", default="")
    ap.add_argument("--jobs", type=int, default=os.cpu_count())
    args = ap.parse_args()
    codecs = [c for c in args.codecs.split(",") if c]
    force = set(c for c in args.force.split(",") if c)
    base = args.dir or os.path.join(HERE, "data", "jpeg", args.set)
    imgs = []
    for dp, _, fs in sorted(os.walk(base)):
        for f in sorted(fs):
            if f.lower().endswith((".jpg", ".jpeg")):
                p = os.path.join(dp, f)
                imgs.append((p, os.path.basename(dp) if dp != base else "all", os.path.getsize(p)))
    name = os.path.basename(os.path.normpath(args.dir)) if args.dir else args.set
    rpath = os.path.join(HERE, "results", f"jpeg_{name}.jsonl") if not args.dir else os.path.join(HERE, "out", f"jpeg_dir_{name}.jsonl")
    results = load(rpath)
    results.update(load(rpath + ".partial"))  # results of an interrupted run
    todo = [(c, p, g, s) for c in codecs for p, g, s in imgs if c in force or (c, os.path.relpath(p, HERE)) not in results]
    print(f"{len(imgs)} files, {len(todo)} jobs", file=sys.stderr)
    with cf.ThreadPoolExecutor(max_workers=args.jobs) as ex:
        futs = [ex.submit(job, j) for j in todo]
        for i, f in enumerate(cf.as_completed(futs)):
            r = f.result()
            results[(r["codec"], r["image"])] = r
            if rpath:
                os.makedirs(os.path.dirname(rpath), exist_ok=True)
                with open(rpath + ".partial", "a") as fo:
                    fo.write(json.dumps(r) + "\n")
            if not r["ok"]:
                print(f"FAILED {r['codec']} {r['image']}: {r.get('error', 'output differs from the original')}", file=sys.stderr)
            if (i + 1) % 50 == 0:
                print(f"  {i + 1}/{len(todo)}", file=sys.stderr)
    if rpath:
        os.makedirs(os.path.dirname(rpath), exist_ok=True)
        with open(rpath, "w") as fo:
            for k in sorted(results):
                fo.write(json.dumps(results[k]) + "\n")
        if os.path.exists(rpath + ".partial"):
            os.remove(rpath + ".partial")
    names = {os.path.relpath(p, HERE) for p, _, _ in imgs}
    sel = {k: v for k, v in results.items() if k[1] in names}
    print(summarize(sel, [c for c in ORDER if c in codecs] + [c for c in codecs if c not in ORDER], args.set if not args.dir else args.dir))


if __name__ == "__main__":
    main()
