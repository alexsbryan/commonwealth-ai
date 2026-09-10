#!/usr/bin/env python3
"""The 90% case: what happens when the corpus does NOT hold the answer?

THE SCOPE DEFECT THIS REPAIRS. §11-§17 all ran on the SEP bank, and §13 proved
that corpus HOLDS every hard-core fact (8/8 reachable). So every result so far
is about the COVERED case -- and "k beats everything" is a statement about a
regime where the fact was already in the pool. A mesh RAG system covering ~10%
of what could be asked lives in the other 90%, where no k reaches anything and
the only decisions that matter are ACQUIRE or ABSTAIN. Nothing measured so far
speaks to it, and §16's sufficiency AUC of 0.633 was computed on a distribution
containing NO true negatives of the kind that matter.

THREE REGIMES, built from banks already on disk:
  covered  SEP questions on SEP -- the 10%, everything measured so far.
  ablated  SEP questions on SEP with each question's own `expected_sources`
           articles REMOVED from the pool. Topically adjacent, specifically
           missing -- the realistic hard case, and PAIRED with covered.
  foreign  wikipedia bank questions (Yalta, Einstein's 1905 papers) on SEP.
           Out of domain -- the extreme.

WHAT IS ACTUALLY UNDER TEST: not coverage, which will obviously collapse, but
DETECTION. In the 90% regime the load-bearing decision is "do I have this?",
because it gates acquisition and abstention. §16 could not measure it for want
of negatives. This has 41 of them.

PRE-REGISTERED BARS (before any call), each with a paired control.
  B1 PRIMARY -- detection AUC, covered (positive) vs ablated+foreign (negative),
    for the best FREE predictor and for the 4B evaluator:
      >= 0.80 -> detection is cheap and solvable; acquisition routing is
                 buildable and the mesh's job is acquire/abstain, not retrieve.
      <= 0.65 -> the system cannot tell what it does not have, even when it
                 genuinely does not have it. That is a far more serious finding
                 than any negative so far and it blocks the whole family.
  B2 -- does k help where the fact is absent? Coverage at k=10 vs k=80 within
    ablated and within foreign. PREDICTION: flat. If confirmed, §14/§17's "k
    wins" is explicitly scoped to the covered regime, which is the correction
    this section owes.
  B3 -- reported both ways (§18.6): whether the 4B evaluator finally beats the
    free predictors here. It lost to counting distinct documents in §16 (0.633
    vs 0.661) on a distribution with no real negatives; a strong signal should
    favour the model, and if it still does not, that is the finding.

  PREDICTION: foreign is easy to detect and ablated is hard, because ablated
  still returns fluent, topically-adjacent philosophy. If that is right, the
  aggregate AUC will be flattered by the easy half and the ablated-only number
  is the one that matters. Both are reported separately for that reason.
"""
import json, re, subprocess, time, tomllib, urllib.request
from pathlib import Path

ROOT = Path("/home/alexbryan/dev/commonwealth-ai")
D = ROOT / "sovereign/bench/sep_atlas/map-conversion-rung6"
GEN = "http://127.0.0.1:9741/v1/chat/completions"
MODEL = "Qwen3.5-4B-UD-MTP-Q6_K_XL"
NS = [10, 28, 80]

def toks(f): return [t for t in f.lower().replace("-", " ").split() if len(t) >= 3]
def hit(f, hay): return all(t in hay.lower() for t in toks(f))
def slug(h):
    m = re.match(r"[0-9.]+\]\s*(\S+)", h.strip())
    return (m.group(1) if m else "").lower().replace("_", "-")

def retrieve(q, k=80):
    q = " ".join(q.split())[:1200]
    p = subprocess.run(["sovereign", "corpus", "search", "sep", q, "--limit", str(k)],
                       capture_output=True, text=True, timeout=300)
    return [x.strip() for x in re.split(r"^\s*\d+\.\s+\[", p.stdout or "", flags=re.M)[1:]]

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

sep = tomllib.load(open(ROOT / "sovereign/bench/sep/questions.toml", "rb"))["questions"]
wik = tomllib.load(open(ROOT / "sovereign/bench/wikipedia/questions.toml", "rb"))["questions"]
cached = json.loads((D / "hyde_pools.json").read_text())

