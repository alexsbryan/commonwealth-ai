#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Routing component objective — order native-grounding-tuning-loop (A3).

Offline embed-only replay (`router fit`, no daemon, no LLM) of the 3 probes
from the parity plan's §3.1 Group 3, plus a shape-discipline guard: the
standing calibration banks (axes_v1 + the holdout intent_frames_v1) must not
lose a single case that passed before tuning began. The guard baseline is
captured on first run and committed; tuning against the guard banks is
forbidden — they are read as a gate, exemplars are tuned only against the
3 probes.

Exit: 0 = all 3 probes fired_correct AND guard clean; 1 = a probe misses or
guard regressed; 2 = could not judge (fit tool failed).
"""
import json, subprocess, sys
from pathlib import Path

LOOP = Path(__file__).resolve().parent
REPO = LOOP.parents[3]
BIN = REPO / "target/debug/sovereign-cli-llm"
PROBES = LOOP / "routing_probes.toml"
GUARD_BANKS = [
    REPO / "sovereign/bench/routing/calibration/axes_v1.toml",
    REPO / "sovereign/bench/routing/calibration/holdout/intent_frames_v1.toml",
]
GUARD_BASELINE = LOOP / "routing_guard_baseline.json"
PROBE_IDS = ["commissive_p_flag_for_friday", "metalingual_p_seps_framing", "research_survey"]
PASSING = {"fired_correct", "abstained_correct"}

def fit(banks):
    cmd = [str(BIN), "router", "fit", "--axis", "intent", "--format", "json"]
    for b in banks:
        cmd += ["--bank", str(b)]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if not r.stdout.strip():
        print(f"could-not-judge: router fit produced no output (rc={r.returncode})\n{r.stderr[-500:]}")
        sys.exit(2)
    try:
        return {c["id"]: c for c in json.loads(r.stdout)["attribution"]["intent"]["shipped"]}
    except (json.JSONDecodeError, KeyError) as e:
        print(f"could-not-judge: fit output unparseable: {e}")
        sys.exit(2)

def main():
    probe = fit([PROBES])
    fails = []
    for pid in PROBE_IDS:
        c = probe.get(pid)
        if c is None:
            print(f"could-not-judge: probe {pid} missing from fit output"); sys.exit(2)
        ok = c["verdict"] == "fired_correct"
        print(f"  probe {pid}: {c['verdict']} predicted={c['predicted']} "
              f"cushion={c['cushion']:+.4f} nearest={c['nearest'][:60]!r} rival={c['rival'][:60]!r}")
        if not ok:
            fails.append(pid)

    guard = fit(GUARD_BANKS)
    if not GUARD_BASELINE.exists():
        GUARD_BASELINE.write_text(json.dumps(
            {i: c["verdict"] for i, c in sorted(guard.items())}, indent=0) + "\n")
        print(f"  guard: baseline captured ({len(guard)} cases) -> {GUARD_BASELINE.name}")
    else:
        base = json.loads(GUARD_BASELINE.read_text())
        regressed = [i for i, v in base.items()
                     if v in PASSING and guard.get(i, {}).get("verdict") not in PASSING]
        if regressed:
            for i in regressed:
                print(f"  guard REGRESSED {i}: {base[i]} -> {guard.get(i, {}).get('verdict')}")
            fails.append("guard")
        else:
            n_pass = sum(1 for c in guard.values() if c["verdict"] in PASSING)
            print(f"  guard: clean ({n_pass}/{len(guard)} passing, no case lost vs baseline)")

    if fails:
        print(f"routing objective FAIL: {fails}"); sys.exit(1)
    print("routing objective PASS: 3/3 probes fire at the embed layer, guard clean")
    sys.exit(0)

if __name__ == "__main__":
    main()
