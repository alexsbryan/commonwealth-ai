#!/usr/bin/env python3
"""Score the index-aligned bank (gym/next-edit/aligned/README.md — PRE-REGISTERED).

Uses the GOLDEN set's ruler verbatim (`score_golden.py::score_positive`,
`site_precision`) so A1's comparison is like-for-like and there is one decider,
not two. A harvest case's `expect.sites` is already the golden `truth` shape.

A3 asks the one SCIP question this construction can answer honestly: of the
JUNK the lane proposes, how much sits where SCIP records no occurrence at all
(a comment or a string)? A referent filter drops that for free. It does not
depend on the rename, so it is not circular — unlike scoring a SCIP filter
against the author's edits, which this bank deliberately does not attempt.
"""
from __future__ import annotations

import argparse
import collections
import importlib.util
import json
import os
import sqlite3
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
_s = importlib.util.spec_from_file_location("sg", HERE.parent / "golden" / "score_golden.py")
SG = importlib.util.module_from_spec(_s)
_s.loader.exec_module(SG)


def line_groups(case: dict, edits: list[dict]) -> list[tuple[int, list[dict]]]:
    """Proposed edits grouped by the text line they land on — the same unit
    `site_precision` judges."""
    text = case["request"]["text"]
    idx = SG._u16_to_str_index(text)
    out: dict[int, list[dict]] = collections.defaultdict(list)
    for e in edits:
        s = idx.get(e["start"])
        if s is None:
            continue
        out[text.count("\n", 0, s)].append(e)
    return sorted(out.items())


def scip_covered(con, path: str, line: int, lo: int, hi: int) -> bool:
    rows = con.execute(
        "select start_col, end_col from refs where file_path=? and line=? and line=end_line",
        (path, line)).fetchall()
    return any(sc < hi and lo < ec for sc, ec in rows)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", default=str(HERE / "cases.jsonl"))
    ap.add_argument("--endpoint", default="http://127.0.0.1:9741")
    ap.add_argument("--db", default=os.path.expanduser(
        "~/.svrnmesh/indexes/commonwealth-ai/scip_graph.db"))
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--json", default=None)
    args = ap.parse_args()

    cases = [json.loads(l) for l in open(args.cases, encoding="utf-8") if l.strip()]
    for c in cases:                       # harvest `sites` IS the golden `truth`
        c["kind"] = "positive" if c["expect"].get("fire") else "negative"
        if c["expect"].get("fire"):
            c["expect"]["truth"] = c["expect"]["sites"]

    def call(c):
        rq = dict(c["request"]); rq["debug"] = True
        r = urllib.request.Request(f"{args.endpoint}/v1/edit_predictions",
                                   data=json.dumps(rq).encode(),
                                   headers={"content-type": "application/json"})
        try:
            with urllib.request.urlopen(r, timeout=60) as resp:
                return json.loads(resp.read())
        except Exception as e:                       # noqa: BLE001
            return {"_error": str(e)}
    with ThreadPoolExecutor(max_workers=args.workers) as ex:
        outs = list(ex.map(call, cases))

    errs = sum(1 for o in outs if "_error" in o)
    if errs:
        raise SystemExit(f"{errs}/{len(cases)} request errors — this run is not a "
                         f"measurement. First: {next(o['_error'] for o in outs if '_error' in o)}")

    con = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    verd = collections.Counter(); good = tot = 0
    a3 = collections.Counter(); rows = []
    for c, o in zip(cases, outs):
        edits = o.get("edits", [])
        g, t = SG.site_precision(c, edits); good += g; tot += t
        v = SG.score_positive(c, edits) if c["kind"] == "positive" else (
            "neg_WRONG" if edits else "neg_silent")
        verd[v] += 1
        rows.append({"id": c["id"], "kind": c["kind"], "outcome": v,
                     "hunks_good": g, "hunks_total": t})
        # --- A3, junk only ---
        if not edits:
            continue
        path = (c.get("aligned") or {}).get("repo_path")
        if not path or not os.path.exists(path):
            continue
        text = c["request"]["text"].split("\n")
        head = open(path, encoding="utf-8", errors="replace").read().split("\n")
        idx = SG._u16_to_str_index(c["request"]["text"])
        for lineno, grp in line_groups(c, edits):
            gg, gt = SG.site_precision(c, grp)
            if gt == 0 or gg == gt:
                continue                                   # not junk
            a3["junk_line_groups"] += 1
            if lineno >= len(head) or text[lineno] != head[lineno]:
                a3["unmappable_to_head"] += 1              # reported, never guessed
                continue
            lo = min(idx[e["start"]] for e in grp); hi = max(idx[e["end"]] for e in grp)
            ls = c["request"]["text"].rfind("\n", 0, lo) + 1
            a3["scip_visible" if scip_covered(con, path, lineno, lo - ls, hi - ls)
               else "scip_INVISIBLE (comment/string)"] += 1

    pos = sum(v for k, v in verd.items() if not k.startswith("neg_"))
    useful = verd["useful"] + verd["partial"]
    fires = useful + verd["wrong"] + verd["neg_WRONG"]
    print(f"cases {len(cases)}  ({pos} positive, {len(cases)-pos} negative)   0 request errors")
    print(f"\nPOSITIVES  {dict(sorted((k,v) for k,v in verd.items() if not k.startswith('neg_')))}")
    print(f"NEGATIVES  silent {verd['neg_silent']}   WRONG FIRE {verd['neg_WRONG']}")
    def pc(k, n, label):
        lo, hi = SG.wilson(k, n) if n else (0, 0)
        print(f"  {label:16s} {k}/{n} = {k/n*100:5.1f}%   (95% CI {lo*100:.1f}-{hi*100:.1f}, +/-{(hi-lo)/2*100:.1f}pts)")
    print()
    pc(useful, pos, "useful-fire")
    pc(verd["missed"], pos, "missed-fire")
    pc(verd["wrong"] + verd["neg_WRONG"], fires, "wrong-fire")
    pc(good, tot, "hunk-precision")
    print(f"\nA3 — where the JUNK lives (junk line-groups: {a3['junk_line_groups']})")
    for k in ("scip_INVISIBLE (comment/string)", "scip_visible", "unmappable_to_head"):
        n = a3[k]
        if a3["junk_line_groups"]:
            print(f"  {k:34s} {n:>4}  {n/a3['junk_line_groups']*100:5.1f}%")
    if args.json:
        json.dump(rows, open(args.json, "w"), indent=2)
        print(f"\nraw -> {args.json}")


if __name__ == "__main__":
    main()