CK = D / "uncovered_pools.json"
pools = json.loads(CK.read_text()) if CK.exists() else {}
for w in wik:
    key = "foreign::" + w["id"]
    if key not in pools:
        pools[key] = retrieve(w["question"]); CK.write_text(json.dumps(pools))
        print(f"foreign retrieve {w['id']}", flush=True)

units = []
for s in sep:
    qid, pool = s["id"], cached.get(s["id"], {}).get("baseline")
    if not pool: continue
    home = {x.lower().replace("_", "-") for x in s.get("expected_sources", [])}
    abl = [h for h in pool if slug(h) not in home]
    units.append({"id": qid, "regime": "covered", "q": s["question"],
                  "facts": s["expected_facts"], "pool": pool})
    units.append({"id": qid, "regime": "ablated", "q": s["question"],
                  "facts": s["expected_facts"], "pool": abl,
                  "dropped": len(pool) - len(abl)})
for w in wik:
    units.append({"id": w["id"], "regime": "foreign", "q": w["question"],
                  "facts": w["expected_facts"], "pool": pools["foreign::" + w["id"]]})

print(f"\n{len(units)} units: " + ", ".join(
    f"{r}={sum(1 for u in units if u['regime']==r)}" for r in ("covered", "ablated", "foreign")))
drop = [u["dropped"] for u in units if u["regime"] == "ablated"]
print(f"ablation removed {sum(drop)/len(drop):.1f} hits/question on average\n")

print("=== B2: does k help where the fact is absent? ===")
print(f"{'regime':<10} " + "  ".join(f"k={k:<12}" for k in NS))
for r in ("covered", "ablated", "foreign"):
    us = [u for u in units if u["regime"] == r]
    tot = sum(len(u["facts"]) for us_ in [us] for u in us_)
    cells = []
    for k in NS:
        c = sum(sum(1 for f in u["facts"] if hit(f, "\n".join(u["pool"][:k]))) for u in us)
        cells.append(f"{c}/{tot} {c/tot:>5.1%}")
    print(f"{r:<10} " + "  ".join(f"{c:<14}" for c in cells))

print("\nevaluator + free predictors at k=28 ...", flush=True)
for u in units:
    w = u["pool"][:28]
    u["f_titles"] = len({slug(h) for h in w})
    u["f_chars"] = len("\n".join(w))
    try:
        u["f_model"] = evaluate(u["q"], w)
    except Exception as e:
        print(f"  EVAL FAILED {u['regime']}/{u['id']}: {str(e)[:70]}"); u["f_model"] = None
(D / "uncovered_regime.json").write_text(json.dumps(units, indent=1))

def auc(pairs):
    pos = [s for s, l in pairs if l == 1]; neg = [s for s, l in pairs if l == 0]
    if not pos or not neg: return None
    return sum(1.0 if p > n else 0.5 if p == n else 0.0 for p in pos for n in neg) / (len(pos)*len(neg))

ok = [u for u in units if u["f_model"] is not None]
print(f"\n=== B1/B3: detection ({len(ok)} scorable) ===")
for label, negs in (("vs ablated+foreign", ("ablated", "foreign")),
                    ("vs ablated only", ("ablated",)),
                    ("vs foreign only", ("foreign",))):
    sub = [u for u in ok if u["regime"] == "covered" or u["regime"] in negs]
    for pred in ("f_titles", "f_model"):
        a = auc([(u[pred], 1 if u["regime"] == "covered" else 0) for u in sub])
        print(f"  {label:<20} {pred:<10} AUC {a if a is None else round(a,3)}")
    print()
say = lambda a: "SOLVABLE" if a and a >= .80 else "BLOCKED" if a and a <= .65 else "MARGINAL"
full = [u for u in ok]
am = auc([(u["f_model"], 1 if u["regime"] == "covered" else 0) for u in full])
at = auc([(u["f_titles"], 1 if u["regime"] == "covered" else 0) for u in full])
print(f"  B1 VERDICT (bar >=0.80 solvable, <=0.65 blocked): model {say(am)}, free {say(at)}")
mod_only = [u for u in ok if u["regime"] in ("covered", "ablated")]
aa = auc([(u["f_model"], 1 if u["regime"] == "covered" else 0) for u in mod_only])
print(f"  the number that matters (ablated-only, the realistic case): {aa if aa is None else round(aa,3)} -> {say(aa)}")
