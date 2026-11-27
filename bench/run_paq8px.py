#!/usr/bin/env python3
"""Size, time and peak memory of paq8px (single-threaded, very slow) on a JPEG set, several
files at a time; results in bench/results/jpeg_paq8px_<set>.jsonl.

paq8px takes hours per set, so it cannot join the interleaved timing of run_jpeg_speed.py; it
runs `--jobs` files at a time, and its times are the processor time (user + system) of each
process, which other work and pauses do not inflate. With `--shuffle SEED` the files run in a
random order, so that the files done when it is stopped are a random sample of the set. Every
output is decompressed and compared with the original byte for byte (SHA-256).

usage: run_paq8px.py --set dev|test|heldout [--jobs N] [--shuffle SEED] [--skip-known results.jsonl]
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.environ.get("TOOLS", os.path.join(HERE, ".tools", "bin"))
PAQ = os.path.join(TOOLS, "paq8px")


def timed(cmd, cwd):
    """Runs cmd under /usr/bin/time; returns (processor seconds, peak RSS in KiB)."""
    tf = os.path.join(cwd, ".time")
    r = subprocess.run(["/usr/bin/time", "-f", "%M %U %S", "-o", tf] + cmd, cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if r.returncode != 0:
        how = f"crashed (signal {-r.returncode})" if r.returncode < 0 else f"failed (exit code {r.returncode})"
        raise RuntimeError(f"paq8px {how}: {r.stderr.decode(errors='replace')[-300:]}")
    rss, user, sys_ = open(tf).read().split()[-3:]
    return float(user) + float(sys_), int(rss)


def one(img):
    with tempfile.TemporaryDirectory() as td:
        shutil.copyfile(img, os.path.join(td, "in.jpg"))
        want = hashlib.sha256(open(img, "rb").read()).digest()
        try:
            de, re_ = timed([PAQ, "-8", "in.jpg", "out.paq8px217"], td)
            size = os.path.getsize(os.path.join(td, "out.paq8px217"))
            os.makedirs(os.path.join(td, "d"))
            dd, rd = timed([PAQ, "-d", "out.paq8px217", "d/x.jpg"], td)
            if hashlib.sha256(open(os.path.join(td, "d", "x.jpg"), "rb").read()).digest() != want:
                raise RuntimeError("restored file differs from the original")
            return {"bytes": size, "enc_s": round(de, 3), "dec_s": round(dd, 3), "enc_rss_kb": re_, "dec_rss_kb": rd, "ok": True, "cpu_time": True}
        except Exception as e:
            return {"bytes": None, "enc_s": 0, "dec_s": 0, "enc_rss_kb": 0, "dec_rss_kb": 0, "ok": False, "error": str(e)[:300]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", default="dev")
    ap.add_argument("--jobs", type=int, default=3)
    ap.add_argument("--skip-known", default="", help="a jsonl file whose paq8px rows are reused")
    ap.add_argument("--shuffle", type=int, default=0, help="run the files in a random order with this seed")
    args = ap.parse_args()
    base = os.path.join(HERE, "data", "jpeg", args.set)
    rpath = os.path.join(HERE, "results", f"jpeg_paq8px_{args.set}.jsonl")
    done = {}
    if os.path.exists(rpath):
        for line in open(rpath):
            r = json.loads(line)
            done[r["image"]] = r
    if args.skip_known:
        for line in open(args.skip_known):
            r = json.loads(line)
            if r.get("codec") == "paq8px" and r["image"] not in done:
                done[r["image"]] = dict(r, reused=os.path.basename(args.skip_known))
    imgs = []
    for dp, _, fs in sorted(os.walk(base)):
        for f in sorted(fs):
            if f.lower().endswith((".jpg", ".jpeg")):
                p = os.path.join(dp, f)
                rel = os.path.relpath(p, HERE)
                if rel not in done:
                    imgs.append((p, rel, os.path.basename(dp)))
    if args.shuffle:
        random.Random(args.shuffle).shuffle(imgs)
    print(f"{len(imgs)} files to run, {len(done)} known", file=sys.stderr)

    def save():
        with open(rpath, "w") as fo:
            for k in sorted(done):
                fo.write(json.dumps(done[k]) + "\n")

    save()
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as ex:
        futs = {ex.submit(one, p): (p, rel, g) for p, rel, g in imgs}
        for fu in concurrent.futures.as_completed(futs):
            p, rel, g = futs[fu]
            r = {"codec": "paq8px", "image": rel, "group": g, "input": os.path.getsize(p), "jobs": args.jobs}
            r.update(fu.result())
            done[rel] = r
            save()
            if not r["ok"]:
                print(f"FAILED paq8px {rel}: {r['error']}", file=sys.stderr)
    print("done", file=sys.stderr)


if __name__ == "__main__":
    main()
