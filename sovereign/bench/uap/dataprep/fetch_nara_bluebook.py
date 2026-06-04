#!/usr/bin/env python3
"""Scan NARA's public AWS Open Data bucket (record group 341) for the
per-case Project Blue Book fileUnit records and emit a structured
metadata table — the cold-tail spine + the hero-content pointers.

Source: s3://nara-national-archives-catalog (us-east-2, public, no-auth),
prefix descriptions/record-groups/rg_341/rg_341-N.jsonl. Each line is a
NARA archival description; Blue Book cases sit at levelOfDescription
"fileUnit" under a "Project Blue Book" series, with location+date in the
title, a structured logicalDate, a stable naId, and digitalObjects[]
pointing at public NARA-hosted page JPGs.

Emits metadata.jsonl (one row/case) + prints a summary. Shards are cached
so re-runs are cheap. No AWS CLI / key required — plain HTTPS list+get.
"""
import argparse
import json
import os
import re
import sys
import urllib.request
import urllib.parse
import collections

BUCKET_HOST = "https://nara-national-archives-catalog.s3.us-east-2.amazonaws.com"
PREFIX = "descriptions/record-groups/rg_341/"
HERE = os.path.dirname(os.path.abspath(__file__))
CACHE = os.path.expanduser("~/.sovereign/corpora-staging/uap-nara-shards")


def http_get(url: str, timeout: int = 90) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "sovereign-uap-dataprep/0.1"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def list_shards() -> list[str]:
    """Paginate list-objects-v2 to enumerate every rg_341-*.jsonl key."""
    keys, token = [], None
    while True:
        url = f"{BUCKET_HOST}/?list-type=2&prefix={PREFIX}&max-keys=1000"
        if token:
            url += "&continuation-token=" + urllib.parse.quote(token, safe="")
        xml = http_get(url).decode("utf-8", "replace")
        keys += [k for k in re.findall(r"<Key>([^<]+)</Key>", xml) if k.endswith(".jsonl")]
        m = re.search(r"<NextContinuationToken>([^<]+)</NextContinuationToken>", xml)
        if m and "<IsTruncated>true" in xml:
            token = m.group(1)
        else:
            break
    return sorted(set(keys))


def shard_bytes(key: str) -> bytes:
    os.makedirs(CACHE, exist_ok=True)
    local = os.path.join(CACHE, os.path.basename(key))
    if os.path.exists(local) and os.path.getsize(local) > 0:
        return open(local, "rb").read()
    data = http_get(f"{BUCKET_HOST}/{key}")
    with open(local, "wb") as f:
        f.write(data)
    return data


def first_date(rec: dict) -> str | None:
    for field in ("coverageStartDate", "inclusiveStartDate"):
        v = rec.get(field)
        if isinstance(v, dict) and v.get("logicalDate"):
            return v["logicalDate"]
    return None


def bluebook_series(rec: dict) -> str | None:
    """Return the Blue Book series title from the ancestors, else None."""
    for a in rec.get("ancestors", []) or []:
        if a.get("levelOfDescription") == "series":
            t = a.get("title", "") or ""
            if "blue book" in t.lower():
                return t
    return None


def image_urls(rec: dict) -> list[str]:
    out = []
    for d in rec.get("digitalObjects", []) or []:
        u = d.get("objectUrl")
        if u and (d.get("objectType", "").lower().startswith("image") or u.lower().endswith((".jpg", ".jpeg", ".png"))):
            out.append(u)
    return out


def parse_location(title: str) -> str:
    """Best-effort: the title is '<location>, <month?> <year>'. Strip a
    trailing date-ish tail (month/year/bracketed-illegible)."""
    t = title.strip()
    # drop a trailing ", <Month> <Year>" or ", <Year>" or ", [ILLEGIBLE] <Year>"
    t = re.sub(
        r",\s*(\[[^\]]*\]\s*)?([A-Z][a-z]+\.?\s*)?(\d{1,2}[-,]?\s*)?\d{4}\s*$",
        "",
        t,
    )
    return t.strip().rstrip(",").strip()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=os.path.join(HERE, "metadata.jsonl"))
    ap.add_argument("--max-shards", type=int, default=0, help="0 = all")
    args = ap.parse_args()

    shards = list_shards()
    if args.max_shards:
        shards = shards[: args.max_shards]
    print(f"rg_341 shards: {len(shards)}", file=sys.stderr)

    rows = []
    series_hist = collections.Counter()
    level_under_bb = collections.Counter()
    for i, key in enumerate(shards):
        data = shard_bytes(key)
        for line in data.decode("utf-8", "replace").splitlines():
            line = line.strip()
            if not line or "blue book" not in line.lower():
                continue
            try:
                rec = json.loads(line)["record"]
            except Exception:
                continue
            series = bluebook_series(rec)
            if not series:
                continue
            level_under_bb[rec.get("levelOfDescription")] += 1
            if rec.get("levelOfDescription") != "fileUnit":
                continue  # cases are fileUnits; series/item levels aren't cases
            series_hist[series] += 1
            imgs = image_urls(rec)
            title = rec.get("title", "") or ""
            rows.append(
                {
                    "naId": rec.get("naId"),
                    "title": title,
                    "location": parse_location(title),
                    "date": first_date(rec),
                    "series": series,
                    "n_images": len(imgs),
                    "image_urls": imgs,
                    "source": "nara_aws_odr_rg341",
                }
            )
        if (i + 1) % 25 == 0:
            print(f"  ...{i+1}/{len(shards)} shards, {len(rows)} cases so far", file=sys.stderr)

    with open(args.out, "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")

    # Summary.
    years = collections.Counter()
    with_imgs = sum(1 for r in rows if r["n_images"] > 0)
    tot_imgs = sum(r["n_images"] for r in rows)
    for r in rows:
        if r["date"]:
            years[r["date"][:4]] += 1
    yk = sorted(years)
    print(f"\nBlue Book per-case fileUnits: {len(rows)} → {args.out}")
    print(f"  with >=1 page image: {with_imgs}  (total page images: {tot_imgs})")
    print(f"  date span: {yk[0] if yk else '?'} … {yk[-1] if yk else '?'}")
    print("  levels seen under Blue Book series:", dict(level_under_bb))
    print("  top series:")
    for s, n in series_hist.most_common(6):
        print(f"    {n:6d}  {s[:70]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
