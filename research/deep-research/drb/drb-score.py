#!/usr/bin/env python3
"""DRB scorer for the T2b between-arm measurement (order deep-research-t2b).

Stages (pre-registered, pre-registration.md "T2b" section 5-7):

  extract    <- verdict-set.json claims (fact = claim text with
                [Source: ...] tails stripped; url = each citation url;
                one (claim, url) pair per citation)
  dedup      <- identical (normalized fact, url) pairs collapse
  validate   <- vendored validate.py English prompt, reference =
                evidence-window chunk content for the url (fallback:
                fetch-list snippet; else NO reference -> all unknown)
                judge = pinned local model via vendored api client
  stat       <- vendored pooled definition + paper per-task definitions
  CI         <- cluster bootstrap over tasks, 10k resamples, seeded

T3d (order deep-research-t3d, measurement-honesty category):

  - ONE decline implementation (§10.6): DECLINE_SHAPE below is the union
    regex ported VERBATIM from sovereign-desktop e2e lib/classify.mjs
    (the single definition site after the duplicate in
    calibrate-judge.mjs was removed). A fact that itself declines or
    asserts absence is classified 'decline' (honest abstention) WITHOUT
    a judge call — the same deterministic class the declared
    no-reference rule gives 'unknown'. Decline is additive telemetry:
    counts['decline'], fact_rows, the graded projection. It is NEVER
    folded into the fabrication denominator (uns/(s+u)) — excluded
    exactly as 'unknown' is (pre-registration §5).
  - Per-fact verdicts persist to the stat layer (fact_rows, per task,
    in the report). The old instrument dropped them (T3C_AUDIT_FORENSICS
    pass site 6).
  - Zero-pair and zero-judged flights are reported as their own rows in
    every aggregation (zero_pair_flights / zero_judged_flights), never
    silently folded into the pooled mean.
  - Graded-ladder projection (chaos_monkey.rs score_answer vocabulary:
    hallucination / grounded / caveated_ood / honest_abstention /
    answered_novalue) as additive telemetry over the verdict channel;
    caveated_ood / answered_novalue are reported null (they need
    score_answer's critic — the pre-registered 122B graded pass),
    unmeasured, never defaulted (§18.3).
  - --replay re-labels the FROZEN t2b artifacts: numbers copied
    verbatim from the frozen report, per-fact rows reconstructed with
    verdict=null / verdict_recoverable=false (absence reported, never
    defaulted), decline-shape telemetry over the recovered pair text.
    Writes ONLY labeled -t3d.json outputs beside the frozen files.

Usage:
  python3 drb-score.py --arm-dir drb/runs/local --subset drb/query.subset.jsonl \
      --out drb/score-local.json [--arm-name local]
  python3 drb-score.py --replay --arm-dir drb/runs/local \
      --subset drb/query.subset.jsonl \
      --frozen-score demo/demo9/score-local.json \
      --out demo/demo9/score-local-t3d.json [--arm-name local]
  python3 drb-score.py --delta <local.json> <hybrid.json> --delta-out <labeled.json>
  python3 drb-score.py --selftest            # arithmetic unit check
  DRB_JUDGE=mock ...                          # scripted judge (selftest only)

Env (the judge pin, pre-registered): LLM_BACKEND=openai
OPENAI_BASE_URL=http://127.0.0.1:9741/v1 OPENAI_API_KEY=local
FACT_MODEL=Qwen3.6-35B-A3B-MTP-UD-Q6_K
"""
import argparse
import hashlib
import json
import os
import random
import re
import sys
import time
from pathlib import Path

BOOTSTRAP_SEED_STRING = "deep-research-t2b-bootstrap-2026-08-17"
BOOTSTRAP_SEED = int(hashlib.sha256(BOOTSTRAP_SEED_STRING.encode()).hexdigest()[:8], 16)
BOOTSTRAP_N = 10_000
SOURCE_TAIL = re.compile(r"\[Source: [^\]]*\]")
REFERENCE_PRIMARY = 0.1737   # perplexity-Research fabrication (82.63 c.acc.)
REFERENCE_SECONDARY = 0.2499  # openai-deepresearch fabrication (75.01)
COST_PROXY = 1.45            # $/task, o3-deep-research at 50K/20K/15-search mix

# THE one decline-shape implementation (§10.6, order deep-research-t3d,
# 2026-08-17): the union regex ported VERBATIM from
# sovereign/crates/sovereign-desktop/tests/e2e/scripts/lib/classify.mjs
# (the single definition site; calibrate-judge.mjs's local copy was
# removed — the union is a superset of both prior copies, so no
# consumer's behavior changed). One decline shape, one definition site;
# this port is the DRB measurement's copy of that one shape.
DECLINE_SHAPE = re.compile(
    r"\b(don't have|do not have|no (specific|relevant)|not (in|covered by|available in) "
    r"(your|the|my)|couldn't find|could not find|sources? (don't|do not)|cannot provide|"
    r"do(es)? not (contain|cover|include|identify|state)|don'?t have (reliable )?"
    r"(information|this)|not (in|covered by) (your|the|these) (sources|passages|documents)|"
    r"passages do not|no (passage|source|document) (contains|covers)|none of "
    r"(them|these|the \w+) (actually )?(cover|contain|mention|state)|rather not guess|"
    r"couldn'?t find (this|that|it) in)\b",
    re.IGNORECASE)

sys.path.insert(0, str(Path(__file__).parent / "vendor"))

# Judge pin, DEFAULT LOCAL (pre-registration §3): the scorer pins the
# declared default itself rather than relying on caller env; an
# operator override (env already set) always wins.
os.environ.setdefault("LLM_BACKEND", "openai")
os.environ.setdefault("OPENAI_BASE_URL", "http://127.0.0.1:9741/v1")
os.environ.setdefault("FACT_MODEL", "Qwen3.6-35B-A3B-MTP-UD-Q6_K")
if os.environ.get("LLM_BACKEND") == "openai":
    # The vendored client refuses an empty API key, but the local daemon
    # does not validate keys. A placeholder key satisfies the client; no
    # external call is made.
    os.environ.setdefault("OPENAI_API_KEY", "local-daemon")


def strip_claim(text: str) -> str:
    """Named fact normalization: drop [Source: ...] tails and leading
    markdown heading/list markers; collapse blank lines. Deterministic."""
    t = SOURCE_TAIL.sub("", text or "")
    lines = []
    for ln in t.split("\n"):
        ln = re.sub(r"^\s*(#+\s+|[-*+]\s+|\d+\.\s+)", "", ln).strip()
        if ln:
            lines.append(ln)
    return " ".join(lines).strip()


