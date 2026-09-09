#!/usr/bin/env python3
"""HELD-OUT confirmation of §18's scatter detector, on a structurally different corpus.

§18 found that when a corpus HOLDS the answer retrieval concentrates, and when
it does not retrieval scatters -- AUC ~0.80, free and model-free. That reading
came from a POST-HOC sign flip (§16 measured the same feature positively on an
all-covered distribution), so it is a hypothesis, not a result, until it
survives data it was not derived on.

WHAT MAKES THIS HELD OUT
  - The rule is FROZEN before this corpus is touched: feature, k, direction and
    threshold are read from scratchpad/frozen_rule.json, derived on §18's SEP
    units alone (top-3 title share at k=28 >= 0.607; 75.8% there). Nothing is
    refit here.
  - The corpus is STRUCTURALLY different, which is the real transfer risk. SEP
    averages ~106 chunks per article (`corpus diag sep`), so 28 hits can come
    from a handful of documents. A code/doc corpus has many small files, so raw
    distinct-title COUNTS cannot transfer -- which is exactly why the frozen
    feature is the scale-free SHARE rather than the count §18 reported first.
  - Ground truth for the positives is GREP-VERIFIED against the repo, not
    asserted: any expected fact whose literal string is not found is dropped
    before the run, so the covered label is checkable rather than my judgement.

PRE-REGISTERED BARS (before the held-out corpus is queried)
  B1 -- AUC of the frozen feature on held-out units:
      >= 0.75 -> the signal transfers; a free gap detector is real and the
                 abstain/acquire trigger can be built on it.
      <= 0.60 -> it does not transfer; §18 was a property of SEP's structure,
                 the post-hoc worry is vindicated, and this dies here.
  B2 -- accuracy of the FROZEN THRESHOLD (not a refit one). A feature can
      transfer while its threshold does not; that distinction decides whether
      the detector needs per-corpus calibration or ships as one constant.
      Reported against the derivation set's 75.8%.
  B3 -- the 4B evaluator on the same held-out units, as the paired control it
      was in §18 (0.626 there). Reported either way (§18.6).

  PREDICTION: the feature transfers (the mechanism -- scatter means no home --
  is not SEP-specific) but the THRESHOLD does not, because chunk-per-document
  ratios differ by an order of magnitude between the two corpora. If that is
  right, B1 passes and B2 fails, and the detector needs per-corpus calibration.
"""
import json, re, subprocess, urllib.request
from pathlib import Path

ROOT = Path("/home/alexbryan/dev/commonwealth-ai")
SCRATCH = Path("/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/"
               "db10109d-a40f-4092-891f-e981f8c9f477/scratchpad")
RULE = json.loads((SCRATCH / "frozen_rule.json").read_text())
CORPUS, K = "commonwealth-ai", RULE["k"]
GEN = "http://127.0.0.1:9741/v1/chat/completions"
MODEL = "Qwen3.5-4B-UD-MTP-Q6_K_XL"

# Positives: questions about THIS repo. Every expected fact is grep-verified below.
COVERED = [
 ("cov_coverage_factor", "How does corpus search compose the final chunk set instead of plain top-k truncation?", ["facility-location", "coverage_factor"]),
 ("cov_reranker_reject", "Why was the cross-encoder reranker slot rejected and what would re-open it?", ["SOVEREIGN_RERANK_MODEL_PATH", "TTFT"]),
 ("cov_refinement_surface", "What does the Refinement gate surface do about retries?", ["GateSurface", "verify-only"]),
 ("cov_zero_test", "What happens when a filtered test run matches no tests?", ["allow-empty", "pass: 0 fail: 0"]),
 ("cov_verification_enum", "What verification states can a claim hold in the epistemic ledger?", ["FailedOnce", "FailOpen", "Unverified"]),
 ("cov_dev_tools", "What breaks if sovereign-cli is built without the dev-tools feature?", ["dev-tools", "sovereign-cli-dev"]),
 ("cov_watchers_off", "Why are the watchers disabled in this workspace?", ["watchers", "enabled = false"]),
 ("cov_toolbox", "Why must builds run inside the sovereign-vulkan toolbox on this host?", ["sovereign-vulkan", "llama-cpp-sys-4"]),
 ("cov_work_atlas", "How does an agent declare and release work scope on the mesh?", ["declare_scope", "release_scope"]),
 ("cov_tombstone", "What did the longform repair ladder tombstone stop executing?", ["SOVEREIGN_GATE_LONGFORM_REPAIR", "tombstone"]),
 ("cov_env_gate", "What must a new environment variable read carry to pass the gate?", ["env-flags.toml", "env-gate"]),
 ("cov_size_gate", "How is per-crate code size ratcheted?", ["size-gate", "baseline"]),
 ("cov_prepush", "What is the time budget for the pre-push gate?", ["pre-push", "rustfmt"]),
 ("cov_scip", "What populates the SCIP call graph and what happens when the export fails?", ["scip", "exporter"]),
 ("cov_drift", "What does a drift finding anchor against?", ["drift", "narrative"]),
]

def grep_ok(fact):
    p = subprocess.run(["git", "grep", "-qF", "--", fact], cwd=ROOT, capture_output=True)
    return p.returncode == 0

def slug(h):
    m = re.match(r"[0-9.]+\]\s*(\S+)", h.strip())
    return (m.group(1) if m else "").lower()

