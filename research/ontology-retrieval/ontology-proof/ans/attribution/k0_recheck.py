#!/usr/bin/env python3
"""Operator-directed K0 recheck on the repaired binary + neutering proof.

Single run per arm, K0 subset only, judge on. NOT bar ratification: n=15,
one run, local judge model differs from study-1's pod judge, so absolute
scores are not comparable across binaries — direction and mechanism are.

Neutering proof, per full-arm question: hashes the atlas walk injected
(after-before at the atlas_grounding step) and the atom-enum injected
(after-before at atom_enum), intersected with the prompt_admission set.
If ontology-sourced hashes reach the recorded prompt, parity was NOT
achieved by neutering the ontology at retrieval.
"""
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import time

ROOT = pathlib.Path("/Users/alexsbryan/dev/commonwealth-ai")
ATTR = ROOT / "research/ontology-retrieval/ontology-proof/ans/attribution"
TMP = pathlib.Path("/var/folders/qf/4ntssn_10d598hw_5rjhx3740000gp/T/opencode")
OUTDIR = ATTR / "k0-recheck-20260923"
OUTDIR.mkdir(exist_ok=True)
COMMIT = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True,
                        text=True, cwd=ROOT).stdout.strip()

# ── 1. K0-only bank, derived from the frozen attested bank ──────────
src = (ROOT / "research/ontology-retrieval/ontology-proof/ans/bank.attested.toml").read_text()
header, *blocks = src.split("[[questions]]")
k0 = [b for b in blocks if 'category = "k0_look_it_up"' in b]
bank_path = TMP / "ei7-k0-recheck.toml"
bank_path.write_text(
    header.replace('name = "ontology-proof-ans-v1"', 'name = "ei7-k0-recheck-diagnostic"')
    + "[[questions]]".join([""] + k0).lstrip("\n"))
n_q = len(k0)
print(f"bank: {n_q} K0 questions -> {bank_path}")

ARMS = {
    "bare": {"SOVEREIGN_ATLAS_GROUNDING": "0", "SOVEREIGN_ATOM_ENUM": "0", "SOVEREIGN_ATOM_ENUM_OVERVIEW": "0"},
    "full": {"SOVEREIGN_ATLAS_GROUNDING": "1", "SOVEREIGN_ATOM_ENUM": "1", "SOVEREIGN_ATOM_ENUM_OVERVIEW": "1"},
}

def parse_trace(text):
    """question_hash -> {step: (before_set, after_set)}"""
    ansi = re.compile(r"\x1b\[[0-9;]*m")
    ev = re.compile(r'passage identities .*?step="([^"]+)" .*?query_hash=([0-9a-f]+) before=(\[[^]]*\]) after=(\[[^]]*\])')
    steps = {}
    for line in ansi.sub("", text).splitlines():
        m = ev.search(line)
        if m:
            steps.setdefault(m.group(2), {})[m.group(1)] = (
                set(json.loads(m.group(3))), set(json.loads(m.group(4))))
    return steps

summary = {"commit": COMMIT, "bank_questions": n_q, "arms": {}}
for arm, env_flags in ARMS.items():
    env = dict(os.environ, RUST_LOG="sovereign_core=info,retrieval.pipeline=debug,retrieval_audit=info",
               NO_COLOR="1", **env_flags)
    cmd = ["target/debug/sovereign-cli", "eval", "run", "--bank", str(bank_path),
           "--synth", "--isolate", "--format", "json", "--chat-model", "commonwealth/primary"]
    t0 = time.time()
    proc = subprocess.run(cmd, env=env, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=5400)
    wall = time.time() - t0
    (OUTDIR / f"{arm}-eval.json").write_text(proc.stdout)
    (OUTDIR / f"{arm}-trace.log").write_text(proc.stderr)
    run = json.loads(proc.stdout)
    steps = parse_trace(proc.stderr)
    rows = []
    for r in run["results"]:
        qhash = hashlib.sha256(r["question"].encode()).hexdigest()[:12]
        st = steps.get(qhash, {})
        atlas = st.get("atlas_grounding", (set(), set()))
        enum = st.get("atom_enum", (set(), set()))
        admitted = st.get("prompt_admission", (set(), set()))[1]
        walk_added = atlas[1] - atlas[0]
        enum_added = enum[1] - enum[0]
        judge = r["synth"].get("judge_fact_score") or {}
        rows.append({
            "qid": r["question_id"],
            "judge": round(judge.get("ratio", -1), 3),
            "matched": len(judge.get("matched", [])), "missing": len(judge.get("missing", [])),
            "gate": r["synth"]["gate"]["action"],
            "walk_kind": (r.get("atlas_walk") or {}).get("kind"),
            "walk_added_echo": (r.get("atlas_walk") or {}).get("added"),
            "walk_injected_hashes": len(walk_added),
            "walk_reached_prompt": len(walk_added & admitted),
            "enum_injected_hashes": len(enum_added),
            "enum_reached_prompt": len(enum_added & admitted),
            "admitted": len(admitted),
        })
    summary["arms"][arm] = {
        "exit": proc.returncode, "wall_s": round(wall),
        "rows": rows,
        "mean_judge": round(sum(x["judge"] for x in rows if x["judge"] >= 0) / max(1, len(rows)), 4),
        "questions_with_walk_reach": sum(1 for x in rows if x["walk_reached_prompt"] > 0),
        "questions_with_enum_reach": sum(1 for x in rows if x["enum_reached_prompt"] > 0),
        "total_ontology_hashes_in_prompts": sum(x["walk_reached_prompt"] + x["enum_reached_prompt"] for x in rows),
    }
    (OUTDIR / f"{arm}-summary.json").write_text(json.dumps(summary["arms"][arm], indent=1))
    a = summary["arms"][arm]
    print(f"[{arm}] exit={a['exit']} wall={a['wall_s']}s mean_judge={a['mean_judge']} "
          f"walk_reach={a['questions_with_walk_reach']}/{n_q} enum_reach={a['questions_with_enum_reach']}/{n_q} "
          f"ontology_hashes_in_prompts={a['total_ontology_hashes_in_prompts']}")

(OUTDIR / "summary.json").write_text(json.dumps(summary, indent=1))
print("wrote", OUTDIR / "summary.json")