def flight_dir(run_dir: Path) -> Path:
    """The deep-research CLI nests flight artifacts in a timestamped
    dr-*/ subdir under the run-dir. Resolve it (newest by mtime, same
    rule as the driver's manifest_of); fall back to run_dir itself."""
    nested = [p for p in run_dir.glob("dr-*") if p.is_dir()]
    if nested:
        return max(nested, key=lambda p: p.stat().st_mtime)
    return run_dir


def load_evidence_map(run_dir: Path):
    """chunk_id -> (url, content) across evidence-window-N.json rounds."""
    run_dir = flight_dir(run_dir)
    ev = {}
    for p in sorted(run_dir.glob("evidence-window-*.json")):
        try:
            w = json.load(open(p, encoding="utf-8"))
        except Exception:
            continue
        for c in w.get("chunks", []):
            cid = c.get("id") or c.get("chunk_id")
            if cid:
                ev[cid] = (c.get("source_url") or c.get("locator") or "", c.get("content") or "")
    return ev


def load_fetch_fallback(run_dir: Path):
    """url -> snippet from fetch-list-N.json search_hits."""
    run_dir = flight_dir(run_dir)
    fb = {}
    for p in sorted(run_dir.glob("fetch-list-*.json")):
        try:
            f = json.load(open(p, encoding="utf-8"))
        except Exception:
            continue
        for h in f.get("search_hits", []) or f.get("hits", []):
            url = h.get("url")
            if url:
                fb.setdefault(url, h.get("snippet") or "")
    return fb


def load_citation_registry(run_dir: Path):
    """evidence_id -> ordered, deduped url list across draft-N.json
    citations (round order, then registry order). NAMED AMENDMENT 3:
    the pipeline's citation registry lives in the round drafts, not in
    the verdict-set's citations[] (populated on 2/151 claims)."""
    run_dir = flight_dir(run_dir)
    reg = {}
    for p in sorted(run_dir.glob("draft-*.json")):
        try:
            d = json.load(open(p, encoding="utf-8"))
        except Exception:
            continue
        for c in d.get("citations", []) or []:
            eid, url = c.get("evidence_id"), c.get("url")
            if eid and url:
                urls = reg.setdefault(eid, [])
                if url not in urls:
                    urls.append(url)
    return reg


def source_tails(text: str):
    """Distinct [Source: X] ids in order of appearance (Amendment 3)."""
    return list(dict.fromkeys(re.findall(r"\[Source: ([^\]]+)\]", text or "")))


def window_urls(run_dir: Path):
    """The set of evidence-window chunk source urls (Amendment 3's
    window-match set)."""
    urls = set()
    for p in sorted(run_dir.glob("evidence-window-*.json")):
        try:
            w = json.load(open(p, encoding="utf-8"))
        except Exception:
            continue
        for c in w.get("chunks", []):
            u = c.get("source_url") or c.get("locator")
            if u:
                urls.add(u)
    return urls


def load_pairs(run_dir: Path):
    """(fact, url, evidence_id) triples per the pre-registered extract +
    dedup stages, with the NAMED AMENDMENT 3 channel truth: citations[]
    when populated, else the claim's [Source:] tails resolved through
    the round drafts' citation registry (first registry url matching a
    window chunk, else the first registry url — reference falls to the
    declared snippet/unknown chain). The url is the pair's identity."""
    run_dir = flight_dir(run_dir)
    vs = json.load(open(run_dir / "verdict-set.json", encoding="utf-8"))
    reg = load_citation_registry(run_dir)
    wurls = window_urls(run_dir)
    pairs = []
    for c in vs.get("claims", []):
        fact = strip_claim(c.get("text", ""))
        if not fact:
            continue
        cits = c.get("citations", []) or []
        if cits:
            for cit in cits:
                url = cit.get("url")
                if url:
                    pairs.append((fact, url, cit.get("evidence_id")))
            continue
        for tail in source_tails(c.get("text", "")):
            rurls = reg.get(tail, [])
            if not rurls:
                continue
            matched = [u for u in rurls if u in wurls]
            url = (matched or rurls)[0]
            pairs.append((fact, url, None))
    # deterministic dedup: identical (normalized fact, url) collapse
    seen, out = set(), []
    for fact, url, evid in pairs:
        key = (fact.casefold(), url)
        if key not in seen:
            seen.add(key)
            out.append((fact, url, evid))
    return out


def wall_seconds(run_dir: Path):
    run_dir = flight_dir(run_dir)
    try:
        m = json.load(open(run_dir / "manifest.json", encoding="utf-8"))
        lock = m.get("lock", {})
        rel = lock.get("released_at_unix")
        acq = lock.get("acquired_at_unix")
        if rel is not None and acq is not None:
            return max(0, rel - acq)
    except Exception:
        pass
    return None


def cost_usd(wall_s: float) -> float:
    # wall_s * 60 W * $0.15/kWh / (3600 s/h * 1000 Wh/kWh)
    return wall_s * 60.0 * 0.15 / 3_600_000.0


class Judge:
    """The pinned judge: vendored client -> daemon :9741.

    DRB_JUDGE=mock replaces the LLM call with a scripted verdict table
    (used only by --selftest): statements containing 'known-true' are
    supported, 'known-false' unsupported, 'known-unknown' unknown."""

    def __init__(self):
        self.mock = os.environ.get("DRB_JUDGE") == "mock"
        if not self.mock:
            from vendor.utils.api import call_model  # noqa: F401
            self._call_model = call_model

    def _mock(self, prompt: str):
        m = re.search(r"<statements>(.*?)</statements>", prompt, re.S)
        lines = m.group(1).split("\n")
        out = []
        for ln in lines:
            mm = re.match(r"(\d+)\.\s+(.*)", ln.strip())
            if not mm:
                continue
            idx, st = int(mm.group(1)), mm.group(2)
            if "known-true" in st:
                res = "supported"
            elif "known-false" in st:
                res = "unsupported"
            else:
                res = "unknown"
            out.append({"idx": idx, "result": res})
        return json.dumps(out)

    def call(self, prompt: str) -> str:
        if self.mock:
            return self._mock(prompt)
        return self._call_model(prompt)


def validate_url(url, ref, facts, task_id, judge):
    """Vendored validate() semantics: 3 retries, validate_error on
    failure. Returns (results, error)."""
    if ref is None:
        return [], "no reference"
    from vendor.utils.validate import prompt_template_en
    facts_str = "\n".join(f"{i+1}. {f}" for i, f in enumerate(facts))
    user_prompt = prompt_template_en.format(reference=ref, statements=facts_str)
    retries = 0
    error = None
    while retries < 3:
        try:
            response = judge.call(user_prompt)
            res = json.loads(response.replace("```json", "").replace("```", ""))
            for v in res:
                v["idx"] -= 1
            assert len(res) == len(facts)
            return res, None
        except Exception as e:  # noqa: BLE001 - vendored retry semantics
            error = str(e)
            time.sleep(3)
            retries += 1
    return [], error


