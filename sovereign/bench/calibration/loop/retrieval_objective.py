#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Retrieval component objective — order native-grounding-tuning-loop (A4).

Pool recomposition + keyword-ratio probe for the 11 Group-4 cases in the
parity plan's §3.1 ledger. For each affected bench a trimmed bank (the
ledger's question blocks, verbatim) is generated into a temp dir and run
through the SAME composition path as the HARD lane (`eval run`, raw for
the wikipedia lane, `--prod-pipeline --isolate` for the sep lane,
`--limit 30` = the lane's retrieval_limit). Per-case pass = fact ratio AND
source ratio back at (or above) the 2026-07-16/17 baseline column.

Targets are read from the committed failure corpus (step3/failure_corpus.jsonl)
— the same rows the plan quotes; no second copy of a threshold exists here.

Usage: retrieval_objective.py [bench-substring]   (scope the inner loop to
one bench, e.g. `summarize_obscure`; the component verdict is the full run)

Exit: 0 all scoped cases pass / 1 any fail / 2 could not judge.
"""
import json, re, subprocess, sys, tempfile
from pathlib import Path

LOOP = Path(__file__).resolve().parent
REPO = LOOP.parents[3]
BIN = REPO / "target/debug/sovereign-cli-llm"
CORPUS = REPO / "sovereign/bench/calibration/step3/failure_corpus.jsonl"

# bench key -> (bank file, prod-pipeline?)
BENCHES = {
    "newsworthy_smoke": (REPO / "sovereign/bench/wikipedia/newsworthy_smoke.toml", False),
    "questions": (REPO / "sovereign/bench/wikipedia/questions.toml", False),
    "summarize-prod-isolated": (REPO / "sovereign/bench/sep/summarize.toml", True),
    "summarize_obscure-prod-isolated": (REPO / "sovereign/bench/sep/summarize_obscure.toml", True),
}

def load_targets():
    """case targets from the committed ledger: bench -> qid -> (fact, src)."""
    targets = {}
    for line in CORPUS.read_text().splitlines():
        r = json.loads(line)
        if not r["family"].startswith("retrieval"):
            continue
        # case_id: "<lane>/<bench>:<qid>"
        bench, qid = r["case_id"].split("/", 1)[1].split(":", 1)
        targets.setdefault(bench, {})[qid] = (
            r["got"]["fact_ratio"][0], r["got"]["source_ratio"][0])
    return targets

def trim_bank(src: Path, qids, dst: Path):
    """Copy [bank] name+corpus and the verbatim [[questions]] blocks for qids."""
    text = src.read_text()
    m_name = re.search(r'^name\s*=\s*"([^"]+)"', text, re.M)
    m_corpus = re.search(r'^corpus\s*=\s*"([^"]+)"', text, re.M)
    if not (m_name and m_corpus):
        print(f"could-not-judge: no [bank] name/corpus in {src}"); sys.exit(2)
    blocks = re.split(r"(?=^\[\[questions\]\])", text, flags=re.M)[1:]
    picked = []
    for b in blocks:
        m = re.search(r'^id\s*=\s*"([^"]+)"', b, re.M)
        if m and m.group(1) in qids:
            picked.append(b.rstrip() + "\n")
    got = {re.search(r'^id\s*=\s*"([^"]+)"', b, re.M).group(1) for b in picked}
    if got != set(qids):
        print(f"could-not-judge: {src.name} missing ids {set(qids) - got}"); sys.exit(2)
    dst.write_text(
        f'[bank]\nname = "{m_name.group(1)}-loop-subset"\n'
        f'corpus = "{m_corpus.group(1)}"\n\n' + "\n".join(picked))

def run_bench(bank: Path, prod: bool, out: Path):
    cmd = [str(BIN), "eval", "run", "--bank", str(bank),
           "--limit", "30", "--output", str(out), "--format", "json"]
    if prod:
        cmd += ["--prod-pipeline", "--isolate"]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if not out.exists():
        print(f"could-not-judge: eval run wrote no output (rc={r.returncode})\n"
              f"{(r.stderr or r.stdout)[-600:]}")
        sys.exit(2)
    return {q["question_id"]: q for q in json.loads(out.read_text())["results"]}

def main():
    scope = sys.argv[1] if len(sys.argv) > 1 and sys.argv[1] else None
    targets = load_targets()
    fails, n_cases = [], 0
    with tempfile.TemporaryDirectory() as td:
        for bench, (bank, prod) in BENCHES.items():
            if scope and scope not in bench:
                continue
            qids = targets.get(bench, {})
            if not qids:
                continue
            trimmed = Path(td) / f"{bench}.toml"
            trim_bank(bank, list(qids), trimmed)
            results = run_bench(trimmed, prod, Path(td) / f"{bench}.json")
            for qid, (t_fact, t_src) in sorted(qids.items()):
                q = results.get(qid)
                if q is None:
                    print(f"could-not-judge: {bench}:{qid} absent from run"); sys.exit(2)
                f_now = q["fact_score"]["ratio"]
                s_now = q["source_score"]["ratio"]
                ok = f_now >= t_fact - 1e-6 and s_now >= t_src - 1e-6
                n_cases += 1
                mark = "PASS" if ok else "FAIL"
                print(f"  {mark} {bench}:{qid} fact {f_now:.3f} (target {t_fact:.3f}) "
                      f"src {s_now:.3f} (target {t_src:.3f})")
                if not ok:
                    fails.append(f"{bench}:{qid}")
    if n_cases == 0:
        print(f"could-not-judge: scope {scope!r} matched no ledger case"); sys.exit(2)
    if fails:
        print(f"retrieval objective FAIL: {len(fails)}/{n_cases} below baseline: {fails}")
        sys.exit(1)
    print(f"retrieval objective PASS: {n_cases}/{n_cases} cases at/above the baseline column")
    sys.exit(0)

if __name__ == "__main__":
    main()
