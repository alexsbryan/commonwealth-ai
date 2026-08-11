#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Read a desktop-soak run and report the three things its scorecards don't.

Order `native-grounding-flip-soak` (operator directive 7aa64f29) asks for
per-turn latency percentiles, native-grounding DISPLAY telemetry, and a
memory profile. The existing surfaces cover most of the ground and are
NOT reimplemented here (ARCH §19 — the inventory outranks the plan):

  * quality scoring        -> chaos-scorecard.mjs / persona-scoreboard.mjs,
                              already rendered by desktop-soak.py itself.
  * per-turn latency       -> ALREADY journalled as `latencyMs`; the
                              scorecards just print p90 and means.

So this script adds exactly three things and reads existing journals for
all of them:

  1. p50/p95 over the journalled per-turn latency (the order says
     percentiles at 2h scale, never single-turn).
  2. A rollup of the `grounding` field added to both journals on
     2026-08-11 — segments rendered, Grounded badges, how many RESOLVE
     (the P1 citability bar), gate actions, answerability.
  3. The free-RAM profile from the `-mem.jsonl` series.

TWO MODES, because an instrument and a result are different questions
(ARCH §18.4):

  --mode shakedown   Did the NEW instrumentation actually record? Four
                     verdicts per instrument (passed / failed /
                     could-not-judge / never-ran), exit non-zero if any
                     instrument is dead. Validates nothing about quality.
  --mode report      The measurement itself, for a completed run.

Usage:
  scripts/flip-soak-verify.py --stamp <stamp> [--mode shakedown|report]