def score_arm(arm_dir: Path, subset_path: Path, arm_name: str, judge: Judge):
    rows = [json.loads(l) for l in open(subset_path, encoding="utf-8")]
    tasks = []
    for r in rows:
        tid = r["id"]
        run_dir = arm_dir / f"drb-{tid}"
        # verdict-set.json lives in the nested dr-*/ subdir; the guard
        # must resolve flight_dir too, or every real flight scores empty
        fd = flight_dir(run_dir)
        pairs = load_pairs(fd) if (fd / "verdict-set.json").exists() else []
        ev = load_evidence_map(run_dir)
        fb = load_fetch_fallback(run_dir)
        # group pairs by url
        by_url = {}
        for fact, url, evid in pairs:
            by_url.setdefault(url, []).append((fact, evid))
        # resolve references: evidence window by id first, then by url,
        # then the fetch-list snippet fallback, else no reference
        counts = {"supported": 0, "unsupported": 0, "unknown": 0,
                  "decline": 0, "errors": 0}
        per_url = {}
        # task-level per-fact rows, accumulated across urls in pair order
        # (the stat-layer persistence — T3d pass site 6 closure). A list
        # rebuilt per url would hold only the last url's rows and leak
        # the previous task's rows on zero-url tasks — the selftest
        # caught exactly that shape.
        fact_rows = []
        for url, fact_evids in by_url.items():
            facts = [f for f, _ in fact_evids]
            # T3d mechanical decline intercept (§10.6, ONE decline shape):
            # a fact that itself declines or asserts absence is classified
            # 'decline' (honest abstention) WITHOUT a judge call — the
            # same deterministic class the declared no-reference rule
            # gives 'unknown'. Additive telemetry only: 'decline' is
            # NEVER folded into the fabrication denominator uns/(s+u),
            # exactly as 'unknown' is excluded (pre-registration §5).
            judge_evids = [(f, e) for f, e in fact_evids
                           if not DECLINE_SHAPE.search(f)]
            counts["decline"] += len(fact_evids) - len(judge_evids)
            ref = None
            for _f, evid in judge_evids:
                if evid and evid in ev:
                    ref = ev[evid][1]
                    break
            if ref is None:
                for cid, (u, content) in ev.items():
                    if u == url:
                        ref = content
                        break
            if ref is None and url in fb:
                ref = fb[url]
            results = []  # aligned with judge_evids order
            if judge_evids:
                if ref is None:
                    # Declaration §5: NO reference -> all remaining
                    # statements for this url judged unknown (official
                    # no-valid-content rule) — NOT a validate_error.
                    # Deterministic, no judge.
                    counts["unknown"] += len(judge_evids)
                    results = [{"result": "unknown", "mechanism": "no-reference"}
                               for _ in judge_evids]
                else:
                    jfacts = [f for f, _ in judge_evids]
                    res, err = validate_url(url, ref, jfacts, tid, judge)
                    if err is not None:
                        counts["errors"] += len(jfacts)
                        results = [{"result": None, "error": err}
                                   for _ in judge_evids]
                    else:
                        vmap = {"supported": 0, "unsupported": 0, "unknown": 0,
                                "decline": 0}
                        for v in res:
                            vmap[v["result"]] = vmap.get(v["result"], 0) + 1
                        for k in vmap:
                            counts[k] += vmap[k]
                        results = [{"result": v["result"]} for v in res]
            # T3d pass site 6 closure: per-fact rows persist to the stat
            # layer in ORIGINAL pair order — never dropped. idx is the
            # 1-based pair position (the fact's identity in this flight).
            url_rows = []
            ji = 0
            for i, (fact, evid) in enumerate(fact_evids):
                if DECLINE_SHAPE.search(fact):
                    url_rows.append({
                        "idx": i + 1, "fact": fact, "url": url,
                        "evidence_id": evid, "verdict": "decline",
                        "mechanism": "decline-shape-intercept",
                    })
                else:
                    r = results[ji]
                    ji += 1
                    row = {"idx": i + 1, "fact": fact, "url": url,
                           "evidence_id": evid, "verdict": r["result"]}
                    if "error" in r:
                        row["error"] = r["error"]
                    if "mechanism" in r:
                        row["mechanism"] = r["mechanism"]
                    url_rows.append(row)
            fact_rows.extend(url_rows)
            per_url[url] = {
                "facts": facts, "fact_rows": url_rows, "error": None,
                "results": [
                    {"result": fr["verdict"], "idx": fr["idx"]}
                    for fr in fact_rows
                ],
            }
        wall = wall_seconds(run_dir)
        s = counts["supported"]
        u = counts["unsupported"]
        tasks.append({
            "id": tid,
            "topic": r.get("topic"),
            "prompt": r.get("prompt"),
            "pairs": len(pairs),
            "counts": counts,
            # T3d: zero-pair (pairs==0) and zero-judged (s+u==0) flights
            # are named rows in every aggregation — never silently folded
            # into the pooled mean (the paper mean folds each at fab=1.0
            # per the declared drop rule; that contribution is now named).
            "zero_pair": len(pairs) == 0,
            "zero_judged": s + u == 0,
            "fact_rows": fact_rows,
            "wall_s": wall,
            "cost_usd": cost_usd(wall) if wall is not None else None,
            "per_url": per_url,
        })
    return tasks


def pooled_fabrication(tasks):
    sup = sum(t["counts"]["supported"] for t in tasks)
    uns = sum(t["counts"]["unsupported"] for t in tasks)
    if sup + uns == 0:
        return None
    return uns / (sup + uns)


def paper_mean_fabrication(tasks):
    """Paper Eq 4-5 mirrored: fabrication_t = 1 - Acc_t; N_u=0 -> 1."""
    accs = []
    for t in tasks:
        sup, uns = t["counts"]["supported"], t["counts"]["unsupported"]
        n = sup + uns
        accs.append(sup / n if n > 0 else 0.0)
    return 1.0 - (sum(accs) / len(accs))


