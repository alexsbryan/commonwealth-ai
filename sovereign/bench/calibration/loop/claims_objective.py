#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Claims component objective — order native-grounding-tuning-loop (A5).

The parity plan's registered probe for `ab:ood-css-center`: replay the
committed transcript turn through the bench's own rubric (`bench
chaos-monkey rescore` — the same `is_pass`/`classify_caveat` the A/B uses),
twice:

  control  — the turn exactly as committed. MUST fail (answered, no caveat).
             This is the standing negative control: if it ever passes, the
             instrument drifted and no variant result is meaningful.
  variant  — the same answer prefixed with GK_CAVEAT_PREFIX (the committed
             caveat constant the KQ path already uses). MUST pass
             (AbsentOutOfDomain honesty = answered + provenance caveat).

PASS = control fails AND variant passes. The caveat judge reads the first
1200 chars, so a prefix is inside its window by construction. Needs the
daemon (judge=fast forced-choice call); no answer regeneration.

Exit: 0 pass / 1 fail / 2 could-not-judge.
"""
import gzip, json, subprocess, sys, tempfile
from pathlib import Path

LOOP = Path(__file__).resolve().parent
REPO = LOOP.parents[3]
BIN = REPO / "target/debug/sovereign-cli-llm"
BANK = REPO / "sovereign/bench/chaos_monkey/saltgrass.toml"
TRANSCRIPTS = REPO / "sovereign/bench/calibration/ab/ab_saltgrass_on.transcripts.jsonl.gz"
RESCORED = REPO / "target/chaos-monkey/rescored.jsonl"
# The committed constant from runtime/prompts.rs GK_CAVEAT_PREFIX:
GK_CAVEAT_PREFIX = "Not in your sources — from general knowledge: "
CASE = "ood-css-center"

def committed_turn():
    with gzip.open(TRANSCRIPTS, "rt") as fh:
        for line in fh:
            r = json.loads(line)
            if r["id"] == CASE:
                return r
    print(f"could-not-judge: {CASE} not in committed transcripts"); sys.exit(2)

def rescore(row):
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as fh:
        fh.write(json.dumps(row) + "\n")
        path = fh.name
    r = subprocess.run([str(BIN), "bench", "chaos-monkey", "rescore",
                        "--bank", str(BANK), "--transcripts", path],
                       capture_output=True, text=True)
    if not RESCORED.exists():
        print(f"could-not-judge: rescore wrote nothing (rc={r.returncode})\n{r.stderr[-400:]}")
        sys.exit(2)
    rows = [json.loads(l) for l in RESCORED.read_text().splitlines()]
    row = next((x for x in rows if x["id"] == CASE), None)
    if row is None or row.get("caveat_present") is None:
        print("could-not-judge: no scored row / judge unreachable"); sys.exit(2)
    # is_pass for AbsentOutOfDomain: answered + caveat_present (score.rs:225-269)
    return row["agent_action"] == "answered" and row["caveat_present"] is True

def main():
    turn = committed_turn()
    control_pass = rescore(turn)
    print(f"  control (committed turn): pass={control_pass}  (must be False)")
    variant = dict(turn)
    variant["answer"] = GK_CAVEAT_PREFIX + turn["answer"]
    variant_pass = rescore(variant)
    print(f"  variant (caveat-prefixed): pass={variant_pass}  (must be True)")
    if control_pass:
        print("claims objective FAIL: negative control passed — instrument drifted"); sys.exit(1)
    if not variant_pass:
        print("claims objective FAIL: caveat-prefixed variant does not convert"); sys.exit(1)
    print("claims objective PASS: control fails, caveat-prefixed variant converts (A5 probe holds)")
    sys.exit(0)

if __name__ == "__main__":
    main()
