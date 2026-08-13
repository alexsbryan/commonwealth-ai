#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Count what one knowledge turn actually did — see probe-t1-expansion-fanout.sh.

Reads a `retrieval_audit` trace from ONE turn and reports:
  * full fan-outs per turn, broken down by the emitting stage (`label=`),
  * corpora searched per fan-out and the turn's total corpus-searches,
  * fan-out wall time (sum and per-fan-out spread) and the turn's
    `retrieval_ms`,
  * prefilter passes per turn, and what each pass pruned and cost.

Everything here is a COUNT of shipped glassbox lines. If a count is zero the
report says so as its own verdict rather than printing a tidy zero — a probe
that silently reports "0 fan-outs" because the target was off is the
could-not-judge case, not a result.
"""
from __future__ import annotations

import argparse
import re
from collections import Counter, defaultdict

# tracing's fmt layer renders fields as `key=value`, values unquoted for
# numbers and quoted for strings. Both forms are accepted.
FIELD = r'{k}=(?:"([^"]*)"|([^\s]+))'


def field(line: str, key: str):
    m = re.search(FIELD.format(k=re.escape(key)), line)
    if not m:
        return None
    return m.group(1) if m.group(1) is not None else m.group(2)


def ints(vals):
    out = []
    for v in vals:
        try:
            out.append(int(float(v)))
        except (TypeError, ValueError):
            pass
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--trace", required=True)
    ap.add_argument("--corpora", type=int, required=True, help="corpora installed in the rig")
    ap.add_argument("--turn-wall", type=float, required=True)
    ap.add_argument("--prefilter", default="off")
    ap.add_argument("--ask-rc", type=int, default=0)
    ap.add_argument("--turn", type=int, default=1)
    args = ap.parse_args()

    raw = open(args.trace, errors="replace").read()
    # Daemon/CLI logs are ANSI-coloured; strip escapes before matching or
    # every field regex silently finds nothing (Probe A learned this the
    # expensive way).
    raw = re.sub(r"\x1b\[[0-9;]*m", "", raw)
    lines = raw.splitlines()

    fanouts = [ln for ln in lines if "retrieval_audit: fanout_complete" in ln]
    prefilters = [ln for ln in lines if "retrieval_audit: corpus_prefilter" in ln]
    corpus_results = [ln for ln in lines if "retrieval_audit: corpus_results" in ln]
    turn_summaries = [ln for ln in lines if "retrieval_audit: turn_summary" in ln
                      or "retrieval_audit: deep_turn_summary" in ln]

    print(f"PROBE_T1 turn={args.turn} rig_corpora={args.corpora} "
          f"prefilter={args.prefilter} ask_rc={args.ask_rc} "
          f"turn_wall_s={args.turn_wall:.1f}")

    if not fanouts:
        print("PROBE_T1 COULD-NOT-JUDGE — no `retrieval_audit: fanout_complete` lines in the "
              "trace. Either the turn never reached corpus search or the target was off; "
              "either way this run measured the instrument, not the system.")
        return

    by_label = Counter()
    corpora_per_fanout = []
    ms_per_fanout = []
    label_ms = defaultdict(int)
    for ln in fanouts:
        lbl = field(ln, "label") or "?"
        by_label[lbl] += 1
        c = ints([field(ln, "corpora")])
        m = ints([field(ln, "fanout_ms")])
        if c:
            corpora_per_fanout.append(c[0])
        if m:
            ms_per_fanout.append(m[0])
            label_ms[lbl] += m[0]

    total_corpus_searches = sum(corpora_per_fanout)
    print(f"PROBE_T1 fanouts_per_turn={len(fanouts)} "
          f"by_label={dict(sorted(by_label.items()))}")
    if corpora_per_fanout:
        print(f"PROBE_T1 corpora_per_fanout min={min(corpora_per_fanout)} "
              f"max={max(corpora_per_fanout)} "
              f"corpus_searches_per_turn={total_corpus_searches} "
              f"(= sum over fan-outs of the corpora each one searched)")
    if ms_per_fanout:
        print(f"PROBE_T1 fanout_ms min={min(ms_per_fanout)} max={max(ms_per_fanout)} "
              f"sum={sum(ms_per_fanout)} "
              f"by_label_ms={dict(sorted(label_ms.items()))}")
    print(f"PROBE_T1 corpus_results_lines={len(corpus_results)} "
          f"turn_summary_lines={len(turn_summaries)}")

    # Per-turn retrieval wall — `runtime:retrieval_start_to_complete`, a
    # debug-level event (streaming.rs:1165). Absent unless the trace was
    # taken with sovereign_core=debug.
    rms = [ints([field(ln, "retrieval_ms")]) for ln in lines
           if "retrieval_start_to_complete" in ln]
    rms = [x[0] for x in rms if x]
    if rms:
        print(f"PROBE_T1 retrieval_ms_per_turn={rms}")
    else:
        print("PROBE_T1 retrieval_ms_per_turn=NOT-OBSERVED "
              "(debug-level event; sum(fanout_ms) is the lower bound instead)")

    if args.prefilter == "off":
        if prefilters:
            print(f"PROBE_T1 prefilter_passes={len(prefilters)} "
                  "UNEXPECTED — the flag was off for this run")
        else:
            print("PROBE_T1 prefilter_passes=0 (flag off — production default)")
    else:
        if not prefilters:
            print("PROBE_T1 prefilter_passes=0 COULD-NOT-JUDGE — the flag was set but no "
                  "`corpus_prefilter` line was emitted (a no-op guard fired: FTS-only "
                  "query embedding, or eligible <= top_k)")
        else:
            kept = ints([field(ln, "kept") for ln in prefilters])
            dropped = ints([field(ln, "dropped") for ln in prefilters])
            pms = ints([field(ln, "prefilter_ms") for ln in prefilters])
            elig = ints([field(ln, "eligible_total") for ln in prefilters])
            print(f"PROBE_T1 prefilter_passes_per_turn={len(prefilters)} "
                  f"(one per fan-out is the red posture; one per turn is the bar)")
            if elig:
                print(f"PROBE_T1 prefilter_eligible_total={sorted(set(elig))} "
                      f"kept={sorted(set(kept))} dropped={sorted(set(dropped))}")
            if pms:
                print(f"PROBE_T1 prefilter_ms per_pass min={min(pms)} max={max(pms)} "
                      f"sum={sum(pms)} mean={sum(pms) / len(pms):.0f}")


if __name__ == "__main__":
    main()