def retrieve(q, corpus=CORPUS, k=K):
    q = " ".join(q.split())[:1200]
    p = subprocess.run(["sovereign", "corpus", "search", corpus, q, "--limit", str(k)],
                       capture_output=True, text=True, timeout=300)
    return [x.strip() for x in re.split(r"^\s*\d+\.\s+\[", p.stdout or "", flags=re.M)[1:]]

def top3(pool):
    sl = [slug(h) for h in pool[:K]]
    if not sl: return 0.0
    return sum(sorted([sl.count(s) for s in set(sl)], reverse=True)[:3]) / len(sl)

def evaluate(question, window):
    ctx = "\n---\n".join(window)[:12000]
    body = {"model": MODEL, "temperature": 0.0, "max_tokens": 4,
            "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "user", "content":
                f"Passages:\n{ctx}\n\nQuestion: {question}\n\n"
                "Do these passages contain the information needed to answer? "
                "Reply with one word: YES or NO."}]}
    req = urllib.request.Request(GEN, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as r:
        return 0.0 if "NO" in json.load(r)["choices"][0]["message"]["content"].strip().upper() else 1.0

print(f"FROZEN RULE (not refit here): {RULE['feature']} @k={RULE['k']} "
      f">= {RULE['threshold']:.3f}, derived on {RULE['derived_on']} at {RULE['derivation_accuracy']:.1%}\n")

kept = []
for qid, q, facts in COVERED:
    bad = [f for f in facts if not grep_ok(f)]
    if bad:
        print(f"  DROPPED {qid}: not found in repo -> {bad}")
    else:
        kept.append((qid, q))
print(f"positives after grep verification: {len(kept)}/{len(COVERED)}")

import tomllib
sep = tomllib.load(open(ROOT / "sovereign/bench/sep/questions.toml", "rb"))["questions"]
wik = tomllib.load(open(ROOT / "sovereign/bench/wikipedia/questions.toml", "rb"))["questions"]
units = ([{"id": i, "q": q, "label": 1, "src": "repo"} for i, q in kept]
       + [{"id": s["id"], "q": s["question"], "label": 0, "src": "sep"} for s in sep]
       + [{"id": w["id"], "q": w["question"], "label": 0, "src": "wikipedia"} for w in wik])
print(f"held-out units: {sum(u['label'] for u in units)} positive, "
      f"{sum(1 for u in units if not u['label'])} negative\n")

for i, u in enumerate(units, 1):
    pool = retrieve(u["q"])
    u["t3"] = top3(pool)
    u["n_titles"] = len({slug(h) for h in pool[:K]})
    try:
        u["model"] = evaluate(u["q"], pool[:K])
    except Exception as e:
        print(f"  eval failed {u['id']}: {str(e)[:60]}"); u["model"] = None
    if i % 10 == 0: print(f"  {i}/{len(units)}", flush=True)

(ROOT / "sovereign/bench/sep_atlas/map-conversion-rung6/scatter_holdout.json").write_text(json.dumps(units, indent=1))

def auc(pairs):
    pos = [s for s, l in pairs if l == 1]; neg = [s for s, l in pairs if l == 0]
    if not pos or not neg: return None
    return sum(1.0 if p > n else 0.5 if p == n else 0.0 for p in pos for n in neg) / (len(pos) * len(neg))

P = [u for u in units if u["label"] == 1]; N = [u for u in units if u["label"] == 0]
print(f"\nmean top-3 share:  covered {sum(u['t3'] for u in P)/len(P):.3f}   "
      f"uncovered {sum(u['t3'] for u in N)/len(N):.3f}")
print(f"mean distinct titles: covered {sum(u['n_titles'] for u in P)/len(P):.1f}   "
      f"uncovered {sum(u['n_titles'] for u in N)/len(N):.1f}")

a = auc([(u["t3"], u["label"]) for u in units])
print(f"\n=== B1 — does the FEATURE transfer? ===")
print(f"  AUC {a:.3f}   (bar: >=0.75 transfers, <=0.60 dead)")
print(f"  {'TRANSFERS' if a>=0.75 else 'DEAD' if a<=0.60 else 'MARGINAL'}")

t = RULE["threshold"]
tp = sum(1 for u in P if u["t3"] >= t); tn = sum(1 for u in N if u["t3"] < t)
print(f"\n=== B2 — does the FROZEN THRESHOLD transfer? ===")
print(f"  accuracy {(tp+tn)/len(units):.1%}  (tpr {tp}/{len(P)}, tnr {tn}/{len(N)}) "
      f"vs {RULE['derivation_accuracy']:.1%} on the derivation set")
best = max(sorted(set(round(u['t3'],3) for u in units)),
           key=lambda th:(sum(1 for u in P if u['t3']>=th)+sum(1 for u in N if u['t3']<th)))
bacc = (sum(1 for u in P if u['t3']>=best)+sum(1 for u in N if u['t3']<best))/len(units)
print(f"  best threshold ON THIS corpus would be {best:.3f} ({bacc:.1%}) — reported to show "
      f"whether calibration is per-corpus, NOT adopted")

ok = [u for u in units if u["model"] is not None]
am = auc([(u["model"], u["label"]) for u in ok])
print(f"\n=== B3 — the 4B evaluator, paired control ===")
print(f"  AUC {am:.3f} on {len(ok)} units   (§18: 0.626)")
print(f"  scatter {'beats' if a>am else 'loses to'} the model here by {abs(a-am):.3f}")