def cluster_bootstrap(tasks, n=BOOTSTRAP_N, seed=BOOTSTRAP_SEED):
    """Resample tasks (clusters), pooled fabrication per resample; returns
    (rates, dropped). Rates: None entries for undefined resamples."""
    rng = random.Random(seed)
    per_task = [
        (t["counts"]["supported"], t["counts"]["unsupported"]) for t in tasks
    ]
    rates, dropped = [], 0
    for _ in range(n):
        s = u = 0
        for _t in range(len(per_task)):
            sup, uns = per_task[rng.randrange(len(per_task))]
            s += sup
            u += uns
        if s + u == 0:
            dropped += 1
            continue
        rates.append(u / (s + u))
    rates.sort()
    return rates, dropped


def ci95(rates):
    if not rates:
        return None, None
    lo = rates[int(0.025 * len(rates))]
    hi = rates[int(0.975 * len(rates)) - 1]
    return lo, hi


def verdict(lo, hi, ref):
    """Four verdicts vs the fixed reference line."""
    if lo is None:
        return "could-not-judge"
    if hi < ref:
        return "met"
    if lo >= ref:
        return "failed"
    return "could-not-judge"


def delta_ci(local_score: dict, hybrid_score: dict, n=BOOTSTRAP_N,
             seed=BOOTSTRAP_SEED):
    """Paired cluster bootstrap on the between-arm delta
    (hybrid_pooled - local_pooled). Tasks are resampled jointly; each
    resample recomputes both arms' pooled fabrication over the same task
    draw. Returns (deltas_sorted, dropped)."""
    lt = [(t["counts"]["supported"], t["counts"]["unsupported"])
          for t in local_score["tasks"]]
    ht = [(t["counts"]["supported"], t["counts"]["unsupported"])
          for t in hybrid_score["tasks"]]
    assert len(lt) == len(ht) == 10
    rng = random.Random(seed)
    deltas, dropped = [], 0
    for _ in range(n):
        ls = lu = hs = hu = 0
        for _t in range(10):
            i = rng.randrange(10)
            s, u = lt[i]
            ls += s; lu += u
            s, u = ht[i]
            hs += s; hu += u
        if ls + lu == 0 or hs + hu == 0:
            dropped += 1
            continue
        # fabrication = unsupported/(s+u); delta = hybrid_fab - local_fab
        deltas.append(hu / (hs + hu) - lu / (ls + lu))
    deltas.sort()
    return deltas, dropped


def task_graded(t):
    """One task's verdict channel projected onto the chaos graded ladder
    (sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs score_answer)."""
    c = t["counts"]
    return {
        "grounded": c.get("supported", 0),
        "hallucination": c.get("unsupported", 0),
        "honest_abstention": c.get("decline", 0),
        "unclassified": c.get("unknown", 0),
    }


def graded_telemetry(tasks, replay=False):
    """Additive telemetry (T3d): the verdict channel projected onto the
    chaos graded ladder. Deterministic projection of what the verdict
    channel already names; caveated_ood and answered_novalue require
    score_answer's critic (the pre-registered 122B graded pass) and are
    reported null — unmeasured, never defaulted (§18.3)."""
    g = {"grounded": 0, "hallucination": 0, "honest_abstention": 0, "unclassified": 0}
    for t in tasks:
        g["grounded"] += t["counts"].get("supported", 0)
        g["hallucination"] += t["counts"].get("unsupported", 0)
        if replay:
            # honest_abstention is the mechanical DECLINE_SHAPE count over
            # the recovered pair text: the old instrument had no decline
            # class, so judge-emittable declines are unrecoverable from
            # the frozen artifacts (T3C_AUDIT_FORENSICS pass site 4).
            g["honest_abstention"] += t.get("decline_shape_count", 0)
        else:
            g["honest_abstention"] += t["counts"].get("decline", 0)
        g["unclassified"] += t["counts"].get("unknown", 0)
    return {
        "grounded": g["grounded"],
        "hallucination": g["hallucination"],
        "honest_abstention": g["honest_abstention"],
        "unclassified_unknown": g["unclassified"],
        "caveated_ood": None,
        "answered_novalue": None,
        "ladder_notes": (
            "projection of the verdict channel onto the chaos graded ladder "
            "(chaos_monkey.rs score_answer). caveated_ood / answered_novalue "
            "need score_answer's critic — the pre-registered 122B graded "
            "pass — and are null here (unmeasured, never defaulted, §18.3)."
            + (" honest_abstention is the mechanical DECLINE_SHAPE count "
               "over the recovered pair text: the old instrument had no "
               "decline class, so judge-emittable declines are "
               "unrecoverable from the frozen artifacts "
               "(T3C_AUDIT_FORENSICS.md pass site 4)." if replay else "")),
    }


def abstention_dimension(tasks, replay=False):
    """T3d gap #4: the abstention dimension is nameable — honest
    abstention (decline) is its own class; the remaining unknown mass is
    unclassified, reported as-is, never collapsed into fabrication."""
    if replay:
        d = sum(t.get("decline_shape_count", 0) for t in tasks)
    else:
        d = sum(t["counts"].get("decline", 0) for t in tasks)
    u = sum(t["counts"].get("unknown", 0) for t in tasks)
    denom = d + u
    return {
        "honest_abstention": d,
        "unclassified_unknown": u,
        "abstention_share_of_unknown": (d / denom) if denom else None,
        "note": "decline (honest abstention) is its own verdict class "
                "(order deep-research-t3d); the remaining unknown mass is "
                "unclassified — never abstention-scored by any instrument "
                "(T3C_AUDIT_FORENSICS.md pass site 3) — and is reported "
                "as-is, never collapsed into fabrication.",
    }


def zero_pair_block(tasks):
    """T3d gap #3: zero-pair and zero-judged flights are their own rows
    in every aggregation — never silently folded into the pooled mean."""
    zp = [t["id"] for t in tasks if t.get("zero_pair")]
    zj = [t["id"] for t in tasks if t.get("zero_judged")]
    return {
        "zero_pair_flights": {
            "count": len(zp),
            "ids": zp,
            "contribution": "pairs=0 -> per the declared drop rule each "
                            "contributes fab=1.0 to the paper mean "
                            "(pre-registration T2b, 'Local 62/90/95'); the "
                            "pooled mean is unaffected (they carry no "
                            "pairs). Reported as their own rows — never "
                            "silently folded into the pooled mean (order "
                            "deep-research-t3d).",
        },
        "zero_judged_flights": {
            "count": len(zj),
            "ids": zj,
            "contribution": "pairs>0 but supported+unsupported == 0 -> "
                            "N_u=0 -> Acc=0 -> fab=1.0 in the paper mean, "
                            "the declared N_u=0->1 rule (pre-registration "
                            "line 2226-2237).",
        },
    }


