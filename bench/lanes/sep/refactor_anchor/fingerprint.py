#!/usr/bin/env python3
"""Render an `svrn eval run --format json` run as canonical fingerprint lines.

Kept out of capture.sh so it is plain Python rather than shell-escaped Python.
`retrieved[]` carries no chunk id field, so identity is the tuple that is
actually stable across runs: corpus, url, title, provenance tag, and the score
rounded to 6 places, plus a short hash of the snippet — several chunks of one
article share url and title, and the hash is what tells them apart. Rounding is
deliberate — f32 formatting jitter is not a behaviour change, and 6 places
still moves on any real reordering.
"""
import hashlib
import json
import sys

run = json.load(sys.stdin)
print(f"bank   {run['bank_name']}  corpus={run['corpus']}  limit={run['limit']}")
for r in run["results"]:
    if r.get("error"):
        # ARCH §18.2 four verdicts: a failed turn is COULD-NOT-JUDGE, not a zero.
        print(f"result {r['question_id']}  ERROR  {r['error']}")
        continue
    print(
        f"result {r['question_id']}  vector_eligible={r['vector_eligible']}"
        f"  corpora_hit={sorted(r['corpora_hit'])}"
    )
    for tag, key in (("src", "source_score"), ("fct", "fact_score")):
        s = r[key]
        print(f"  {tag}    matched={sorted(s['matched'])}")
        print(f"  {tag}    missing={sorted(s['missing'])}")
        print(
            f"  {tag}    unscorable={sorted(s.get('unscorable', []))}"
            f"  total_expected={s['total_expected']}"
        )
    for i, c in enumerate(r["retrieved"]):
        print(
            f"  chunk {i:03d}  {c['corpus_id']}  {c.get('url') or '-'}"
            f"  {c.get('title') or '-'}  {c.get('source') or '-'}  {c['score']:.6f}"
            f"  {hashlib.sha256(c['snippet'].encode()).hexdigest()[:12]}"
        )
