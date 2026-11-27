#!/usr/bin/env python3
"""Draws the held-out JPEG set at random from a recorded pool of Wikimedia Commons files and
downloads it to bench/data/jpeg/heldout/<group>/.

  python3 select_heldout.py pool      lists the pool -> datasets/jpeg_heldout_pool.json.gz
  python3 select_heldout.py draw      draws from the pool -> datasets/jpeg_heldout.json
  python3 select_heldout.py fetch     downloads the drawn files (checks SHA-1)

  A further set from the same pool: draw/fetch --name heldout2 --seed 20261128
  --exclude datasets/jpeg_heldout.json (writes datasets/jpeg_heldout2.json, data/jpeg/heldout2).

Pool: for each group, the JPEG files among the first MAX_MEMBERS files (API order) of each of
its categories, with a free licence (CC0, public domain, CC BY, CC BY-SA) and a size in the
group's range, minus every file of the development and test sets (by SHA-1 and by title).

Draw: the pool of each group, sorted by SHA-1, is shuffled with random.Random(f"{SEED}:{group}");
walking that order, a file is kept if no earlier group kept it and it passes the group's check,
read from the first bytes of the file: camera originals need EXIF make and model and three
components, the progressive group needs a progressive frame, the grayscale group one
component; other groups take any JPEG frame type. No file was picked or rejected by hand.
"""
import gzip
import hashlib
import json
import os
import random
import sys
import time
import urllib.parse

from select_jpegs import LICENCE, frame_info, head_bytes, infos, members, sof  # noqa: F401

HERE = os.path.dirname(os.path.abspath(__file__))
SEED = 20261127
POOL = os.path.join(HERE, "datasets", "jpeg_heldout_pool.json.gz")
FIELDS = ["sha1", "bytes", "title", "make", "model", "licence"]
PHOTOS = ["Category:Quality images", "Category:Valued images", "Category:Featured pictures on Wikimedia Commons"]
# (group, files, min bytes, max bytes, categories)
GROUPS = [
    ("camera", 16, 300_000, 15_000_000, [
        "Category:Taken with Canon EOS 5D Mark IV", "Category:Taken with Canon EOS 600D", "Category:Taken with Nikon D750",
        "Category:Taken with Nikon D3200", "Category:Taken with Sony ILCE-7M3", "Category:Taken with Sony DSC-RX100",
        "Category:Taken with Panasonic DMC-FZ200", "Category:Taken with Olympus E-M5 Mark II", "Category:Taken with Fujifilm X-T3",
        "Category:Taken with Pentax K-5", "Category:Taken with Apple iPhone 11", "Category:Taken with Apple iPhone 6",
        "Category:Taken with Samsung Galaxy S9", "Category:Taken with Huawei P20 Pro", "Category:Taken with Google Pixel 3",
        "Category:Taken with Xiaomi Redmi Note 8 Pro"]),
    ("progressive", 10, 100_000, 15_000_000, PHOTOS + ["Category:Photographs by subject", "Category:Images from Flickr"]),
    ("photo", 14, 100_000, 15_000_000, PHOTOS),
    ("gray", 6, 50_000, 10_000_000, ["Category:Black and white photographs", "Category:Monochrome photographs"]),
    ("screenshot", 6, 20_000, 6_000_000, ["Category:Screenshots", "Category:Screenshots of software"]),
    ("scan", 8, 50_000, 15_000_000, ["Category:Old photographs", "Category:Paintings", "Category:Postcards", "Category:Scanned documents", "Category:Maps"]),
    ("web", 6, 10_000, 300_000, PHOTOS + ["Category:Screenshots", "Category:Old photographs", "Category:Paintings", "Category:Postcards"]),
]


def file_url(title):
    """The address of a Commons file (its path holds the MD5 of the name)."""
    name = title[5:].replace(" ", "_")
    m = hashlib.md5(name.encode()).hexdigest()
    return f"https://upload.wikimedia.org/wikipedia/commons/{m[0]}/{m[:2]}/{urllib.parse.quote(name)}"


