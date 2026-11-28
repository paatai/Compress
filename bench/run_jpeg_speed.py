#!/usr/bin/env python3
"""Size, speed and peak memory of lossless JPEG recompressors, one process at a time.

Every codec runs on every file twice: single-threaded ("1t") and at its default thread count
("mt"; for codecs without threads the "mt" row repeats the "1t" measurement). The fastest of
--reps runs counts (one run for the slow default Komla mode). Each encode and decode is timed (wall
clock) and its peak resident memory read from /usr/bin/time; the output of every decode is
compared with the original file byte for byte (SHA-256). A codec that rejects a file or does not
restore it exactly is counted at the original size and reported as a failure. Files are run in
order, all codecs on one file before the next, so that every codec sees the same conditions.

Every run's times are kept (enc_all, dec_all) to show the run-to-run variation; for Komla the
container mode is recorded (3 fast mode, 5 strong mode, 2 maximum mode; 0 and 4 mean the file was
stored). "komla-pr2" is the fast mode as of the previous evaluation (bench/.tools/bin/komla-pr2,
built from commit 8430279), kept as the reference for the strong mode's speed.

usage: run_jpeg_speed.py --set dev|test|heldout|heldout2 [--codecs a,b] [--threads 1t,mt] [--reps N] [--force a,b]
"""
import argparse
import hashlib
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
KOMLA = os.environ.get("KOMLA", os.path.join(ROOT, "target", "release", "komla"))
SCRATCH = "/dev/shm" if os.path.isdir("/dev/shm") else None


def t(name):
    return os.path.join(TOOLS, name)


LEP = [t("lepton"), "-unjailed", "-allowprogressive", "-memory=4096M", "-threadmemory=2048M"]

# codec: {variant: (encode command, output name, decode command, environment)}; commands run in a
# scratch directory holding the input as in.jpg and must leave the restored file in dec.jpg
KOMLA_PR2 = os.environ.get("KOMLA_PR2", t("komla-pr2"))
CODECS = {
    "komla-strong": {
        "1t": ([KOMLA, "compress", "--strong", "in.jpg", "-o", "out.komla"], "out.komla", [KOMLA, "decompress", "out.komla", "-o", "dec.jpg"], {"KOMLA_THREADS": "1"}),
        "mt": ([KOMLA, "compress", "--strong", "in.jpg", "-o", "out.komla"], "out.komla", [KOMLA, "decompress", "out.komla", "-o", "dec.jpg"], {}),
    },
    "komla-pr2": {
        "1t": ([KOMLA_PR2, "compress", "--fast", "in.jpg", "-o", "out.komla"], "out.komla", [KOMLA_PR2, "decompress", "out.komla", "-o", "dec.jpg"], {"KOMLA_THREADS": "1"}),
        "mt": ([KOMLA_PR2, "compress", "--fast", "in.jpg", "-o", "out.komla"], "out.komla", [KOMLA_PR2, "decompress", "out.komla", "-o", "dec.jpg"], {}),
    },
    "komla-fast": {
        "1t": ([KOMLA, "compress", "--fast", "in.jpg", "-o", "out.komla"], "out.komla", [KOMLA, "decompress", "out.komla", "-o", "dec.jpg"], {"KOMLA_THREADS": "1"}),
        "mt": ([KOMLA, "compress", "--fast", "in.jpg", "-o", "out.komla"], "out.komla", [KOMLA, "decompress", "out.komla", "-o", "dec.jpg"], {}),
    },
    "komla": {
        "1t": ([KOMLA, "compress", "in.jpg", "-o", "out.komla"], "out.komla", [KOMLA, "decompress", "out.komla", "-o", "dec.jpg"], {"KOMLA_THREADS": "1"}),
        "mt": ([KOMLA, "compress", "in.jpg", "-o", "out.komla"], "out.komla", [KOMLA, "decompress", "out.komla", "-o", "dec.jpg"], {}),
    },
    "jxl": {
        "1t": ([t("cjxl"), "--lossless_jpeg=1", "--num_threads=0", "--quiet", "in.jpg", "out.jxl"], "out.jxl", [t("djxl"), "--num_threads=0", "--quiet", "out.jxl", "dec.jpg"], {}),
        "mt": ([t("cjxl"), "--lossless_jpeg=1", "--quiet", "in.jpg", "out.jxl"], "out.jxl", [t("djxl"), "--quiet", "out.jxl", "dec.jpg"], {}),
    },
    "lepton": {
        "1t": (LEP + ["-singlethread", "in.jpg", "out.lep"], "out.lep", LEP + ["-singlethread", "out.lep", "dec.jpg"], {}),
        "mt": (LEP + ["in.jpg", "out.lep"], "out.lep", LEP + ["out.lep", "dec.jpg"], {}),
    },
    "lepton-rust": {
        "1t": ([t("lepton-rust"), "--threads", "1", "--quiet", "--noverify", "in.jpg", "out.lep"], "out.lep", [t("lepton-rust"), "--threads", "1", "--quiet", "out.lep", "dec.jpg"], {}),
        "mt": ([t("lepton-rust"), "--quiet", "--noverify", "in.jpg", "out.lep"], "out.lep", [t("lepton-rust"), "--quiet", "out.lep", "dec.jpg"], {}),
    },
    "brunsli": {
        "1t": ([t("cbrunsli"), "in.jpg", "out.brn"], "out.brn", [t("dbrunsli"), "out.brn", "dec.jpg"], {}),
    },
    "packjpg": {
        # packJPG names its output after its input, so decoding runs in a subdirectory
        "1t": ([t("packJPG"), "-np", "-o", "in.jpg"], "in.pjg", [t("packJPG"), "-np", "-o", "d/in.pjg"], {}),
    },
}
# no threads: the multi-threaded column repeats the single-threaded measurement
SAME = {"brunsli", "packjpg"}
CODECS["brunsli"]["mt"] = CODECS["brunsli"]["1t"]
CODECS["packjpg"]["mt"] = CODECS["packjpg"]["1t"]
# slow codecs run once per file
REPS = {"komla": 1}
ORDER = ["jxl", "brunsli", "packjpg", "lepton", "lepton-rust", "komla", "komla-fast", "komla-strong", "komla-pr2"]
GROUPS = ["kodak_q50", "kodak_q75", "kodak_q90_444", "kodak_q95_prog", "camera", "progressive", "gray", "screenshot"]


