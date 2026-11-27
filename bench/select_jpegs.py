#!/usr/bin/env python3
"""Deterministically selects the real-world JPEGs of the JPEG benchmark from Wikimedia Commons
and writes a manifest (URL, SHA-1, size, licence) to stdout.

Selection rule (no manual picking): for each group, list the first MAX_MEMBERS files of each
category (API order), keep JPEGs within the group's size range whose licence is CC0, public
domain, CC BY or CC BY-SA, sort them by the SHA-1 of the file, and walk that list keeping the
files that pass the group's check (read from the first bytes of the file): camera originals
(EXIF camera make/model, no editing software), progressive JPEGs, grayscale (one component)
JPEGs, or any JPEG for screenshots. Kept files alternate between the test set (even positions)
and the development set (odd positions) until each has N files. A file is used at most once.
"""
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

UA = {"User-Agent": "komla-benchmark/1.0 (https://github.com/paatai/Compress; lossless JPEG recompression benchmark)"}
MAX_MEMBERS = 500
GROUPS = [
    ("camera", 12, 1_000_000, 8_000_000, [
        "Category:Taken with Canon EOS 80D", "Category:Taken with Sony ILCE-6000", "Category:Taken with Nikon D7100",
        "Category:Taken with Fujifilm X-T20", "Category:Taken with Olympus E-M10", "Category:Taken with Google Pixel 4a",
        "Category:Taken with Google Pixel 7", "Category:Taken with Xiaomi 13"]),
    ("progressive", 8, 150_000, 6_000_000, ["Category:Quality images", "Category:Featured pictures on Wikimedia Commons"]),
    ("gray", 6, 100_000, 6_000_000, ["Category:Black and white photographs"]),
    ("screenshot", 6, 40_000, 4_000_000, ["Category:Screenshots"]),
]
LICENCE = re.compile(r"^(CC0|Public domain|PD|CC BY(-SA)? [0-9.]+( [A-Za-z]+)?)", re.I)
EDITORS = re.compile(r"photoshop|lightroom|gimp|capture one|darktable|rawtherapee|dxo|luminar|affinity|paint\.net|acdsee|picasa|snapseed|photos|imagemagick|digikam|shotwell|faststone|irfanview|silkypix|aftershot", re.I)


def api(params):
    data = urllib.parse.urlencode(dict(params, format="json")).encode()
    for attempt in range(8):
        try:
            time.sleep(1.0)
            req = urllib.request.Request("https://commons.wikimedia.org/w/api.php", data=data, headers=UA)
            return json.load(urllib.request.urlopen(req, timeout=60))
        except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError):
            if attempt == 7:
                raise
            time.sleep(5 * 2 ** attempt)


def members(cat):
    out, cont = [], {}
    while len(out) < MAX_MEMBERS:
        r = api({"action": "query", "list": "categorymembers", "cmtitle": cat, "cmtype": "file", "cmlimit": "500", **cont})
        out += [m["title"] for m in r["query"]["categorymembers"] if m["title"].lower().endswith((".jpg", ".jpeg"))]
        if "continue" not in r:
            break
        cont = {"cmcontinue": r["continue"]["cmcontinue"]}
    return out[:MAX_MEMBERS]


def infos(titles):
    res = []
    for i in range(0, len(titles), 50):
        r = api({"action": "query", "prop": "imageinfo", "titles": "|".join(titles[i:i + 50]),
                 "iiprop": "url|size|sha1|mime|extmetadata|metadata", "iiextmetadatafilter": "LicenseShortName"})
        for page in r["query"]["pages"].values():
            if "imageinfo" not in page:
                continue
            ii = page["imageinfo"][0]
            md = {m["name"]: str(m["value"]) for m in (ii.get("metadata") or []) if isinstance(m, dict) and m.get("name") in ("Make", "Model", "Software")}
            res.append({"title": page["title"], "url": ii["url"], "page": ii["descriptionurl"], "sha1": ii["sha1"], "width": ii["width"],
                        "height": ii["height"], "bytes": ii["size"], "mime": ii["mime"],
                        "licence": ii.get("extmetadata", {}).get("LicenseShortName", {}).get("value", ""), "exif": md})
    return res


CACHE = os.environ.get("SELECT_CACHE", os.path.join(os.path.dirname(os.path.abspath(__file__)), ".select_cache.json"))
_cache = json.load(open(CACHE)) if os.path.exists(CACHE) else {}


def head_bytes(url, n=262144):
    for attempt in range(10):
        try:
            time.sleep(2.0)
            req = urllib.request.Request(url, headers=dict(UA, Range=f"bytes=0-{n - 1}"))
            return urllib.request.urlopen(req, timeout=120).read()
        except urllib.error.HTTPError as e:
            if attempt == 9 or e.code not in (429, 500, 502, 503, 504):
                raise
            time.sleep(max(int(e.headers.get("Retry-After") or 0), 30 * 2 ** min(attempt, 4)))
        except (urllib.error.URLError, TimeoutError):
            if attempt == 9:
                raise
            time.sleep(10 * 2 ** min(attempt, 4))


def frame_info(f):
    """(SOF marker, components) from the start of the file, cached by SHA-1."""
    if f["sha1"] not in _cache:
        s = sof(head_bytes(f["url"], 65536))
        _cache[f["sha1"]] = s if s is not None else sof(head_bytes(f["url"]))
        json.dump(_cache, open(CACHE, "w"))
    return _cache[f["sha1"]]


def sof(b):
    """(marker, components) of the first frame header, or None."""
    if b[:2] != b"\xff\xd8":
        return None
    i = 2
    while i + 4 <= len(b):
        if b[i] != 0xFF:
            return None
        m = b[i + 1]
        if m == 0xFF:
            i += 1
            continue
        ln = (b[i + 2] << 8) | b[i + 3]
        if 0xC0 <= m <= 0xCF and m not in (0xC4, 0xC8, 0xCC):
            return (m, b[i + 9]) if i + 10 <= len(b) else None
        if m == 0xDA:
            return None
        i += 2 + ln
    return None


def check(group, f):
    if group == "camera":
        ex = f["exif"]
        if not ex.get("Make") or not ex.get("Model") or EDITORS.search(ex.get("Software", "")):
            return False
    s = frame_info(f)
    if s is None or s[0] not in (0xC0, 0xC1, 0xC2):
        return False
    if group == "progressive":
        return s[0] == 0xC2
    if group == "gray":
        return s[1] == 1
    if group == "camera":
        return s[1] == 3
    return True


def main():
    manifest = {"test": [], "dev": []}
    seen = set()
    for group, n, lo, hi, cats in GROUPS:
        titles = sorted({t for c in cats for t in members(c)})
        cand = [f for f in infos(titles) if f["mime"] == "image/jpeg" and lo <= f["bytes"] <= hi and LICENCE.match(f["licence"])]
        cand.sort(key=lambda f: f["sha1"])
        kept = 0
        for f in cand:
            if kept >= 2 * n:
                break
            if f["sha1"] in seen or not check(group, f):
                continue
            seen.add(f["sha1"])
            f["group"] = group
            manifest["test" if kept % 2 == 0 else "dev"].append(f)
            kept += 1
        print(f"{group}: {len(titles)} listed, {len(cand)} eligible, {kept} kept", file=sys.stderr)
    json.dump(manifest, sys.stdout, indent=1)


if __name__ == "__main__":
    main()
