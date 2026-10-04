#!/usr/bin/env python3
"""Stamp `is_unidentified` on the NARA Blue Book cases by joining the
NICAP unidentified list (the AF's own ~558 "Unknown" rulings) to the
NARA per-case fileUnits, matched on normalized location + year.

Inputs:  unidentified.jsonl (NICAP), metadata.jsonl (NARA, in place)
Output:  rewrites metadata.jsonl with `is_unidentified` (+ `nicap_case_no`
         when matched) and prints the match rate.
"""
import json
import os
import re
import sys
import collections

HERE = os.path.dirname(os.path.abspath(__file__))


def norm_loc(s: str) -> str:
    s = (s or "").lower()
    s = re.sub(r"[^a-z0-9]+", " ", s)
    return " ".join(s.split())


def year_of(s: str) -> str | None:
    m = re.search(r"\b(19\d{2})\b", s or "")
    return m.group(1) if m else None


def main() -> int:
    nicap = [json.loads(l) for l in open(os.path.join(HERE, "unidentified.jsonl")) if l.strip()]
    nara = [json.loads(l) for l in open(os.path.join(HERE, "metadata.jsonl")) if l.strip()]

    # NICAP index: (norm_location, year) -> case_no. Locations are the
    # whole "City, State" string; fall back to the first token (city) too.
    idx = {}
    for u in nicap:
        y = year_of(u.get("date", ""))
        loc = norm_loc(u.get("location", ""))
        if y and loc:
            idx.setdefault((loc, y), u["case_no"])
            # also key on city-only (first comma segment) for state/typo drift
            city = norm_loc(u["location"].split(",")[0])
            idx.setdefault((city, y), u["case_no"])

    matched_nara = 0
    matched_nicap = set()
    for r in nara:
        y = (r.get("date") or "")[:4]
        loc = norm_loc(r.get("location", ""))
        city = norm_loc((r.get("location") or "").split(",")[0])
        hit = idx.get((loc, y)) or idx.get((city, y))
        if hit:
            r["is_unidentified"] = True
            r["nicap_case_no"] = hit
            matched_nara += 1
            matched_nicap.add(hit)
        else:
            r["is_unidentified"] = False

    with open(os.path.join(HERE, "metadata.jsonl"), "w") as f:
        for r in nara:
            f.write(json.dumps(r) + "\n")

    n_unid_with_imgs = sum(1 for r in nara if r["is_unidentified"] and r["n_images"] > 0)
    print(f"NARA cases: {len(nara)}  |  NICAP unidentified: {len(nicap)}")
    print(f"matched (NARA cases flagged unidentified): {matched_nara}")
    print(f"distinct NICAP cases matched: {len(matched_nicap)} / {len(nicap)} "
          f"({100*len(matched_nicap)//max(1,len(nicap))}%)")
    print(f"unidentified cases WITH page images (the OCR hero set): {n_unid_with_imgs}")
    # A few unmatched NICAP examples to gauge the gap.
    unmatched = [u for u in nicap if u["case_no"] not in matched_nicap][:5]
    if unmatched:
        print("sample unmatched NICAP:", [f"{u['location']} {year_of(u['date'])}" for u in unmatched])
    return 0


if __name__ == "__main__":
    sys.exit(main())
