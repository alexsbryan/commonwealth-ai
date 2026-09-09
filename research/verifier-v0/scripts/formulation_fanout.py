#!/usr/bin/env python3
"""Does REALISTIC query fan-out recover what one query misses -- and what does it cost?

§13 showed the 8 facts no rung-6 arm reached are all recoverable from the same
corpus by an ORACLE query (the fact text itself, 8/8, control 7/7). That proves
reachability and proves nothing about strategy: you cannot query for a fact you
do not know you are missing. This removes the oracle. Sub-queries are generated
from the QUESTION ALONE by the daemon's resident 4B, exactly as a node in a
fan-out would have to.

THREE ARMS, ONE INSTRUMENT. §13's numbers came from the bench's eval pipeline
(atlas walk, rerank) while this harness uses plain `corpus search`. Rather than
compare across instruments -- which would confound strategy with pipeline -- all
three arms run through THIS harness and the comparison is paired within it. The
arms' published 88-89% is context, not the baseline.
  baseline : the question, as one query, top-N
  fanout   : K sub-queries from the question alone, union of their hits
  oracle   : the missing fact as the query -- the ceiling, re-measured here

SCORING is the bench's own rule (score.rs::partition_facts: every space-
separated token >= 3 chars present, case-insensitive substring). The haystack is
the CLI's TRUNCATED titles+snippets, identically for every arm -- so all three
are undercounted by the same bias and a positive delta is CONSERVATIVE.

PRE-REGISTERED BARS (written before the first generation call)
  Primary -- does strategy replace the oracle? Of the 8 hard-core facts:
    >= 4/8 recovered by fanout -> question-only decomposition reaches what one
        query misses. The mechanism survives without the oracle and is live.
    <= 1/8 -> §13 was a curiosity about reachability, not a route to it.
        Report and stop; the mesh's route is acquisition after all.
    2-3   -> weak, reported as weak.
  Secondary -- does it help beyond the hard core? Total fact coverage,
    fanout vs baseline, over all 158 expected facts. Reported either way (§18.6),
    including if fan-out RETRIEVES FEWER facts than the single query, which is a
    real possibility: K narrow queries can each miss what one broad query caught.

COSTS, reported unconditionally and never traded away silently
  - retrievals issued (K+1 vs 1)
  - unique chunks in the union, and chars of context added over baseline
  - wall time, BOTH serial (sum -- the work) and parallel (max -- the added
    latency when N nodes run the sub-queries concurrently). The gap between
    those two numbers IS the mesh's economic argument, so both are printed.
  - This also closes a debt named in §13: retrieval cost is uninstrumented in
    the bench (embed_ms/search_ms read 0 against a 58.8s synthesis median), so
    these are the first retrieval timings this arc has.

NOT MEASURED, and it is the next rung, not a footnote: whether a larger union
makes the ANSWER better. Prior work here says a bigger window is not free
(prefill dominates a turn; K-cuts hurt synthesis), so coverage gain at the
retrieval layer is an upper bound on the end-to-end gain, not a prediction of it.
"""
import json, re, subprocess, sys, time
import concurrent.futures as cf
from pathlib import Path

D = Path("/home/alexbryan/dev/commonwealth-ai/sovereign/bench/sep_atlas/map-conversion-rung6")
GEN_URL = "http://127.0.0.1:9741/v1/chat/completions"
GEN_MODEL = "Qwen3.5-4B-UD-MTP-Q6_K_XL"
K = 4          # sub-queries per question
TOPN = 10      # hits per sub-query; baseline gets K*TOPN so context is comparable

def tok(f):
    return [t for t in f.lower().replace("-", " ").split() if len(t) >= 3]

def matched(fact, hay):
    h = hay.lower()
    return all(t in h for t in tok(fact))

def search(q, limit):
    t0 = time.time()
    try:
        p = subprocess.run(["sovereign", "corpus", "search", "sep", q, "--limit", str(limit)],
                           capture_output=True, text=True, timeout=240)
        return (p.stdout or ""), time.time() - t0
    except subprocess.TimeoutExpired:
        return None, time.time() - t0

