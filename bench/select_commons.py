#!/usr/bin/env python3
"""Deterministically selects synthetic test images (screenshots, charts, diagrams, maps)
from Wikimedia Commons and writes a manifest with URLs and SHA-1 hashes.

Selection rule (no manual picking): take every PNG in the listed categories whose size is
0.25-4 megapixels and at most 12 MB, sort by the SHA-1 of the file, and take the first
2*N per group. Even positions go to the test set, odd positions to the dev (tuning) set.
A short SHA-1 exclusion list removes files whose titles name commercial products.
"""
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

UA = {"User-Agent": "kolmo-benchmark/1.0 (lossless image compression benchmark)"}
GROUPS = {
    "screenshot": ["Category:Screenshots of Linux software", "Category:Screenshots of web browsers"],
    "graphics": ["Category:Charts", "Category:PNG diagrams", "Category:Maps of the world"],
}
N_PER_SET = 12
# Files whose titles contain product names that must not appear in the published results.
EXCLUDE_SHA1 = {"0151d61a5ddafb450589943ffe35c4e3867b0975"}


def api(params):
    params = dict(params, format="json")
    data = urllib.parse.urlencode(params).encode()
    for attempt in range(8):
        try:
            time.sleep(1.0)
            req = urllib.request.Request("https://commons.wikimedia.org/w/api.php", data=data, headers=UA)
            return json.load(urllib.request.urlopen(req, timeout=60))
        except urllib.error.HTTPError as e:
            if e.code != 429 or attempt == 7:
                raise
            time.sleep(5 * 2 ** attempt)


def members(cat):
    out, cont = [], {}
    while True:
        r = api({"action": "query", "list": "categorymembers", "cmtitle": cat, "cmtype": "file", "cmlimit": "500", **cont})
        out += [m["title"] for m in r["query"]["categorymembers"] if m["title"].lower().endswith(".png")]
        if "continue" not in r:
            return out
        cont = {"cmcontinue": r["continue"]["cmcontinue"]}


def infos(titles):
    res = []
    for i in range(0, len(titles), 50):
        r = api({"action": "query", "prop": "imageinfo", "iiprop": "url|size|sha1|mime", "titles": "|".join(titles[i:i + 50])})
        for page in r["query"]["pages"].values():
            if "imageinfo" not in page:
                continue
            ii = page["imageinfo"][0]
            res.append({"title": page["title"], "url": ii["url"], "page": ii["descriptionurl"], "sha1": ii["sha1"],
                        "width": ii["width"], "height": ii["height"], "bytes": ii["size"], "mime": ii["mime"]})
    return res


def main():
    manifest = {"test": [], "dev": []}
    for group, cats in GROUPS.items():
        titles = sorted({t for c in cats for t in members(c)})
        cand = [f for f in infos(titles) if f["mime"] == "image/png" and 250_000 <= f["width"] * f["height"] <= 4_000_000 and f["bytes"] <= 12_000_000 and f["sha1"] not in EXCLUDE_SHA1]
        cand.sort(key=lambda f: f["sha1"])
        for k, f in enumerate(cand[:2 * N_PER_SET]):
            f["group"] = group
            manifest["test" if k % 2 == 0 else "dev"].append(f)
        print(f"{group}: {len(titles)} PNGs, {len(cand)} eligible", file=sys.stderr)
    json.dump(manifest, sys.stdout, indent=1)


if __name__ == "__main__":
    main()