"""
import argparse
import json
import os
import statistics
import sys

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
OUTDIR = os.path.join(
    REPO, "sovereign/crates/sovereign-desktop/test-artifacts/qa-iterations")


def load(path):
    if not os.path.exists(path):
        return None  # never-ran, which is NOT the same as empty
    rows = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return rows


def pct(values, q):
    """Nearest-rank percentile. Explicit about small n rather than
    returning a confident-looking number from three samples."""
    if not values:
        return None
    s = sorted(values)
    k = max(0, min(len(s) - 1, int(round((q / 100.0) * len(s) + 0.5)) - 1))
    return s[k]


def turn_rows(chaos, personas):
    """Chat turns from both journals, normalised to (source, latency, grounding)."""
    out = []
    for r in chaos or []:
        # chaos journals every MOVE; only chat moves carry a latency that
        # means "the user waited for an answer".
        if r.get("latencyMs") is None:
            continue
        out.append(("chaos", r.get("latencyMs"), r.get("grounding"), r))
    for r in personas or []:
        if r.get("kind") != "turn" or r.get("latencyMs") is None:
            continue
        out.append(("persona", r.get("latencyMs"), r.get("grounding"), r))
    return out


def latency_table(turns):
    lines = []
    for label in ("chaos", "persona", "ALL"):
        vals = [t[1] for t in turns if label == "ALL" or t[0] == label]
        vals = [v for v in vals if isinstance(v, (int, float))]
        if not vals:
            lines.append(f"  {label:<8} n=0        (never ran)")
            continue
        lines.append(
            f"  {label:<8} n={len(vals):<5} "
            f"p50={pct(vals, 50)/1000:.1f}s  p95={pct(vals, 95)/1000:.1f}s  "
            f"max={max(vals)/1000:.1f}s  mean={statistics.mean(vals)/1000:.1f}s")
    return lines


def grounding_rollup(turns):
    """Summarise the display telemetry. ABSENT and EMPTY stay distinct."""
    total = len(turns)
    have_field = [t for t in turns if t[2] is not None]
    # `segments is None` means the turn never segmented (opted out, or
    # NoInstrument). `0` means it segmented and found nothing.
    segmented = [t for t in have_field if t[2].get("segments") is not None]
    seg_counts = [t[2]["segments"] for t in segmented]
    grounded = sum(t[2].get("grounded") or 0 for t in segmented)
    addressed = sum(t[2].get("addressed") or 0 for t in segmented)
    answerabilities = [t[2]["nativeAnswerability"] for t in have_field
                       if isinstance(t[2].get("nativeAnswerability"), (int, float))]
    actions = {}
    for t in have_field:
        a = t[2].get("gateAction")
        if a:
            actions[a] = actions.get(a, 0) + 1
    return {
        "turns": total,
        "with_field": len(have_field),
        "segmented": len(segmented),
        "not_segmented": len(have_field) - len(segmented),
        "segments_total": sum(seg_counts),
        "grounded": grounded,
        "addressed": addressed,
        # THE citability number: of the badges shown, how many resolve.
        "citability": (addressed / grounded) if grounded else None,
        "answerability_n": len(answerabilities),
        "answerability_median": statistics.median(answerabilities) if answerabilities else None,
        "actions": actions,
    }


def mem_profile(mem):
    if mem is None:
        return None
    measured = [r for r in mem if r.get("free_gb") is not None]
    unmeasured = [r for r in mem if r.get("free_gb") is None]
    if not measured:
        return {"samples": len(mem), "measured": 0,
                "why": (unmeasured[0].get("why") if unmeasured else "no samples")}
    free = [r["free_gb"] for r in measured]
    avail = [r["avail_gb"] for r in measured if r.get("avail_gb") is not None]
    return {
        "samples": len(mem),
        "measured": len(measured),
        "unmeasured": len(unmeasured),
        "free_min": min(free), "free_p05": pct(free, 5),
        "free_median": statistics.median(free), "free_max": max(free),
        "avail_min": min(avail) if avail else None,
        "avail_median": statistics.median(avail) if avail else None,
        "below_2gb": sum(1 for v in free if v < 2.0),
    }


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--stamp", required=True)
    p.add_argument("--mode", choices=["shakedown", "report"], default="report")
    a = p.parse_args()

    chaos = load(os.path.join(OUTDIR, f"{a.stamp}-chaos.jsonl"))
    personas = load(os.path.join(OUTDIR, f"{a.stamp}-personas.jsonl"))
    mem = load(os.path.join(OUTDIR, f"{a.stamp}-mem.jsonl"))
    done_path = os.path.join(OUTDIR, f"{a.stamp}.DONE")
    done = open(done_path).read().strip() if os.path.exists(done_path) else None

    turns = turn_rows(chaos, personas)
    g = grounding_rollup(turns)
    m = mem_profile(mem)

    print(f"=== flip-soak-verify stamp={a.stamp} mode={a.mode} ===")
    print(f"sentinel: {done or 'ABSENT — run did not reach completion'}")
    print(f"journals: chaos={'absent' if chaos is None else len(chaos)} rows, "
          f"personas={'absent' if personas is None else len(personas)} rows, "
          f"mem={'absent' if mem is None else len(mem)} samples")
    print()
    print("LATENCY (per-turn wall clock, journalled `latencyMs`)")
    for line in latency_table(turns):
        print(line)
    print()
    print("NATIVE-GROUNDING DISPLAY TELEMETRY")
    print(f"  chat turns                : {g['turns']}")
    print(f"  carrying grounding field  : {g['with_field']}")
    print(f"  segmented (field present) : {g['segmented']}")
    print(f"  did NOT segment           : {g['not_segmented']}  "
          f"(opted out / no instrument — NOT the same as 'segmented into nothing')")
    print(f"  segments rendered         : {g['segments_total']}")
    print(f"  Grounded badges           : {g['grounded']}")
    print(f"  ...of which RESOLVE       : {g['addressed']}")
    print(f"  CITABILITY (resolved/shown): "
          f"{'n/a — no badges' if g['citability'] is None else f'{g['citability']:.3f}'}")
    print(f"  answerability samples     : {g['answerability_n']}"
          + (f" (median {g['answerability_median']:.3f})"
             if g["answerability_median"] is not None else ""))
    print(f"  gate actions              : {g['actions'] or '(none recorded)'}")
    print()
    print("MEMORY PROFILE (free RAM, strict)")
    if m is None:
        print("  NEVER RAN — no -mem.jsonl for this stamp")
    elif m["measured"] == 0:
        print(f"  COULD NOT JUDGE — {m['samples']} samples, none measured "
              f"(why: {m['why']})")
    else:
        print(f"  samples {m['measured']} measured / {m['unmeasured']} not")
        print(f"  free  min={m['free_min']:.2f}GB  p05={m['free_p05']:.2f}GB  "
              f"median={m['free_median']:.2f}GB  max={m['free_max']:.2f}GB")
        if m["avail_median"] is not None:
            print(f"  reclaimable  min={m['avail_min']:.2f}GB  "
                  f"median={m['avail_median']:.2f}GB")
        print(f"  samples below the 2GB abort line: {m['below_2gb']}")

    if a.mode == "report":
        return 0

    # ── shakedown: four verdicts on the NEW instrumentation only ──
    print()
    print("INSTRUMENT VERDICTS (shakedown — validates capture, not quality)")
    failures = []

    def verdict(name, state, detail):
        print(f"  [{state:<15}] {name} — {detail}")
        if state == "FAILED":
            failures.append(name)

    if not turns:
        verdict("chat turns", "FAILED",
                "no chat turns journalled — spawns or bridge did not work")
    else:
        verdict("chat turns", "PASSED", f"{len(turns)} turns with latency")

    if not turns:
        verdict("grounding telemetry", "NEVER RAN", "no turns to carry it")
    elif g["with_field"] == 0:
        verdict("grounding telemetry", "FAILED",
                "no turn carried a `grounding` field — the journal wiring is dead")
    elif g["segmented"] == 0:
        verdict("grounding telemetry", "COULD NOT JUDGE",
                "field present but no turn segmented; flag may be off or no "
                "instrument (check the daemon log for 'no answerability instrument')")
    else:
        verdict("grounding telemetry", "PASSED",
                f"{g['segmented']}/{g['with_field']} turns segmented, "
                f"{g['grounded']} badges, {g['addressed']} resolve")

    if m is None:
        verdict("free-RAM sampler", "NEVER RAN", "no -mem.jsonl written")
    elif m["measured"] == 0:
        verdict("free-RAM sampler", "FAILED",
                f"samples written but none measured ({m['why']})")
    else:
        verdict("free-RAM sampler", "PASSED",
                f"{m['measured']} real samples, median {m['free_median']:.2f}GB free")

    # daemonRssMb — the §18.3 fix. Prior real runs journalled null here.
    rss = [r.get("daemonRssMb") for r in (personas or []) if "daemonRssMb" in r]
    if not rss:
        verdict("daemon RSS", "NEVER RAN", "no run_start/run_end rows (persona phase absent?)")
    else:
        got = [v for v in rss if isinstance(v, dict) and v.get("mb") is not None]
        if got:
            verdict("daemon RSS", "PASSED",
                    f"{len(got)}/{len(rss)} readings carry an mb value "
                    f"(was silently null on every prior macOS run)")
        else:
            whys = {v.get("why") for v in rss if isinstance(v, dict)}
            verdict("daemon RSS", "FAILED",
                    f"still no mb value; reasons={whys or 'legacy bare-null shape'}")

    print()
    if failures:
        print(f"SHAKEDOWN FAILED — dead instruments: {', '.join(failures)}")
        return 1
    print("SHAKEDOWN OK — the new instrumentation records. This says NOTHING "
          "about answer quality or latency health; that is the 2h run's job.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
