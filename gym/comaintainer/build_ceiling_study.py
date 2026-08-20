#!/usr/bin/env python3
"""Build the blind rating instrument for the reliability-ceiling study.

    python3 gym/comaintainer/build_ceiling_study.py \
        runs/<frontier-run> runs/<local-run> --out /tmp/ceiling

Regenerates everything derived, so nothing derived is committed: the item
set, the sealed answer key, and a self-contained rating page. Design and
decision rules: CEILING_STUDY_PREREG.md.

Two structural properties, checked here rather than asserted:
  1. the page never contains the answer key (verified by search, not by
     "I didn't add it");
  2. every item passes markers.lint_leaks over exactly the text the rater
     will see.
Either check failing is a hard exit — a blind that might not be blind is
worth nothing.
"""
from __future__ import annotations

import argparse
import gzip
import json
import random
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import markers as M                    # noqa: E402
from score import extract_verdict      # noqa: E402

SEED = 20260818          # recorded with the data; changing it changes the study


def load_run(d: Path) -> dict[str, str]:
    out: dict[str, str] = {}
    for line in (d / "rows.jsonl").read_text().splitlines():
        if not line.strip():
            continue
        r = json.loads(line)
        v, _ = extract_verdict(r.get("raw") or "")
        if v and v.get("verdict"):
            out[r["id"]] = v["verdict"]
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("run_a", help="frontier run dir")
    ap.add_argument("run_b", help="local run dir")
    ap.add_argument("--out", default="/tmp/ceiling")
    ap.add_argument("--bank", default=str(HERE / "cases.jsonl.gz"))
    ap.add_argument("--template", default=str(HERE / "ceiling_rate.html"))
    a = ap.parse_args()

    A, B = load_run(Path(a.run_a)), load_run(Path(a.run_b))
    bank = {}
    with gzip.open(a.bank, "rt") as fh:
        for line in fh:
            e = json.loads(line)
            bank[e["id"]] = e

    ids = [i for i in sorted(set(A) & set(B))
           if i in bank and bank[i]["tier"] == "A"
           and bank[i].get("scope") != "situated"]
    if not ids:
        sys.exit("no shared tier-A episodes between these runs")

    order = ids[:]
    random.Random(SEED).shuffle(order)
    items = [{"id": i, "n": k + 1,
              "situation": bank[i]["request"]["situation"],
              "proposal": bank[i]["request"]["proposal"],
              "evidence": bank[i]["request"].get("evidence") or "[none provided]"}
             for k, i in enumerate(order)]

    # GUARD 1 — the bank's own leak linter, over exactly what the rater sees.
    leaky = []
    for it in items:
        ep = {"id": it["id"],
              "request": {k: it[k] for k in ("situation", "proposal", "evidence")},
              "expect": bank[it["id"]]["expect"]}
        if M.lint_leaks(ep):
            leaky.append((it["id"], M.lint_leaks(ep)))
    if leaky:
        sys.exit(f"REFUSING: {len(leaky)} items leak their answer: {leaky[:3]}")

    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "study_items.json").write_text(json.dumps({"items": items, "seed": SEED}))
    key = {i: {"gold": bank[i]["expect"]["verdict"], "frontier": A[i], "local": B[i],
               "source": bank[i]["source"], "tier": bank[i]["tier"]} for i in ids}
    (out / "study_key.json").write_text(json.dumps(key, indent=1))

    html = (Path(a.template).read_text()
            .replace("__ITEMS_JSON__", json.dumps(items))
            .replace("__SEED__", str(SEED)))
    (out / "rate.html").write_text(html)

    # GUARD 2 — the page must not carry the answers. Searched, not assumed.
    if re.search(r'"(gold|frontier|local)"\s*:', html) or "study_key" in html:
        sys.exit("REFUSING: the rating page contains answer-key material")

    # Items whose own visible text echoes their gold verdict as prose: not a
    # leak the linter catches, but a pre-specified sensitivity analysis.
    echo = [i for i in ids
            if key[i]["gold"] in (bank[i]["request"]["situation"] + " "
                                  + bank[i]["request"]["proposal"] + " "
                                  + (bank[i]["request"].get("evidence") or "")).lower()]

    print(f"built {out}/rate.html — {len(items)} paired tier-A episodes, seed {SEED}")
    print(f"  leak-lint: clean over all {len(items)} items")
    print(f"  answer key in page: no (searched)")
    print(f"  prose-echo items (sensitivity, not dropped): {len(echo)} {echo}")
    print(f"\n  open {out}/rate.html")
    print(f"  then: python3 {HERE.name}/analyze_ceiling.py <ratings.jsonl> "
          f"--key {out}/study_key.json")


if __name__ == "__main__":
    main()
