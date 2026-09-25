#!/usr/bin/env python3
"""W2 attribution: where the grounded path's K0/K1 losses come from.

Run over the committed study-1 boards. Findings (2026-09-23):

- K0 losses on `full` are DISPLACEMENT, three forms:
    wrong-sections  — right document floods the prompt, the exact section
                      similarity would have picked is not in the pool
                      (lookup-megara-obverse-dies, lookup-agrinion-acquired)
    retrieved-cut   — gold chunk retrieved (rank 13) but in_prompt=False,
                      cut by the char budget (lookup-demanhur-first-notice)
    never-pooled    — gold chunk absent from the pool entirely
                      (lookup-histiaea-oxidation)
- K1 decline (list-igch0076-mints): bare pools 10 chunks from ONE monograph
  (judge 0.33, answers); full pools 24 chunks from 8 documents — the walk
  reached the Kyparissia hoard atom at hop 0 (153 seeds) but its chunks are
  not in the pool; judge 0.00; the decline text is the MODEL'S draft
  (gate only reclassifies, grounding/inner.rs:1086).
- Mechanism, one family: walk-injected real chunks (seeds >= 12, per-kind
  quotas stack on top — ground.rs:777) enter the pool ahead of the merge and
  displace similarity's concentrated evidence within the fixed slot/char
  budget. The D2 half-reservation bounds virtual chunks only.
"""
import json, os, sys

PROOF = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def load(arm, run):
    if arm in ("bare", "full"):
        p = os.path.join(PROOF, "runs", arm, f"run-{run}", "eval.json")
    else:
        p = os.path.join(PROOF, f"runs-{arm}", f"run-{run}", "eval.json")
    return {r["question_id"]: r for r in json.load(open(p))["results"]}

def doc_of(title):
    return title.split("›")[0].strip() if title and "›" in title else (title or "")[:40]

K0_LOSSES = [
    "lookup-agrinion-acquired",
    "lookup-demanhur-first-notice",
    "lookup-histiaea-oxidation",
    "lookup-megara-obverse-dies",
]
K1_CASE = "list-igch0076-mints"

def cite_title(ans):
    import re
    m = re.search(r"Grounded in the source:\s*(.+?)\s*—", ans) or re.search(r"\[Source:\s*(.+?)\]", ans)
    return m.group(1) if m else None

def dissect(q, bare, full):
    print(f"== {q}")
    b, f = bare[q], full[q]
    cited = cite_title(b["synth"]["answer"])
    bp = [c for c in b["retrieved"] if c.get("in_prompt")]
    gold = [c["title"] for c in bp if cited and cited[:40] in (c.get("title") or "")]
    print(f"   bare: n_in_prompt={len(bp)} judge={b['synth']['judge_fact_score']['ratio']:.2f} cited={cited!r}")
    for t in gold[:1]:
        in_f = [(i, c.get("in_prompt")) for i, c in enumerate(f["retrieved"]) if c["title"] == t]
        form = "retrieved-cut" if in_f and not in_f[0][1] else ("in-pool" if in_f else "never-pooled")
        print(f"   gold in full: {form} {in_f[:1]}")
    if not gold:
        print("   gold: bare title match failed")
    fp = [c for c in f["retrieved"] if c.get("in_prompt")]
    docs = {}
    for c in fp:
        docs[doc_of(c.get("title"))] = docs.get(doc_of(c.get("title")), 0) + 1
    print(f"   full: n_in_prompt={len(fp)} docs={len(docs)} judge={f['synth']['judge_fact_score']['ratio']:.2f} "
          f"gate={f['synth']['gate']['action']}")
    print(f"   full in_prompt docs: {docs}")

def main():
    which = sys.argv[1:] or ["r1"]
    bare = load("bare", 1)
    full = load("full", 1)
    for q in K0_LOSSES:
        dissect(q, bare, full)
    b, f = bare[K1_CASE], full[K1_CASE]
    w = f.get("atlas_walk") or {}
    kyp = [n for n in (w.get("nodes") or []) if "yparissia" in json.dumps(n).lower()]
    kyp_chunks = [i for i, c in enumerate(f["retrieved"]) if "yparissia" in ((c.get("title") or "") + (c.get("snippet") or "")).lower()]
    print(f"== {K1_CASE}")
    print(f"   bare: n_in_prompt={sum(1 for c in b['retrieved'] if c.get('in_prompt'))} "
          f"docs={len({doc_of(c.get('title')) for c in b['retrieved']})} judge={b['synth']['judge_fact_score']['ratio']:.2f}")
    print(f"   full: n_in_prompt={sum(1 for c in f['retrieved'] if c.get('in_prompt'))} "
          f"docs={len({doc_of(c.get('title')) for c in f['retrieved']})} judge={f['synth']['judge_fact_score']['ratio']:.2f} "
          f"gate={f['synth']['gate']['action']}")
    print(f"   walk: seeds={w.get('seeds')} hop0_hoard_atoms={[n['name'] for n in kyp][:2]} "
          f"kyparissia_chunks_in_pool={kyp_chunks}")

if __name__ == "__main__":
    main()