def subqueries(question):
    """K short search queries from the QUESTION ALONE. Parsimonious prompt: small
    models do better with fewer words, and this one must not see the facts."""
    body = {"model": GEN_MODEL, "temperature": 0.3, "max_tokens": 220,
            "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "user", "content":
                f"Question: {question}\n\n"
                f"Write {K} short search queries that would find different parts of the "
                f"evidence needed. Each on its own line, no numbering, no explanation."}]}
    import urllib.request
    req = urllib.request.Request(GEN_URL, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as r:
        txt = json.load(r)["choices"][0]["message"]["content"]
    qs = [re.sub(r"^[\s\-\*\d\.\)]+", "", l).strip().strip('"')
          for l in txt.splitlines() if l.strip()]
    return [q for q in qs if len(q) > 8][:K]

R = {a: {r["question_id"]: r for r in json.load(open(D / f"{a}.json"))["results"]}
     for a in ("armA", "armA2", "armB", "armB2")}
QS = sorted(R["armA"])

hard = {}
for q in QS:
    miss = None
    for a in R:
        m = set(x.strip() for x in R[a][q]["fact_score"]["missing"])
        miss = m if miss is None else (miss & m)
    if miss:
        hard[q] = sorted(miss)

rows = []
for i, q in enumerate(QS, 1):
    rec = R["armA"][q]
    question = rec["question"]
    expected = ([x.strip() for x in rec["fact_score"]["matched"]] +
                [x.strip() for x in rec["fact_score"]["missing"]])
    print(f"[{i}/{len(QS)}] {q}", flush=True)

    b_hay, b_t = search(question, K * TOPN)
    try:
        subs = subqueries(question)
    except Exception as e:
        print(f"    subquery generation FAILED: {str(e)[:90]}", flush=True)
        subs = []
    if not subs:
        rows.append({"q": q, "error": "no subqueries"}); continue

    with cf.ThreadPoolExecutor(max_workers=K) as ex:
        res = list(ex.map(lambda s: search(s, TOPN), subs))
    f_hay = "\n".join(h or "" for h, _ in res)
    times = [t for _, t in res]

    b_ok = [f for f in expected if b_hay and matched(f, b_hay)]
    f_ok = [f for f in expected if matched(f, f_hay)]
    rows.append({"q": q, "question": question, "subs": subs, "expected": len(expected),
                 "baseline": len(b_ok), "fanout": len(f_ok),
                 "hard": hard.get(q, []),
                 "hard_baseline": [f for f in hard.get(q, []) if b_hay and matched(f, b_hay)],
                 "hard_fanout":   [f for f in hard.get(q, []) if matched(f, f_hay)],
                 "b_chars": len(b_hay or ""), "f_chars": len(f_hay),
                 "b_time": b_t, "f_serial": sum(times), "f_parallel": max(times)})

ok = [r for r in rows if "error" not in r]
Path(D / "formulation_fanout.json").write_text(json.dumps(rows, indent=1))

tot_e = sum(r["expected"] for r in ok)
tot_b = sum(r["baseline"] for r in ok)
tot_f = sum(r["fanout"] for r in ok)
hb = sum(len(r["hard_baseline"]) for r in ok)
hf = sum(len(r["hard_fanout"]) for r in ok)
hn = sum(len(r["hard"]) for r in ok)

print(f"\n=== BENEFIT ===  ({len(ok)}/{len(QS)} questions scored)")
print(f"  total expected facts            {tot_e}")
print(f"  baseline  (1 query, {K*TOPN} hits)   {tot_b:>4}  = {tot_b/tot_e:.1%}")
print(f"  fan-out   ({K} queries x {TOPN})      {tot_f:>4}  = {tot_f/tot_e:.1%}   "
      f"delta {tot_f-tot_b:+d} facts ({(tot_f-tot_b)/tot_e:+.1%})")
print(f"\n  HARD CORE (missed by all four rung-6 arms), n={hn}")
print(f"    recovered by baseline here    {hb}/{hn}")
print(f"    recovered by fan-out          {hf}/{hn}      (pre-registered bar: >= 4/8)")

print(f"\n=== COST ===")
bc = sum(r["b_chars"] for r in ok); fc = sum(r["f_chars"] for r in ok)
print(f"  retrievals issued               {len(ok)} baseline  vs  {len(ok)*K} fan-out")
print(f"  context chars (haystack)        {bc:,} vs {fc:,}   = {fc/bc:.2f}x")
print(f"  retrieval wall, baseline        {sum(r['b_time'] for r in ok):.1f}s total, "
      f"{sum(r['b_time'] for r in ok)/len(ok):.2f}s median-ish per question")
print(f"  retrieval wall, fan-out SERIAL  {sum(r['f_serial'] for r in ok):.1f}s   (the work)")
print(f"  retrieval wall, fan-out PARALLEL{sum(r['f_parallel'] for r in ok):.1f}s   (added latency at N>=K)")
if tot_f > tot_b:
    print(f"  marginal cost                   {(fc-bc)/(tot_f-tot_b):,.0f} added chars per added fact")

print(f"\n=== VERDICT vs the pre-registered bar ===")
if hf >= 4:
    print(f"  {hf}/{hn} — LIVE: question-only decomposition reaches what one query misses.")
elif hf <= 1:
    print(f"  {hf}/{hn} — DEAD: §13 proved reachability, not a route. Acquisition it is.")
else:
    print(f"  {hf}/{hn} — WEAK: reported as weak, no story either way.")
print(f"  total-coverage direction: {tot_f-tot_b:+d} facts "
      f"({'fan-out ahead' if tot_f>tot_b else 'single query ahead' if tot_f<tot_b else 'tied'})")
