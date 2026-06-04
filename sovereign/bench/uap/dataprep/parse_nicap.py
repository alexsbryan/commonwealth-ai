#!/usr/bin/env python3
"""Parse the NICAP "Complete List of Project Blue Book's Unsolved Cases"
into structured JSONL — the demo-hero spine (the ~564 officially-Unknown
cases) and the `is_unidentified` salience signal.

Source: https://www.nicap.org/bluebook/bluelist.htm — a <pre> block of
<br>-separated rows shaped `<case#> <date>  <location>`, where the date
and location are separated by a 2+-space column gap.

Usage: python3 parse_nicap.py [--html <cached.html>] [--out <path>]
Default: fetches the URL (falls back to a cached file), writes
unidentified.jsonl next to this script.
"""
import argparse
import html
import json
import os
import re
import sys
import urllib.request

URL = "https://www.nicap.org/bluebook/bluelist.htm"
HERE = os.path.dirname(os.path.abspath(__file__))


def load_html(cached: str | None) -> str:
    if cached and os.path.exists(cached):
        return open(cached, encoding="latin-1").read()
    req = urllib.request.Request(URL, headers={"User-Agent": "sovereign-uap-dataprep/0.1"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read().decode("latin-1")


def parse_rows(raw: str) -> list[dict]:
    # Isolate the <pre> block.
    m = re.search(r"<pre>(.*?)</pre>", raw, re.DOTALL | re.IGNORECASE)
    body = m.group(1) if m else raw
    # Rows are <br>-separated.
    rows = re.split(r"<br\s*/?>", body, flags=re.IGNORECASE)
    out = []
    for row in rows:
        line = html.unescape(re.sub(r"<[^>]+>", "", row)).strip()
        if not line:
            continue
        # A data row starts with the Blue Book case number (digits).
        m = re.match(r"^(\d+)\s+(.*)$", line)
        if not m:
            continue
        case_no, rest = m.group(1), m.group(2)
        # date and location are separated by a 2+-space column gap.
        parts = re.split(r"\s{2,}", rest, maxsplit=1)
        if len(parts) == 2:
            date, location = parts[0].strip(), parts[1].strip()
        else:
            # No clean gap — best-effort: split at the last comma-bearing tail.
            date, location = rest.strip(), ""
        out.append(
            {
                "case_no": case_no,
                "date": date,
                "location": location,
                "is_unidentified": True,
                "source": "nicap_bluelist",
            }
        )
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--html", default=None, help="cached HTML file (else fetch URL)")
    ap.add_argument("--out", default=os.path.join(HERE, "unidentified.jsonl"))
    args = ap.parse_args()

    raw = load_html(args.html)
    rows = parse_rows(raw)
    with open(args.out, "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")

    # Sanity report.
    n_loc = sum(1 for r in rows if r["location"])
    print(f"parsed {len(rows)} unidentified cases ({n_loc} with a location) → {args.out}")
    if rows:
        print("first:", json.dumps(rows[0]))
        print("last: ", json.dumps(rows[-1]))
    return 0 if 400 <= len(rows) <= 800 else 1


if __name__ == "__main__":
    sys.exit(main())