def delta_mode(local_path: str, hybrid_path: str, out_path: str = None):
    """Descriptive between-arm delta (pre-registration §6): hybrid - local
    pooled fabrication, same seeded cluster bootstrap. Reported, never a
    gate. Writes ONLY a labeled output — an explicit --delta-out, else
    the derived <hybrid stem>-delta.json — and REFUSES to overwrite an
    existing file not explicitly named (the frozen
    demo/demo9/score-hybrid-delta.json must never be clobbered, order
    deep-research-t3d)."""
    with open(local_path, encoding="utf-8") as f:
        local_score = json.load(f)
    with open(hybrid_path, encoding="utf-8") as f:
        hybrid_score = json.load(f)
    deltas, dropped = delta_ci(local_score, hybrid_score)
    lo, hi = ci95(deltas)
    observed = hybrid_score["pooled_fabrication"] - local_score["pooled_fabrication"]
    # three-way read vs zero, descriptive only
    direction = "met" if hi < 0.0 else ("failed" if lo >= 0.0 else "could-not-judge")
    out = {
        "delta": "hybrid - local",
        "pooled_delta": observed,
        "local_pooled": local_score["pooled_fabrication"],
        "hybrid_pooled": hybrid_score["pooled_fabrication"],
        "bootstrap": {
            "n_resamples": BOOTSTRAP_N,
            "seed": BOOTSTRAP_SEED,
            "dropped_undefined": dropped,
            "ci95_lower": lo,
            "ci95_upper": hi,
        },
        "descriptive_verdict": direction,
        # T3d: the delta read names the flight shapes it is read against
        "zero_pair_flights": {
            "local": local_score.get("zero_pair_flights", {}).get("ids"),
            "hybrid": hybrid_score.get("zero_pair_flights", {}).get("ids"),
        },
        "zero_judged_flights": {
            "local": local_score.get("zero_judged_flights", {}).get("ids"),
            "hybrid": hybrid_score.get("zero_judged_flights", {}).get("ids"),
        },
        "abstention_dimension": {
            "local": local_score.get("abstention_dimension"),
            "hybrid": hybrid_score.get("abstention_dimension"),
        },
    }
    derived = Path(hybrid_path).with_name(Path(hybrid_path).stem + "-delta.json")
    if out_path is None:
        out_path = derived
    out_path = Path(out_path)
    if out_path.exists() and str(out_path) == str(derived):
        print(f"refusing to overwrite {out_path} (the frozen delta "
              f"artifact, order deep-research-t3d); pass --delta-out to "
              f"name a labeled output")
        return 2
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1)
    print(f"between-arm delta hybrid-local: {observed:+.4f} "
          f"[{lo:.4f}, {hi:.4f}] (dropped={dropped}/{BOOTSTRAP_N}) "
          f"-> {direction.upper()} (descriptive)")
    print(f"wrote {out_path}")
    return 0


