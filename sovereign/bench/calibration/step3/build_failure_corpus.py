#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""D2 — the Step 3 failure corpus, derived from committed artifacts only.

Order native-grounding-step3-tuning, deliverable D2. Every failing case from
the Step 2 A/B (the competence-loss refusals and the uncaptured absent probe)
plus the failing HARD-lane cases, one row each, with the full stage trace:

    retrieval pool -> admission margin -> resolver -> judge verdict
    -> abstention action -> synthesis

Replayability: every input is a committed file — the A/B ResultRow JSONL,
transcripts and run logs under sovereign/bench/calibration/ab/, the saltgrass
bank (gold keywords), and the dated baseline snapshots under
sovereign/bench/*/baselines/ (old mint vs 2026-08-10 re-mint). Re-running
this script regenerates failure_corpus.jsonl byte-for-byte from the repo.

Stages a case never reached are recorded as {"ran": false, "why": ...} —
absence is reported, never defaulted (ARCH §18.3).
"""
import gzip, json, re, sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
AB = REPO / "bench/calibration/ab"
BANK = REPO / "bench/chaos_monkey/saltgrass.toml"
OUT = Path(__file__).resolve().parent / "failure_corpus.jsonl"

ANSI = re.compile(r"\x1b\[[0-9;]*m")
ABSENT_QTYPES = {"absent_adjacent", "absent_out_of_domain"}

# ---------------------------------------------------------------- bank gold
def load_bank():
    """gold_keywords per question id, parsed from the committed bank TOML."""
    try:
        import tomllib
        qs = tomllib.loads(BANK.read_text())["questions"]
    except ModuleNotFoundError:  # py<3.11
        import toml
        qs = toml.loads(BANK.read_text())["questions"]
    return {q["id"]: q for q in qs}

# ------------------------------------------------------------- A/B loading
def load_transcripts(arm):
    with gzip.open(AB / f"ab_saltgrass_{arm}.transcripts.jsonl.gz", "rt") as fh:
        return {r["id"]: r for r in map(json.loads, fh)}

def parse_admissions(arm):
    """Per-turn H1 admission records from the run log, keyed by the [router]
    question prefix that precedes each admission line."""
    out, cur_q = [], None
    with gzip.open(AB / f"ab_saltgrass_{arm}.run.log.gz", "rt") as fh:
        for line in fh:
            line = ANSI.sub("", line)
            m = re.search(r'\[router\] "([^"]+)"', line)
            if m:
                cur_q = m.group(1)
                continue
            if "native-grounding H1: answerability admission" in line:
                g = lambda k: re.search(k + r"=(\S+)", line).group(1)
                out.append({
                    "q_prefix": cur_q,
                    "decision": g("decision"),
                    "answerability": float(g("answerability")),
                    "margin": float(g("margin")),
                    "pool": int(g("pool")),
                    "tau_abstain": float(g("tau_abstain")),
                    "tau_answer": float(g("tau_answer")),
                })
            m = re.search(r"EARLY DECLINE — parametric turn, evidence withheld.*chunks_dropped=(\d+)", line)
            if m and out:
                out[-1]["chunks_dropped"] = int(m.group(1))
    return out

def admission_for(admissions, question):
    for a in admissions:
        if a["q_prefix"] and question.startswith(a["q_prefix"]):
            return a
    return None

def gold_in_pool(gold_keywords, chunks):
    """Presence-of-answer probe: is any gold keyword verbatim in the retrieved
    pool? Case-insensitive substring, same discipline as the bank's own
    grep-verified authoring contract."""
    blob = "\n".join(chunks).lower()
    hits = [k for k in gold_keywords if k.lower() in blob]
    return {"gold_keywords": gold_keywords, "hits": hits, "present": bool(hits)}

DISCLAIMER = "Not in your sources"

def ab_stage_trace(qid, bank, off, on1, on2, adm1, adm2):
    t_off, t_on1, t_on2 = off[qid], on1[qid], on2[qid]
    q = t_off["question"]
    gold = bank.get(qid, {}).get("gold_keywords", [])
    a1 = admission_for(adm1, q)
    a2 = admission_for(adm2, q)
    # Retrieval pool: the off arm carries the pre-admission pool (both arms run
    # the identical rerank-carrying pipeline; the on arm's transcript pool is
    # post-drop). Recorded with that caveat, not silently substituted.
    pool_chunks = t_off["retrieved_chunks"]
    return {
        "retrieval": {
            "ran": True,
            "pool_source": "flag-off arm transcript (pre-admission pool proxy; on-arm transcript pool is post-drop)",
            "pool_n": len(pool_chunks),
            "answer_in_pool": gold_in_pool(gold, pool_chunks) if gold else {"gold_keywords": [], "hits": [], "present": None},
        },
        "admission": {
            "ran": a1 is not None,
            "why": None if a1 else "no H1 admission line for this turn (9 of 42 turns never reached H1)",
            "r1": a1, "r2": a2,
        },
        "resolver": {
            "ran": False,
            "why": "abstained/parametric turns never reach span resolution; citation_located="
                   + str(t_on1.get("citation_located")),
        },
        "judge": {
            "ran": t_on1.get("gate_action") is not None,
            "gate_action_on_r1": t_on1.get("gate_action"),
            "gate_action_off": t_off.get("gate_action"),
            "why": None if t_on1.get("gate_action") else "EARLY DECLINE skips synthesis gate entirely",
        },
        "abstention_action": {
            "ran": bool(a1 and a1["decision"] == "Abstain"),
            "evidence_withheld_chunks": (a1 or {}).get("chunks_dropped"),
            "fallback": "general-knowledge parametric turn",
            "disclaimer_present_r1": DISCLAIMER in t_on1["answer"],
            "epistemic_verdict_on_r1": (t_on1.get("epistemic_state") or {}).get("verdict"),
        },
        "synthesis": {
            "ran": True,
            "off": {"action": t_off["agent_action"], "pass": t_off["pass"], "answer_excerpt": t_off["answer"][:240]},
            "on_r1": {"action": t_on1["agent_action"], "pass": t_on1["pass"], "answer_excerpt": t_on1["answer"][:240]},
            "on_r2": {"action": t_on2["agent_action"], "pass": t_on2["pass"], "answer_excerpt": t_on2["answer"][:240]},
        },
    }

def build_ab_rows():
    bank = load_bank()
    off, on1, on2 = load_transcripts("off"), load_transcripts("on"), load_transcripts("on_r2")
    adm1, adm2 = parse_admissions("on"), parse_admissions("on_r2")
    rows = []
    comp_set = [i for i, r in off.items() if r["qtype"] not in ABSENT_QTYPES]
    for qid in off:
        r = off[qid]
        fam = None
        if qid in comp_set and r["pass"] and not on1[qid]["pass"]:
            fam = "comp_loss"                       # the 15
        elif qid in comp_set and r["pass"] and on1[qid]["pass"] and not on2[qid]["pass"]:
            fam = "comp_loss_r2_only"               # r2's 16th
        elif qid == "ood-css-center":
            fam = "absent_uncaptured"               # the 1: uncaveated GK answer in every arm
        if not fam:
            continue
        rows.append({
            "case_id": f"ab:{qid}",
            "source": "ab_saltgrass (committed Step 2 A/B)",
            "family": fam,
            "qtype": r["qtype"],
            "question": r["question"],
            "expected": r["expected_action"],
            "got": {"off": f'{r["agent_action"]}/{"P" if r["pass"] else "F"}',
                    "on_r1": f'{on1[qid]["agent_action"]}/{"P" if on1[qid]["pass"] else "F"}',
                    "on_r2": f'{on2[qid]["agent_action"]}/{"P" if on2[qid]["pass"] else "F"}'},
            "stage_trace": ab_stage_trace(qid, bank, off, on1, on2, adm1, adm2),
        })
    return rows

# ------------------------------------------------------- HARD-lane loading
def snap(group, bench, date):
    p = REPO / f"bench/{group}/baselines/{bench}/{date}.json"
    return {r["question_id"]: r for r in json.loads(p.read_text())["results"]}

RETRIEVAL_BENCHES = [
    # (lane, group, baseline-dir, old-date)
    ("retrieval:wikipedia", "wikipedia", "newsworthy_smoke", "2026-07-16"),
    ("retrieval:wikipedia", "wikipedia", "questions", "2026-07-16"),
    ("retrieval-prod:sep", "sep", "summarize-prod-isolated", "2026-07-17"),
    ("retrieval-prod:sep", "sep", "summarize_obscure-prod-isolated", "2026-07-17"),
]
ROUTING_BENCHES = [
    ("routing", "routing", "cells_v1_paraphrases-routing", "2026-07-16"),
    ("routing", "routing", "skills_migration_smoke-routing", "2026-07-16"),
]
NEW_DATE = "2026-08-10"

def build_hard_rows():
    rows = []
    for lane, group, bench, old_date in RETRIEVAL_BENCHES:
        old, new = snap(group, bench, old_date), snap(group, bench, NEW_DATE)
        for qid, b in old.items():
            c = new.get(qid)
            if not c:
                continue
            df = c["fact_score"]["ratio"] - b["fact_score"]["ratio"]
            ds = c["source_score"]["ratio"] - b["source_score"]["ratio"]
            if df >= 0 and ds >= 0:
                continue  # improvements/green are adjudicated, not failures
            lost_f = sorted(set(b["fact_score"]["matched"]) - set(c["fact_score"]["matched"]))
            lost_s = sorted(set(b["source_score"]["matched"]) - set(c["source_score"]["matched"]))
            rows.append({
                "case_id": f"{lane}/{bench}:{qid}",
                "source": f"HARD lane {lane} (committed baselines {old_date} vs {NEW_DATE})",
                "family": "retrieval_fact_loss" if df < 0 else "retrieval_source_loss",
                "question": b["question"],
                "expected": {"facts": b["fact_score"]["matched"] + b["fact_score"]["missing"],
                             "sources": b["source_score"]["matched"] + b["source_score"]["missing"]},
                "got": {"fact_ratio": [b["fact_score"]["ratio"], c["fact_score"]["ratio"]],
                        "source_ratio": [b["source_score"]["ratio"], c["source_score"]["ratio"]],
                        "lost_facts": lost_f, "lost_sources": lost_s},
                "stage_trace": {
                    "retrieval": {"ran": True, "pool_n_old": len(b["retrieved"]), "pool_n_new": len(c["retrieved"]),
                                  "titles_new": sorted({x["title"] for x in c["retrieved"]})[:12]},
                    "admission": {"ran": False, "why": "HARD retrieval lane stops at the evidence pool; native grounding not in path (flag OFF)"},
                    "resolver": {"ran": False, "why": "same"},
                    "judge": {"ran": False, "why": "same"},
                    "abstention_action": {"ran": False, "why": "same"},
                    "synthesis": {"ran": False, "why": "same"},
                },
            })
    for lane, group, bench, old_date in ROUTING_BENCHES:
        old, new = snap(group, bench, old_date), snap(group, bench, NEW_DATE)
        for qid, b in old.items():
            c = new.get(qid)
            if not c:
                continue
            b_ok = b["actual_intent"] == b["expected"]
            c_ok = c["actual_intent"] == c["expected"]
            if b_ok and not c_ok:
                rows.append({
                    "case_id": f"{lane}/{bench}:{qid}",
                    "source": f"HARD lane routing (committed baselines {old_date} vs {NEW_DATE})",
                    "family": "routing_misroute",
                    "question": b["question"],
                    "expected": b["expected"],
                    "got": {"baseline": {"intent": b["actual_intent"], "layer": b["coarse_intent"], "conf": b["confidence"]},
                            "current": {"intent": c["actual_intent"], "layer": c["coarse_intent"], "conf": c["confidence"],
                                        "rationale": (c.get("rationale") or "")[:160]}},
                    "stage_trace": {
                        "routing": {"ran": True,
                                    "mechanism": "probe formerly resolved at embed/lookup layer now falls through to coarse-LLM"},
                        "retrieval": {"ran": False, "why": "routing lane stops at intent classification"},
                        "admission": {"ran": False, "why": "same"},
                        "resolver": {"ran": False, "why": "same"},
                        "judge": {"ran": False, "why": "same"},
                        "abstention_action": {"ran": False, "why": "same"},
                        "synthesis": {"ran": False, "why": "same"},
                    },
                })
    return rows

def main():
    rows = build_ab_rows() + build_hard_rows()
    with open(OUT, "w") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False) + "\n")
    from collections import Counter
    fam = Counter(r["family"] for r in rows)
    print(f"wrote {len(rows)} cases -> {OUT}")
    for k, v in sorted(fam.items()):
        print(f"  {k}: {v}")

if __name__ == "__main__":
    sys.exit(main())
