#!/usr/bin/env python3
"""Builds the JPEG benchmark sets in bench/data/jpeg/{test,dev}/<group>/ and verifies hashes.

  kodak_q50, kodak_q75, kodak_q90_444, kodak_q95_prog
      the 24 Kodak images encoded with libjpeg-turbo 3.2.1 cjpeg (bench/install-jpeg-baselines.sh):
      -quality 50 | -quality 75 -optimize | -quality 90 -sample 1x1 -optimize | -quality 95 -progressive
      odd image numbers -> dev, even -> test
  camera, progressive, gray, screenshot
      Wikimedia Commons files listed in datasets/jpeg_commons.json (chosen by select_jpegs.py)

The SHA-256 of every generated Kodak JPEG is recorded in datasets/jpeg_kodak.json; a different
cjpeg build that produces other bytes is reported.
"""
import hashlib
import io
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "data", "jpeg")
TOOLS = os.environ.get("TOOLS", os.path.join(HERE, ".tools", "bin"))
UA = {"User-Agent": "komla-benchmark/1.0 (https://github.com/paatai/Compress; lossless JPEG recompression benchmark)"}
KODAK_URL = "http://r0k.us/graphics/kodak/kodak/{name}"
VARIANTS = {
    "kodak_q50": ["-quality", "50"],
    "kodak_q75": ["-quality", "75", "-optimize"],
    "kodak_q90_444": ["-quality", "90", "-sample", "1x1", "-optimize"],
    "kodak_q95_prog": ["-quality", "95", "-progressive"],
}


def fetch(url):
    for attempt in range(10):
        try:
            return urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=300).read()
        except urllib.error.HTTPError as e:
            if attempt == 9 or e.code not in (429, 500, 502, 503, 504):
                raise
            time.sleep(max(int(e.headers.get("Retry-After") or 0), 30 * 2 ** min(attempt, 4)))
        except Exception:
            if attempt == 9:
                raise
            time.sleep(10 * 2 ** min(attempt, 4))


def digest(data, algo):
    return hashlib.new(algo, data).hexdigest()


def main():
    from PIL import Image

    photos = json.load(open(os.path.join(HERE, "datasets", "photos.json")))
    kpath = os.path.join(HERE, "datasets", "jpeg_kodak.json")
    known = json.load(open(kpath)) if os.path.exists(kpath) else {}
    recorded = dict(known)
    cjpeg = os.path.join(TOOLS, "cjpeg")
    for f in photos["kodak"]:
        num = int(f["name"][5:7])
        split = "dev" if num % 2 else "test"
        png = None
        for group, flags in VARIANTS.items():
            out = os.path.join(DATA, split, group, f["name"].replace(".png", ".jpg"))
            key = f"{group}/{os.path.basename(out)}"
            if os.path.exists(out) and key in known and digest(open(out, "rb").read(), "sha256") == known[key]:
                continue
            if png is None:
                png = fetch(KODAK_URL.format(name=f["name"]))
                if digest(png, "sha256") != f["sha256"]:
                    sys.exit(f"hash mismatch for {f['name']}")
            with tempfile.TemporaryDirectory() as td:
                ppm = os.path.join(td, "in.ppm")
                Image.open(io.BytesIO(png)).convert("RGB").save(ppm)
                jpg = subprocess.run([cjpeg, *flags, ppm], stdout=subprocess.PIPE, check=True).stdout
            h = digest(jpg, "sha256")
            if key in known and known[key] != h:
                print(f"warning: {key} differs from the recorded file (different cjpeg build?)", file=sys.stderr)
            recorded[key] = h
            os.makedirs(os.path.dirname(out), exist_ok=True)
            open(out, "wb").write(jpg)
    if recorded != known:
        json.dump(dict(sorted(recorded.items())), open(kpath, "w"), indent=1)

    commons = json.load(open(os.path.join(HERE, "datasets", "jpeg_commons.json")))
    for split in ("test", "dev"):
        for f in commons[split]:
            p = os.path.join(DATA, split, f["group"], f"{f['group']}_{f['sha1'][:10]}.jpg")
            if os.path.exists(p) and digest(open(p, "rb").read(), "sha1") == f["sha1"]:
                continue
            data = fetch(f["url"])
            if digest(data, "sha1") != f["sha1"]:
                sys.exit(f"hash mismatch for {f['url']}")
            os.makedirs(os.path.dirname(p), exist_ok=True)
            open(p, "wb").write(data)
            time.sleep(3)
    print("JPEG datasets ready in", DATA, file=sys.stderr)


if __name__ == "__main__":
    main()
