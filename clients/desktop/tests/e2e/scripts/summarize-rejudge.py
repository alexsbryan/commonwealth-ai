#!/usr/bin/env python3
"""Aggregate a rejudge sidecar into an honest composite + per-category breakdown,
and join each verdict back to its question+answer head for eyeballing. Also
de-duplicates repeated questions so the "unique honest" number isn't inflated by
the chaos brain re-asking the same hard question."""
import json
import sys
import re

sidecar = sys.argv[1]
journal = sys.argv[2]

# journal: step -> (question, answer)
qa = {}
for line in open(journal):
    try:
        r = json.loads(line)
    except Exception:
        continue
    if r.get("cmd") != "send_message_stream":
        continue
    step = r.get("step")
    args = r.get("args") or ""
    m = re.search(r'"message":"(.*?)","conversationId', args, re.S) or re.search(r'"message":"(.*)$', args, re.S)
    q = (m.group(1).replace('\\"', '"').replace('\\\\', '\\') if m else "")[:120]
    qa[step] = (q, (r.get("answer") or "")[:220].replace("\n", " ⏎ "))

verdicts = []
for line in open(sidecar):
    line = line.strip()
    if not line:
        continue
    try:
        verdicts.append(json.loads(line))
    except Exception:
        pass

n = len(verdicts)
broke = [v for v in verdicts if v.get("broken")]
cats = {}
for v in verdicts:
    cats[v.get("category", "?")] = cats.get(v.get("category", "?"), 0) + 1

print(f"=== REJUDGE SUMMARY: {sidecar} ===")
print(f"judged={n}  broke={len(broke)}  honest_composite={100*(n-len(broke))//max(n,1)}%")
print("categories:", json.dumps(cats, sort_keys=True))
print()
print("=== BROKE detail (step | category | why | Q | A-head) ===")
for v in broke:
    step = v.get("step")
    q, a = qa.get(step, ("?", "?"))
    print(f"\n[step {step}] {v.get('category')}")
    print(f"  why: {v.get('why','')[:140]}")
    print(f"  Q  : {q}")
    print(f"  A  : {a}")

# unique-question dedup: collapse by normalized question
print("\n=== UNIQUE-QUESTION view (dedup repeated Qs) ===")
norm = {}
for v in verdicts:
    q, _ = qa.get(v.get("step"), ("?", "?"))
    key = re.sub(r"\s+", " ", q.lower()).strip()[:80]
    # keep the best (non-broken wins) per question
    prev = norm.get(key)
    if prev is None or (prev.get("broken") and not v.get("broken")):
        norm[key] = v
uq = list(norm.values())
ub = [v for v in uq if v.get("broken")]
print(f"unique_questions={len(uq)}  unique_broke={len(ub)}  unique_honest={100*(len(uq)-len(ub))//max(len(uq),1)}%")
ucats = {}
for v in uq:
    ucats[v.get("category", "?")] = ucats.get(v.get("category", "?"), 0) + 1
print("unique categories:", json.dumps(ucats, sort_keys=True))
