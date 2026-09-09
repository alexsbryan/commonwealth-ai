#!/usr/bin/env python3
"""Are the facts NO retrieval configuration found absent from the corpus, or
merely unreached? That fork decides acquisition vs fan-out.

Rung 6's four arms (2 stack versions x 2 samples) missed the same 8 expected
facts on the 21-question SEP bank -- the retrieval analogue of §11's stuck
floor. If those facts are simply not in SEP, more retrievers cannot help and
the mesh's route to groundedness is ACQUISITION. If they are present but were
never reached by the question-derived query, then query formulation is leaving
evidence on the table, and N nodes asking differently is a real mechanism.

METHOD. For each missed fact, issue an ORACLE query -- the fact text itself,
the best possible formulation -- against the same `sep` corpus, and apply the
bench's OWN matching rule (score.rs::partition_facts: every space-separated
token >= 3 chars must appear, case-insensitive substring). The haystack is the
CLI's titles + snippets, which are TRUNCATED to ~190 chars per hit. That makes
the probe CONSERVATIVE: a hit is definitive, a miss is weak evidence, and the
recovery rate reported here is a LOWER bound on what a full-text haystack
would give.

PRE-REGISTERED BARS (written before the first query)
  >= 5/8 recovered -> the hard core is PRESENT AND REACHABLE. The misses are
      query-formulation failures, not corpus gaps. Retrieval diversity has real
      headroom and fan-out over formulations is a live mechanism.
  <= 2/8 recovered -> consistent with absence (though truncation means this
      cannot prove it). Acquisition is the route; report and stop.
  3-4 -> mixed, reported as mixed, no story either way.

  INSTRUMENT CONTROL (§18.4), run in the same pass: the same oracle query for
  facts the arms DID find. If those do not recover at a high rate, the probe's
  truncated haystack is too small to detect presence at all and the primary
  result is VOID rather than negative.

  PREDICTION: all 8 are canonical SEP vocabulary for the entries their own
  questions name -- descriptivism is what Kripke's causal-historical theory is
  defined against; MacCallum is the standard citation in Positive and Negative
  Liberty. I expect high recovery, i.e. these are reachability failures. Stated
  before the run so the result can contradict it.
"""
import json, subprocess, sys
from pathlib import Path

SCRATCH = Path("/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/"
               "db10109d-a40f-4092-891f-e981f8c9f477/scratchpad")

def tokens(fact):
    return [t for t in fact.lower().replace("-", " ").split() if len(t) >= 3]

def matched(fact, haystack):
    """score.rs::partition_facts, reproduced: every token >=3 chars present."""
    h = haystack.lower()
    return all(t in h for t in tokens(fact))

def oracle(fact, limit=10, timeout=180):
    try:
        p = subprocess.run(["sovereign", "corpus", "search", "sep", fact,
                            "--limit", str(limit)],
                           capture_output=True, text=True, timeout=timeout)
        return p.stdout or ""
    except subprocess.TimeoutExpired:
        return None          # never counted as a miss (§18.3)

def run(label, facts):
    print(f"\n=== {label} ===")
    hit = err = 0
    for q, f in facts:
        out = oracle(f)
        if out is None:
            print(f"  {'TIMEOUT':>8}  {f:<22} ({q})"); err += 1; continue
        ok = matched(f, out)
        hit += ok
        print(f"  {'FOUND' if ok else 'not found':>9}  {f:<22} ({q})")
    n = len(facts) - err
    print(f"  recovered {hit}/{n}" + (f"  [{err} unscorable]" if err else ""))
    return hit, n

hard = json.load(open(SCRATCH / "hardcore.json"))
hard_facts = [(q, f) for q, fs in hard.items() for f in fs]

# control: facts every arm matched, one per question, same questions where possible
D = Path("/home/alexbryan/dev/commonwealth-ai/sovereign/bench/sep_atlas/map-conversion-rung6")
R = {a: {r["question_id"]: r for r in json.load(open(D / f"{a}.json"))["results"]}
     for a in ("armA", "armA2", "armB", "armB2")}
ctrl = []
for q in hard:
    common = set.intersection(*[set(x.strip() for x in R[a][q]["fact_score"]["matched"])
                                for a in R])
    if common:
        ctrl.append((q, sorted(common)[0]))

h_hit, h_n = run("HARD CORE — missed by all four arms", hard_facts)
c_hit, c_n = run("CONTROL — found by all four arms (instrument check)", ctrl)

print("\n=== verdict against the pre-registered bars ===")
print(f"  control recovery : {c_hit}/{c_n}"
      f"{' — instrument OK' if c_n and c_hit/c_n >= 0.6 else ' — INSTRUMENT TOO WEAK, primary is VOID'}")
print(f"  hard-core recovery: {h_hit}/{h_n}")
if c_n and c_hit / c_n < 0.6:
    print("  VERDICT: VOID — the truncated haystack cannot detect presence reliably.")
elif h_hit >= 5:
    print("  VERDICT: PRESENT AND REACHABLE — the hard core is a query-formulation\n"
          "  failure, not a corpus gap. Fan-out over formulations is a live mechanism.")
elif h_hit <= 2:
    print("  VERDICT: consistent with ABSENCE — acquisition, not fan-out.")
else:
    print("  VERDICT: MIXED — no story either way.")
