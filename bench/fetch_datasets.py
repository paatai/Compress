#!/usr/bin/env python3
"""Downloads the benchmark images into bench/data/{test,dev}/<category>/ and verifies hashes.

  test/photo_kodak     Kodak Lossless True Color Image Suite (24 images, 768x512)
  test/photo_div2k     DIV2K validation images 0801-0810 (about 2040x1356)
  test/screenshot      12 Wikimedia Commons screenshots (see commons_synthetic.json)
  test/graphics        12 Wikimedia Commons charts, diagrams and maps
  dev/...              disjoint images used for tuning: DIV2K 0861-0870 and 24 Commons images
"""
import hashlib
import io
import json
import os
import sys
import time
import urllib.request
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "data")
UA = {"User-Agent": "komla-benchmark/1.0 (lossless image compression benchmark)"}
KODAK_URL = "http://r0k.us/graphics/kodak/kodak/{name}"
DIV2K_URL = "http://data.vision.ee.ethz.ch/cvl/DIV2K/DIV2K_valid_HR.zip"


def fetch(url):
    for attempt in range(6):
        try:
            req = urllib.request.Request(url, headers=UA)
            return urllib.request.urlopen(req, timeout=300).read()
        except Exception:
            if attempt == 5:
                raise
            time.sleep(4 * 2 ** attempt)


def save(path, data, digest, algo):
    h = hashlib.new(algo, data).hexdigest()
    if h != digest:
        sys.exit(f"hash mismatch for {path}: {h} != {digest}")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)


def have(path, digest, algo):
    return os.path.exists(path) and hashlib.new(algo, open(path, "rb").read()).hexdigest() == digest


def main():
    photos = json.load(open(os.path.join(HERE, "datasets", "photos.json")))
    synth = json.load(open(os.path.join(HERE, "datasets", "commons_synthetic.json")))

    for f in photos["kodak"]:
        p = os.path.join(DATA, "test", "photo_kodak", f["name"])
        if not have(p, f["sha256"], "sha256"):
            save(p, fetch(KODAK_URL.format(name=f["name"])), f["sha256"], "sha256")

    div = [("test", f) for f in photos["div2k_test"]] + [("dev", f) for f in photos["div2k_dev"]]
    missing = [(s, f) for s, f in div if not have(os.path.join(DATA, s, "photo_div2k", f["name"]), f["sha256"], "sha256")]
    if missing:
        print("downloading DIV2K validation set (about 450 MB)...", file=sys.stderr)
        z = zipfile.ZipFile(io.BytesIO(fetch(DIV2K_URL)))
        for s, f in missing:
            save(os.path.join(DATA, s, "photo_div2k", f["name"]), z.read("DIV2K_valid_HR/" + f["name"]), f["sha256"], "sha256")

    for s in ("test", "dev"):
        for f in synth[s]:
            p = os.path.join(DATA, s, f["group"], f"{f['group']}_{f['sha1'][:10]}.png")
            if not have(p, f["sha1"], "sha1"):
                save(p, fetch(f["url"]), f["sha1"], "sha1")
                time.sleep(0.5)
    print("datasets ready in", DATA, file=sys.stderr)


if __name__ == "__main__":
    main()