def entry(row):
    f = dict(zip(FIELDS, row))
    return {"title": f["title"], "url": file_url(f["title"]), "sha1": f["sha1"], "bytes": f["bytes"], "licence": f["licence"],
            "exif": {k: v for k, v in (("Make", f["make"]), ("Model", f["model"])) if v}}


def check(group, f):
    if group == "camera":
        ex = f["exif"]
        if not ex.get("Make") or not ex.get("Model"):
            return False
    s = frame_info(f)
    if s is None:
        return False
    if group == "progressive":
        return s[0] == 0xC2
    if group == "gray":
        return s[1] == 1
    if group == "camera":
        return s[1] == 3
    return True


def pool():
    used = json.load(open(os.path.join(HERE, "datasets", "jpeg_commons.json")))
    used_sha = {f["sha1"] for s in used.values() for f in s}
    used_title = {f["title"] for s in used.values() for f in s}
    out = {"seed": SEED, "created": time.strftime("%Y-%m-%d"), "groups": {}}
    cache = {}
    for group, n, lo, hi, cats in GROUPS:
        titles = sorted({t for c in cats for t in members(c)} - used_title)
        need = [t for t in titles if t not in cache]
        for f in infos(need):
            cache[f["title"]] = f
        cand = [cache[t] for t in titles if t in cache]
        cand = [f for f in cand if f["mime"] == "image/jpeg" and lo <= f["bytes"] <= hi and LICENCE.match(f["licence"]) and f["sha1"] not in used_sha]
        cand.sort(key=lambda f: f["sha1"])
        rows = [[f["sha1"], f["bytes"], f["title"], f["exif"].get("Make", ""), f["exif"].get("Model", ""), f["licence"]] for f in cand]
        out["groups"][group] = {"files": n, "bytes": [lo, hi], "categories": cats, "pool": rows}
        print(f"{group}: {len(titles)} listed, {len(cand)} in the pool", file=sys.stderr)
    out["fields"] = FIELDS
    with open(POOL, "wb") as fo:
        fo.write(gzip.compress(json.dumps(out, separators=(",", ":")).encode(), 9, mtime=0))


def draw(name, seed, exclude):
    p = json.loads(gzip.decompress(open(POOL, "rb").read()))
    seed = seed or p["seed"]
    kept, seen = [], set()
    for path in exclude:
        seen |= {f["sha1"] for f in json.load(open(os.path.join(HERE, path)))["files"]}
    for group, g in p["groups"].items():
        order = [entry(r) for r in g["pool"]]
        random.Random(f"{seed}:{group}").shuffle(order)
        k = 0
        for f in order:
            if k >= g["files"]:
                break
            if f["sha1"] in seen or not check(group, f):
                continue
            seen.add(f["sha1"])
            kept.append(dict(f, group=group))
            k += 1
        print(f"{group}: {k} drawn from {len(order)}", file=sys.stderr)
    out = {"seed": seed, "pool": os.path.basename(POOL), "files": kept}
    if exclude:
        out["excluded"] = exclude
    json.dump(out, open(os.path.join(HERE, "datasets", f"jpeg_{name}.json"), "w"), indent=1)


def fetch(name):
    from fetch_jpeg_datasets import fetch as get
    for f in json.load(open(os.path.join(HERE, "datasets", f"jpeg_{name}.json")))["files"]:
        path = os.path.join(HERE, "data", "jpeg", name, f["group"], f"{f['group']}_{f['sha1'][:10]}.jpg")
        if os.path.exists(path) and hashlib.sha1(open(path, "rb").read()).hexdigest() == f["sha1"]:
            continue
        data = get(f["url"])
        if hashlib.sha1(data).hexdigest() != f["sha1"]:
            sys.exit(f"hash mismatch for {f['url']}")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        open(path, "wb").write(data)
        time.sleep(3)
    print("held-out set ready in", os.path.join(HERE, "data", "jpeg", name), file=sys.stderr)


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("step", choices=["pool", "draw", "fetch"])
    ap.add_argument("--name", default="heldout")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--exclude", action="append", default=[])
    a = ap.parse_args()
    if a.step == "pool":
        pool()
    elif a.step == "draw":
        draw(a.name, a.seed, a.exclude)
    else:
        fetch(a.name)