def timed(cmd, cwd, env):
    """Runs cmd under /usr/bin/time; returns (wall seconds, peak RSS in KiB)."""
    tf = os.path.join(cwd, ".time")
    t0 = time.perf_counter()
    r = subprocess.run(["/usr/bin/time", "-f", "%M", "-o", tf] + cmd, cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=dict(os.environ, **env))
    dt = time.perf_counter() - t0
    if r.returncode != 0:
        how = f"crashed (signal {-r.returncode})" if r.returncode < 0 else f"failed (exit code {r.returncode})"
        raise RuntimeError(f"{os.path.basename(cmd[0])} {how}: {r.stderr.decode(errors='replace')[-300:]}")
    rss = int(open(tf).read().split()[-1])
    return dt, rss


def run_one(codec, variant, img, reps):
    enc, outname, dec, env = CODECS[codec][variant]
    best_e = best_d = float("inf")
    rss_e = rss_d = 0
    all_e, all_d = [], []
    mode = None
    with tempfile.TemporaryDirectory(dir=SCRATCH) as td:
        shutil.copyfile(img, os.path.join(td, "in.jpg"))
        want = hashlib.sha256(open(img, "rb").read()).digest()
        for _ in range(reps):
            for f in (outname, "dec.jpg"):
                if os.path.exists(os.path.join(td, f)):
                    os.remove(os.path.join(td, f))
            de, re_ = timed(enc, td, env)
            size = os.path.getsize(os.path.join(td, outname))
            if codec.startswith("komla"):
                with open(os.path.join(td, outname), "rb") as fh:
                    mode = fh.read(7)[6]
            if codec == "packjpg":
                os.makedirs(os.path.join(td, "d"), exist_ok=True)
                shutil.move(os.path.join(td, outname), os.path.join(td, "d", "in.pjg"))
            dd, rd = timed(dec, td, env)
            if codec == "packjpg":
                shutil.move(os.path.join(td, "d", "in.jpg"), os.path.join(td, "dec.jpg"))
                shutil.rmtree(os.path.join(td, "d"))
            if hashlib.sha256(open(os.path.join(td, "dec.jpg"), "rb").read()).digest() != want:
                raise RuntimeError("restored file differs from the original")
            best_e, best_d = min(best_e, de), min(best_d, dd)
            rss_e, rss_d = max(rss_e, re_), max(rss_d, rd)
            all_e.append(round(de, 4))
            all_d.append(round(dd, 4))
    r = {"bytes": size, "enc_s": round(best_e, 4), "dec_s": round(best_d, 4), "enc_rss_kb": rss_e, "dec_rss_kb": rss_d, "ok": True, "enc_all": all_e, "dec_all": all_d}
    if mode is not None:
        r["mode"] = mode
    return r


def load(path):
    res = {}
    if os.path.exists(path):
        for line in open(path):
            r = json.loads(line)
            res[(r["codec"], r["threads"], r["image"])] = r
    return res