def replay_mode(arm_dir: Path, subset_path: str, arm_name: str,
                frozen_path: str, out: str) -> int:
    """T3d: re-label the FROZEN t2b artifacts. The frozen score report's
    numbers are copied VERBATIM (same scorer, same run — nothing
    recomputed); per-fact rows are reconstructed from the frozen arm
    dirs with verdict=null and verdict_recoverable=false, because the
    old instrument dropped the per-fact judge verdicts at stat time
    (T3C_AUDIT_FORENSICS.md pass site 6). Absence is reported, never
    defaulted (§18.3). Writes ONLY the labeled -t3d.json output; the
    frozen files are never written."""
    t0 = time.time()
    frozen = json.load(open(frozen_path, encoding="utf-8"))
    rows = [json.loads(l) for l in open(subset_path, encoding="utf-8")]
    tasks = []
    for r in rows:
        tid = r["id"]
        ft = next((t for t in frozen["tasks"] if t["id"] == tid), None)
        if ft is None:
            print(f"replay: task {tid} not found in frozen {frozen_path} — "
                  f"aborting (nothing written)")
            return 2
        run_dir = arm_dir / f"drb-{tid}"
        fd = flight_dir(run_dir)
        pairs = []
        pairs_recovered = False
        if (fd / "verdict-set.json").exists():
            pairs = load_pairs(fd)
            pairs_recovered = True
        fact_rows = [{
            "idx": i + 1, "fact": fact, "url": url, "evidence_id": evid,
            "verdict": None, "verdict_recoverable": False,
            "decline_shape": bool(DECLINE_SHAPE.search(fact)),
        } for i, (fact, url, evid) in enumerate(pairs)]
        c = ft["counts"]
        tasks.append({
            "id": tid,
            "topic": ft.get("topic"),
            "prompt": r.get("prompt"),
            "pairs": ft.get("pairs"),
            "counts": dict(c),
            "zero_pair": bool(ft.get("pairs", 0) == 0),
            "zero_judged": bool(c.get("supported", 0) + c.get("unsupported", 0) == 0),
            "fact_rows": fact_rows,
            "pairs_recovered": pairs_recovered,
            "decline_shape_count": sum(1 for x in fact_rows if x["decline_shape"]),
            "wall_s": ft.get("wall_s"),
            "cost_usd": ft.get("cost_usd"),
        })
    report = {
        "arm": arm_name,
        "subset": subset_path,
        "judge_pin": frozen.get("judge_pin", "(frozen)"),
        "n_tasks": len(tasks),
        "replay": {
            "label": "t3d",
            "source": str(frozen_path),
            "numbers": "copied verbatim from the frozen old-instrument "
                       "report (same scorer, same run — nothing recomputed)",
            "verdict_recoverable": False,
            "verdict_recovery_note": (
                "per-fact judge verdicts were dropped at stat time by the "
                "old instrument (T3C_AUDIT_FORENSICS.md pass site 6); the "
                "35B verdicts are not recoverable from the frozen artifacts "
                "without re-judging. Per-fact rows below reconstruct fact "
                "text + url from the frozen arm dirs with verdict=null — "
                "absence reported, never defaulted (§18.3)."),
            "decline_shape_telemetry": {
                "count": sum(t["decline_shape_count"] for t in tasks),
                "pairs_scanned": sum(len(t["fact_rows"]) for t in tasks),
                "basis": "DECLINE_SHAPE (the ONE decline implementation, "
                         "§10.6 — the union of the two copies, ported "
                         "verbatim from sovereign-desktop e2e "
                         "lib/classify.mjs) applied to the recovered paired "
                         "fact text. The forensics' pass-site-4 count (7) is "
                         "not mechanically reproducible by any single basis "
                         "(union regex on paired facts: 3; raw claim text: "
                         "15; the gate's answer_declines zoo: 9); this "
                         "instrument's count supersedes it — a named "
                         "substitution (§18.3, pre-registration.md T3d).",
            },
        },
        "tasks": [
            {k: t[k] for k in ("id", "topic", "pairs", "counts", "zero_pair",
                               "zero_judged", "fact_rows", "pairs_recovered",
                               "decline_shape_count", "wall_s", "cost_usd")}
            for t in tasks
        ],
        "pooled_fabrication": frozen.get("pooled_fabrication"),
        "paper_mean_fabrication": frozen.get("paper_mean_fabrication"),
        "pooled_citation_accuracy": frozen.get("pooled_citation_accuracy"),
        "bootstrap": frozen.get("bootstrap"),
        "references": frozen.get("references"),
        "verdict_primary": frozen.get("verdict_primary"),
        "mean_cost_usd": frozen.get("mean_cost_usd"),
        "cost_proxy_usd": COST_PROXY,
        **zero_pair_block(tasks),
        "graded_telemetry": graded_telemetry(tasks, replay=True),
        "abstention_dimension": abstention_dimension(tasks, replay=True),
        "elapsed_s": round(time.time() - t0, 1),
    }
    with open(out, "w", encoding="utf-8") as f:
        json.dump(report, f, indent=1, ensure_ascii=False)
    print(f"=== replay {arm_name} arm (n={len(tasks)}, numbers verbatim "
          f"from {frozen_path}) ===")
    for t in report["tasks"]:
        c = t["counts"]
        n = c["supported"] + c["unsupported"]
        fab = c["unsupported"] / n if n else 1.0
        flags = (" zero-pair" if t["zero_pair"] else "") + \
                (" zero-judged" if t["zero_judged"] else "")
        print(f"  task {t['id']:>3} {str(t['topic'])[:34]:<36} pairs={t['pairs']:>3} "
              f"s={c['supported']} u={c['unsupported']} ?={c['unknown']} "
              f"e={c['errors']} fab={fab:.3f}{flags} wall={t['wall_s']}s")
    zp = zero_pair_block(tasks)
    print(f"zero-pair flights: {zp['zero_pair_flights']['count']} "
          f"{zp['zero_pair_flights']['ids']} (each fab=1.0 in the paper "
          f"mean per the declared drop rule; pooled unaffected — reported "
          f"as their own rows, never folded into the pooled mean)")
    print(f"zero-judged flights (s+u==0): {zp['zero_judged_flights']['count']} "
          f"{zp['zero_judged_flights']['ids']} (N_u=0 -> fab=1.0, the "
          f"declared rule)")
    ad = abstention_dimension(tasks, replay=True)
    print(f"abstention dimension: honest_abstention={ad['honest_abstention']} "
          f"(mechanical decline-shape) unclassified_unknown="
          f"{ad['unclassified_unknown']} "
          f"share_of_unknown={ad['abstention_share_of_unknown']}")
    print(f"pooled fabrication: {report['pooled_fabrication']}")
    print(f"paper-mean fabrication (Eq 4-5): {report['paper_mean_fabrication']}")
    b = report["bootstrap"] or {}
    if b.get("ci95_lower") is not None:
        print(f"cluster-bootstrap 95% CI: [{b['ci95_lower']:.4f}, "
              f"{b['ci95_upper']:.4f}]  "
              f"(dropped={b.get('dropped_undefined')}/{BOOTSTRAP_N})")
    print(f"verdict_primary (frozen): {report['verdict_primary']}")
    print(f"wrote {out}")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm-dir")
    ap.add_argument("--subset", default=str(Path(__file__).parent / "query.subset.jsonl"))
    ap.add_argument("--out")
    ap.add_argument("--arm-name", default="arm")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--delta", nargs=2, metavar=("LOCAL_JSON", "HYBRID_JSON"),
                    help="paired cluster bootstrap on hybrid - local")
    ap.add_argument("--delta-out",
                    help="labeled output for --delta (refuses to clobber "
                         "the frozen score-hybrid-delta.json)")
    ap.add_argument("--replay", action="store_true",
                    help="re-label a FROZEN score file: numbers verbatim, "
                         "per-fact rows reconstructed (verdict=null, "
                         "verdict_recoverable=false), labeled -t3d output")
    ap.add_argument("--frozen-score",
                    help="the frozen score-*.json --replay re-labels")
    args = ap.parse_args()

    if args.selftest:
        sys.exit(selftest())
    if args.delta:
        sys.exit(delta_mode(args.delta[0], args.delta[1], args.delta_out))
    if args.replay:
        if not args.frozen_score or not args.out:
            print("--replay requires --frozen-score <frozen score json> and "
                  "--out <labeled output, e.g. demo/demo9/score-local-t3d.json>")
            return 2
        sys.exit(replay_mode(Path(args.arm_dir), args.subset, args.arm_name,
                             args.frozen_score, args.out))

    judge = Judge()
    t0 = time.time()
    tasks = score_arm(Path(args.arm_dir), args.subset, args.arm_name, judge)
    pooled = pooled_fabrication(tasks)
    paper = paper_mean_fabrication(tasks)
    rates, dropped = cluster_bootstrap(tasks)
    lo, hi = ci95(rates)
    costs = [t["cost_usd"] for t in tasks if t["cost_usd"] is not None]
    mean_cost = sum(costs) / len(costs) if costs else None
    verdict_primary = verdict(lo, hi, REFERENCE_PRIMARY)
    report = {
        "arm": args.arm_name,
        "subset": args.subset,
        "judge_pin": os.environ.get("FACT_MODEL", "(unset)"),
        "n_tasks": len(tasks),
        "tasks": [
            {
                **{k: t[k] for k in ("id", "topic", "pairs", "counts",
                                     "wall_s", "cost_usd")},
                "zero_pair": t["zero_pair"],
                "zero_judged": t["zero_judged"],
                "graded_telemetry": task_graded(t),
                "fact_rows": t["fact_rows"],
            }
            for t in tasks
        ],
        "pooled_fabrication": pooled,
        "paper_mean_fabrication": paper,
        "pooled_citation_accuracy": (1.0 - pooled) if pooled is not None else None,
        "bootstrap": {
            "n_resamples": BOOTSTRAP_N,
            "seed": BOOTSTRAP_SEED,
            "dropped_undefined": dropped,
            "ci95_lower": lo,
            "ci95_upper": hi,
        },
        "references": {
            "primary": {"agent": "perplexity-Research", "fabrication": REFERENCE_PRIMARY},
            "secondary": {"agent": "openai-deepresearch", "fabrication": REFERENCE_SECONDARY},
        },
        "verdict_primary": verdict_primary,
        "mean_cost_usd": mean_cost,
        "cost_proxy_usd": COST_PROXY,
        **zero_pair_block(tasks),
        "graded_telemetry": graded_telemetry(tasks),
        "abstention_dimension": abstention_dimension(tasks),
        "elapsed_s": round(time.time() - t0, 1),
    }
    out = args.out or f"score-{args.arm_name}.json"
    with open(out, "w", encoding="utf-8") as f:
        json.dump(report, f, indent=1, ensure_ascii=False)
    print(f"=== {args.arm_name} arm (n={len(tasks)}) ===")
    for t in report["tasks"]:
        c = t["counts"]
        n = c["supported"] + c["unsupported"]
        fab = c["unsupported"] / n if n else 1.0
        flags = (" zero-pair" if t["zero_pair"] else "") + \
                (" zero-judged" if t["zero_judged"] else "")
        print(f"  task {t['id']:>3} {t['topic'][:34]:<36} pairs={t['pairs']:>3} "
              f"s={c['supported']} u={c['unsupported']} ?={c['unknown']} "
              f"d={c['decline']} e={c['errors']} fab={fab:.3f}{flags} "
              f"wall={t['wall_s']}s")
    print(f"pooled fabrication: {pooled if pooled is not None else 'n/a'}")
    print(f"paper-mean fabrication (Eq 4-5): {paper:.4f}")
    print(f"citation accuracy (pooled): {(1.0-pooled) if pooled is not None else 'n/a'}")
    if lo is None:
        print("cluster-bootstrap 95% CI: n/a (no judged pairs)")
    else:
        print(f"cluster-bootstrap 95% CI: [{lo:.4f}, {hi:.4f}]  "
              f"(dropped={dropped}/{BOOTSTRAP_N})")
    print(f"vs primary ref {REFERENCE_PRIMARY} -> {verdict_primary.upper()}")
    print(f"mean cost/task: {mean_cost if mean_cost is not None else 'n/a'} "
          f"vs proxy {COST_PROXY} -> "
          f"{'met' if mean_cost is not None and mean_cost < COST_PROXY else 'n/a'}")
    zp = zero_pair_block(tasks)
    print(f"zero-pair flights: {zp['zero_pair_flights']['count']} "
          f"{zp['zero_pair_flights']['ids']} (each fab=1.0 in the paper "
          f"mean per the declared drop rule; pooled unaffected — reported "
          f"as their own rows, never folded into the pooled mean)")
    print(f"zero-judged flights (s+u==0): {zp['zero_judged_flights']['count']} "
          f"{zp['zero_judged_flights']['ids']} (N_u=0 -> fab=1.0, the "
          f"declared rule)")
    ad = abstention_dimension(tasks)
    print(f"abstention dimension: honest_abstention={ad['honest_abstention']} "
          f"unclassified_unknown={ad['unclassified_unknown']} "
          f"share_of_unknown={ad['abstention_share_of_unknown']}")
    print(f"wrote {out}")


