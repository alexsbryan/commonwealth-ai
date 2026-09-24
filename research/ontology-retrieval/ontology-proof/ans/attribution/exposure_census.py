#!/usr/bin/env python3
"""Exposure census: how many bank questions were in the dominance moved band?

Retrieval-only (prod-pipeline): no synthesis, no judge, no answer scores.
The evidence-shape routing line logs count / top_source_repeat /
distinct_sources per question; attributed to questions via query_hash on
pipeline trace events. Classification:
  still_dominant      repeat>=2 and repeat*5>=count  (fix changes nothing)
  exposed_moved_band  repeat>=2 and repeat*5<count   (old trigger collapsed it)
  never_dominant      repeat<2                        (fix changes nothing)
"""
import hashlib
import json
import os
import re
import subprocess
import sys
from collections import Counter

ENV = dict(os.environ, RUST_LOG="sovereign_core=info,retrieval.pipeline=debug,retrieval_audit=info",
           NO_COLOR="1", SOVEREIGN_ATLAS_GROUNDING="1",
           SOVEREIGN_ATOM_ENUM="0", SOVEREIGN_ATOM_ENUM_OVERVIEW="0")
CMD = ["target/debug/sovereign-cli", "eval", "run",
       "--bank", "research/ontology-retrieval/ontology-proof/ans/bank.attested.toml",
       "--prod-pipeline", "--isolate", "--limit", "20", "--format", "json"]
OUT = sys.argv[1] if len(sys.argv) > 1 else "/var/folders/qf/4ntssn_10d598hw_5rjhx3740000gp/T/opencode/ei7-shape-census.json"

proc = subprocess.run(CMD, env=ENV, text=True, stdout=subprocess.PIPE,
                      stderr=subprocess.PIPE, timeout=900)
if proc.returncode != 0:
    tail = [x[:200] for x in proc.stderr.splitlines()[-4:]]
    raise SystemExit(f"exit {proc.returncode}\n" + "\n".join(tail))
run = json.loads(proc.stdout)
lines = [re.sub(r"\x1b\[[0-9;]*m", "", s) for s in proc.stderr.splitlines()]

SHAPE = re.compile(
    r"evidence-shape routing decision count=(\d+) .*?"
    r"top_source_repeat=(\d+) distinct_sources=(\d+)")
HASH = re.compile(r"passage identities .*?query_hash=([0-9a-f]+)")

current = None
shapes = {}
for line in lines:
    m = HASH.search(line)
    if m:
        current = m.group(1)
        continue
    m = SHAPE.search(line)
    if m and current:
        shapes[current] = tuple(int(g) for g in m.groups())
        current = None  # one routing decision per question

rows, matched = {}, 0
for row in run["results"]:
    qhash = hashlib.sha256(row["question"].encode()).hexdigest()[:12]
    shape = shapes.get(qhash)
    if shape:
        matched += 1
    rows[row["question_id"]] = {"category": row["category"], "shape": shape}

tab = Counter()
for qid, r in rows.items():
    cat, sh = r["category"], r["shape"]
    if sh is None:
        tab[(cat, "NO_SHAPE")] += 1
        continue
    count, repeat, _distinct = sh
    if repeat < 2:
        tab[(cat, "never_dominant")] += 1
    elif repeat * 5 >= count:
        tab[(cat, "still_dominant")] += 1
    else:
        tab[(cat, "exposed_moved_band")] += 1

out = {"matched": matched, "questions": len(rows), "table": rows,
       "summary": {f"{k[0]}/{k[1]}": v for k, v in sorted(tab.items())}}
open(OUT, "w").write(json.dumps(out, indent=1, sort_keys=True))
print(f"matched {matched}/{len(rows)}")
for k, v in sorted(tab.items()):
    print(f"  {k[0]:18s} {k[1]:20s} {v}")
exposed = {qid: r for qid, r in rows.items() if r["shape"] and r["shape"][1] >= 2 and r["shape"][1] * 5 < r["shape"][0]}
print(f"\nexposed question ids ({len(exposed)}):")
for qid, r in sorted(exposed.items()):
    print(f"  {qid:36s} {r['category']}  shape={r['shape']}")