def summarize(results, title):
    out = [f"{title}: % smaller (failed files at original size), MB of JPEG per second (total input / total time), peak RSS (largest over all files)",
           f"{'codec':<12}{'threads':>8}{'files':>7}{'saved %':>9}{'enc MB/s':>10}{'dec MB/s':>10}{'enc MiB':>9}{'dec MiB':>9}{'fails':>7}"]
    keys = sorted({(r["codec"], r["threads"]) for r in results.values()}, key=lambda k: (ORDER.index(k[0]) if k[0] in ORDER else 99, k[1]))
    for codec, th in keys:
        rs = [r for r in results.values() if r["codec"] == codec and r["threads"] == th]
        ok = [r for r in rs if r["ok"]]
        i = sum(r["input"] for r in rs)
        o = sum(r["bytes"] if r["ok"] else r["input"] for r in rs)
        ib = sum(r["input"] for r in ok)
        es = sum(r["enc_s"] for r in ok)
        ds = sum(r["dec_s"] for r in ok)
        er = max((r["enc_rss_kb"] for r in ok), default=0) / 1024
        dr = max((r["dec_rss_kb"] for r in ok), default=0) / 1024
        out.append(f"{codec:<12}{th:>8}{len(rs):>7}{100 * (1 - o / i):>9.2f}{(ib / es / 1e6 if es else 0):>10.2f}{(ib / ds / 1e6 if ds else 0):>10.2f}{er:>9.1f}{dr:>9.1f}{len(rs) - len(ok):>7}")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", default="dev")
    ap.add_argument("--dir", help="every .jpg under DIR instead of a named set (results in bench/out)")
    ap.add_argument("--codecs", default=",".join(ORDER))
    ap.add_argument("--threads", default="1t,mt")
    ap.add_argument("--reps", type=int, default=3, help="runs per file; the fastest counts")
    ap.add_argument("--force", default="", help="codecs to rerun even if results exist")
    ap.add_argument("--groups", default="", help="only these groups")
    args = ap.parse_args()
    codecs = [c for c in args.codecs.split(",") if c]
    variants = [v for v in args.threads.split(",") if v]
    force = set(c for c in args.force.split(",") if c)
    groups = set(g for g in args.groups.split(",") if g)
    base = args.dir or os.path.join(HERE, "data", "jpeg", args.set)
    imgs = []
    for dp, _, fs in sorted(os.walk(base)):
        for f in sorted(fs):
            g = os.path.basename(dp) if dp != base else "all"
            if f.lower().endswith((".jpg", ".jpeg")) and (not groups or g in groups):
                p = os.path.join(dp, f)
                imgs.append((p, g, os.path.getsize(p)))
    name = os.path.basename(os.path.normpath(args.dir)) if args.dir else args.set
    rpath = os.path.join(HERE, "out" if args.dir else "results", f"jpeg_speed_{name}.jsonl")
    os.makedirs(os.path.dirname(rpath), exist_ok=True)
    results = load(rpath)
    todo = [(c, v, p, g, s) for p, g, s in imgs for c in codecs for v in variants if c in force or (c, v, os.path.relpath(p, HERE)) not in results]
    print(f"{len(imgs)} files, {len(todo)} runs", file=sys.stderr)
    for n, (c, v, p, g, s) in enumerate(todo):
        rel = os.path.relpath(p, HERE)
        r = {"codec": c, "threads": v, "image": rel, "group": g, "input": s}
        try:
            if v == "mt" and c in SAME and (c, "1t", rel) in results:
                r.update({k: x for k, x in results[(c, "1t", rel)].items() if k not in r})
                r["same_as_1t"] = True
            else:
                r.update(run_one(c, v, p, REPS.get(c, args.reps)))
        except Exception as e:
            r.update(bytes=None, enc_s=0, dec_s=0, enc_rss_kb=0, dec_rss_kb=0, ok=False, error=str(e).replace(ROOT + os.sep, "")[:300])
            print(f"FAILED {c} {v} {rel}: {r['error']}", file=sys.stderr)
        results[(c, v, rel)] = r
        with open(rpath, "w") as fo:
            for k in sorted(results):
                fo.write(json.dumps(results[k]) + "\n")
        if (n + 1) % 25 == 0:
            print(f"  {n + 1}/{len(todo)}", file=sys.stderr)
    names = {os.path.relpath(p, HERE) for p, _, _ in imgs}
    sel = {k: v for k, v in results.items() if k[2] in names and k[0] in codecs and k[1] in variants}
    print(summarize(sel, name))


if __name__ == "__main__":
    main()