def selftest():
    """Known pairs -> known pooled rate, paper mean, CI, verdict. The mock
    judge scripts verdicts by marker words; the bootstrap is seeded, so
    the CI is exact. T3d extensions: a decline-shaped fact is intercepted
    mechanically (no judge call, counts['decline']), a zero-pair flight
    (203) and a zero-judged flight (204) get their own named rows."""
    import tempfile
    tmp = Path(tempfile.mkdtemp(prefix="drb-selftest-"))
    subset = tmp / "subset.jsonl"
    with open(subset, "w", encoding="utf-8") as f:
        for i in range(2):
            f.write(json.dumps({"id": 100 + i, "topic": f"t{i}", "language": "en",
                                "prompt": f"p{i}"}) + "\n")
        # task 202 exercises the Amendment 3 channel: claims WITHOUT
        # citations[] — pairs come from [Source:] tails resolved through
        # the draft registry.
        f.write(json.dumps({"id": 202, "topic": "t2", "language": "en",
                            "prompt": "p2"}) + "\n")
        f.write(json.dumps({"id": 203, "topic": "t3", "language": "en",
                            "prompt": "p3"}) + "\n")
        f.write(json.dumps({"id": 204, "topic": "t4", "language": "en",
                            "prompt": "p4"}) + "\n")
    # task 100: 2 judged pairs (1 supported, 1 unsupported -> fab .5)
    # plus a decline-shaped fact ('does not include' -> DECLINE_SHAPE)
    # that is intercepted BEFORE any judge call: counts['decline'] += 1
    # and the judge prompt never sees it (mock would say 'unknown').
    # Reference resolves by EVIDENCE ID (window chunk id == evidence_id)
    # task 101: 1 pair -> 1 supported (fab 0); plus 1 unknown pair;
    # reference resolves by URL (chunk source_url == url)
    for tid, claims in ((100, [
        ("known-true one", "http://a/1", "ref-a", "ev-0"),
        ("known-false one", "http://a/1", "ref-a", "ev-1"),  # same url -> same batch
        ("the document does not include the exact figure", "http://a/1", "ref-a", "ev-2"),
    ]), (101, [
        ("known-true two", "http://b/1", "ref-b", "http://b/1"),
        ("noinfo two", "http://b/2", "ref-b2", "http://b/2"),
    ])):
        run = tmp / f"drb-{tid}"
        run.mkdir()
        if tid == 101:
            # task 101 uses the REAL nested layout (dr-<ts>/ subdir) —
            # flight_dir resolution must not change the score
            run = run / "dr-7777"
            run.mkdir()
        vs = {"claims": []}
        for i, (text, url, content, cid) in enumerate(claims):
            vs["claims"].append({
                "id": f"c{i}", "text": text, "verdict": "passed",
                "evidence_ids": [f"ev-{i}"], "citations": [
                    {"evidence_id": f"ev-{i}", "url": url, "chunk_id": cid}],
            })
        json.dump(vs, open(run / "verdict-set.json", "w"))
        w = {"chunks": [
            {"id": cid, "source_url": url, "content": content}
            for _, url, content, cid in claims]}
        json.dump(w, open(run / "evidence-window-1.json", "w"))
        m = {"lock": {"acquired_at_unix": 1000, "released_at_unix": 1360},
             "terminal_state": "done", "consent": {"release-floor": "personal"}}
        json.dump(m, open(run / "manifest.json", "w"))
    # task 202 (Amendment 3): c0 tail estate-7 -> registry -> window-
    # matched url -> supported; c1 tail estate-8 -> registry url NOT in
    # window -> no reference -> unknown; c2 tail estate-9 -> no registry
    # entry -> dropped (official no-ref rule).
    run = tmp / "drb-202"
    run.mkdir()
    vs = {"claims": [
        {"id": "c0", "text": "known-true three [Source: estate-7]",
         "verdict": "passed", "evidence_ids": [], "citations": []},
        {"id": "c1", "text": "known-false three [Source: estate-8]",
         "verdict": "failed", "evidence_ids": [], "citations": []},
        {"id": "c2", "text": "known-true four [Source: estate-9]",
         "verdict": "passed", "evidence_ids": [], "citations": []},
    ]}
    json.dump(vs, open(run / "verdict-set.json", "w"))
    json.dump({"citations": [
        {"evidence_id": "estate-7", "url": "http://c/7", "custody": "personal"},
        {"evidence_id": "estate-8", "url": "http://c/8", "custody": "personal"},
    ]}, open(run / "draft-1.json", "w"))
    json.dump({"chunks": [
        {"id": "ev-7", "source_url": "http://c/7", "content": "ref-c7"},
    ]}, open(run / "evidence-window-1.json", "w"))
    json.dump({"lock": {"acquired_at_unix": 1000, "released_at_unix": 1360},
               "terminal_state": "done", "consent": {"release-floor": "personal"}},
              open(run / "manifest.json", "w"))
    # task 203 (T3d): a zero-PAIR flight — claims carry no citations and
    # no [Source:] tails -> the extract+dedup stages yield zero pairs.
    run = tmp / "drb-203"
    run.mkdir()
    json.dump({"claims": [
        {"id": "c0", "text": "known-true zero", "verdict": "passed",
         "evidence_ids": [], "citations": []},
    ]}, open(run / "verdict-set.json", "w"))
    # task 204 (T3d): a zero-JUDGED flight — the pair exists but no
    # reference resolves (evidence_id absent, url absent from the window,
    # no fetch fallback) -> all unknown -> s+u == 0 -> zero_judged, and
    # fab=1.0 in the paper mean under the declared N_u=0->1 rule (NOT
    # zero-pair: the pair exists).
    run = tmp / "drb-204"
    run.mkdir()
    json.dump({"claims": [
        {"id": "c0", "text": "noinfo zero", "verdict": "passed",
         "evidence_ids": ["ev-z"], "citations": [
             {"evidence_id": "ev-z", "url": "http://z/9", "chunk_id": "ev-z"}]},
    ]}, open(run / "verdict-set.json", "w"))
    json.dump({"chunks": [
        {"id": "other-id", "source_url": "http://other/9", "content": "ref-z9"},
    ]}, open(run / "evidence-window-1.json", "w"))
    os.environ["DRB_JUDGE"] = "mock"
    judge = Judge()
    tasks = score_arm(tmp, subset, "selftest", judge)
    pooled = pooled_fabrication(tasks)
    paper = paper_mean_fabrication(tasks)
    rates, dropped = cluster_bootstrap(tasks)
    lo, hi = ci95(rates)
    costs = [t["cost_usd"] for t in tasks]
    # expected: totals supported 3, unsupported 1, decline 1, unknown 3
    # (101:1, 202:1, 204:1), errors 0.
    # pooled = 1/(3+1) = 0.25 — decline excluded from the denominator
    # exactly as unknown is (pre-registration §5).
    # paper mean = 1 - (0.5 + 1.0 + 1.0 + 0.0 + 0.0)/5 = 0.5 — tasks 203
    # and 204 hit N_u=0 -> Acc=0 -> fab=1.0, the declared rule.
    # wall 360s -> 360*60*0.15/3.6e6 = $0.0009; CI deterministic given seed.
    checks = [
        ("pooled", abs(pooled - 0.25) < 1e-12),
        ("paper mean", abs(paper - 0.5) < 1e-12),
        ("task0 counts", tasks[0]["counts"] == {"supported": 1, "unsupported": 1,
                                                "unknown": 0, "decline": 1,
                                                "errors": 0}),
        ("task1 counts", tasks[1]["counts"] == {"supported": 1, "unsupported": 0,
                                                "unknown": 1, "decline": 0,
                                                "errors": 0}),
        ("task202 counts", tasks[2]["counts"] == {"supported": 1, "unsupported": 0,
                                                  "unknown": 1, "decline": 0,
                                                  "errors": 0}),
        ("task203 zero-pair", tasks[3]["zero_pair"] is True
                              and tasks[3]["zero_judged"] is True
                              and tasks[3]["counts"] == {"supported": 0, "unsupported": 0,
                                                         "unknown": 0, "decline": 0,
                                                         "errors": 0}),
        ("task204 zero-judged", tasks[4]["zero_judged"] is True
                                and tasks[4]["zero_pair"] is False
                                and tasks[4]["counts"] == {"supported": 0, "unsupported": 0,
                                                           "unknown": 1, "decline": 0,
                                                           "errors": 0}),
        ("decline intercept", any(
            fr["verdict"] == "decline" and fr["mechanism"] == "decline-shape-intercept"
            for fr in tasks[0]["fact_rows"])),
        ("fact_rows persisted", sum(len(t["fact_rows"]) for t in tasks) == 8),
        ("wall", tasks[0]["wall_s"] == 360),
        ("cost", abs(costs[0] - 0.0009) < 1e-12),
        ("bootstrap dropped", dropped == BOOTSTRAP_DROPPED_SELFTEST),
    ]
    # exact CI from the seeded bootstrap: pooled over resampled tasks;
    # per_task = [(1,1),(1,0),(1,0),(0,0),(0,0)] — a resample is
    # undefined only when all 5 picks land on the two zero tasks
    # ((2/5)^5 = 0.01024 of resamples); the exact count is deterministic
    # given the seed (pinned below from the seeded run).
    ok = all(ok for _, ok in checks) and lo is not None and hi is not None
    print("selftest checks:")
    for name, passed in checks:
        print(f"  {name}: {'PASS' if passed else 'FAIL'}")
    print(f"  ci95 [{lo:.4f}, {hi:.4f}] verdict-vs-primary: {verdict(lo, hi, REFERENCE_PRIMARY)}")
    # the CI must be within [0, 1] and lo <= pooled <= hi
    ok = ok and 0.0 <= lo <= hi <= 1.0 and lo <= pooled <= hi
    print("SELFTEST:", "PASS" if ok else "FAIL")
    return 0 if ok else 1


# Pinned from the seeded selftest run (5-task universe, 2 zero tasks):
# exact dropped count for BOOTSTRAP_N=10k resamples, seed BOOTSTRAP_SEED.
# Theoretical expectation: (2/5)^5 = 0.01024 -> ~102.4; measured 123.
BOOTSTRAP_DROPPED_SELFTEST = 123


if __name__ == "__main__":
    main()
