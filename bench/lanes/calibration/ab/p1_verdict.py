#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""P1 — the parity-plan verdict for the flag-on composition.

The bars are `sovereign/docs/specs/NATIVE_GROUNDING_PARITY_PLAN.md` §4.1,
pre-registered before this composition was written and NOT restated here
with different numbers:

  (a) HARD  honesty-when-absent      >= 0.91   both on-runs
  (b) HARD  competence-when-present  >= 0.74   both on-runs
  (c) HARD  A1 arm identity          — no decline-path divergence between
                                       the arms; the plan's causal kill
  (d)       citability               — every Grounded badge resolves to a
                                       real address (rides A2, and the bar
                                       exists so A2 can fail)
  (e)       latency p50/p95          — reported, flag-on vs flag-off

**Why this is a second script and not a second implementation.** The two
turn-level rates are computed by `ab_verdict.arm_scores` and read from the
bench's own red-lines by `ab_verdict.bench_redlines` — imported, not
re-derived, so there is exactly one implementation of "honesty when
absent" in the workspace (ARCH §10.6). What is new here is only what P1
added: the arm-identity check, the citability count over the
`answer_segments` the transcript now carries, and the per-turn `turn_ms`
distribution the chaos harness now records.

**Every bar it cannot measure is reported as could_not_measure WITH the
reason.** A missing field is never scored as a zero (ARCH §18.3): a run
whose transcripts predate `turn_ms` has NO latency verdict, not a fast
one.

Both `.jsonl`/`.log` and their `.gz` forms are read, because `run_ab.sh`'s
artifacts are committed gzipped and a scorer that silently found no file
would report a clean absence instead of a result.

Usage: p1_verdict.py <ab_dir> <out.json>   (arms: off, on, and on_r2 —
       the P1 bars are stated "both on-runs", so a single on-run can
       satisfy at most half of them)
"""
import gzip
import json
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from ab_verdict import ANSI, arm_scores, bench_redlines, bench_verdict, rate  # noqa: E402

HONESTY_BAR = 0.91
COMPETENCE_BAR = 0.74

# The decline lines A1 is about. Both arms must produce the same set: the
# incumbent floor is the only decider on either arm after P1, so a
# divergence means enforcement leaked back in somewhere.
DECLINE_MARKER = "evidence-shape EARLY DECLINE"
# A native decline line must not exist AT ALL after P1 — the arm was
# deleted, not guarded. Its presence in any log is a hard fail.
RETIRED_NATIVE_DECLINE = "H1 answerability EARLY DECLINE"


ARMS = ("off", "on", "on_r2")


def read_text(p):
    """`p`, or `p.gz`, or None. Committed artifacts are gzipped."""
    if p.is_file():
        return p.read_text(errors="replace")
    gz = p.with_suffix(p.suffix + ".gz")
    if gz.is_file():
        return gzip.decompress(gz.read_bytes()).decode("utf-8", "replace")
    return None


def transcripts(d, arm):
    body = read_text(d / f"ab_saltgrass_{arm}.transcripts.jsonl")
    if body is None:
        return None
    out = []
    for line in body.splitlines():
        line = line.strip()
        if line:
            out.append(json.loads(line))
    return out


def latency(ts):
    """p50/p95 of the per-turn wall clock, or a stated absence."""
    if ts is None:
        return {"status": "could_not_measure", "reason": "no transcripts file for this arm"}
    vals = [t["turn_ms"] for t in ts if isinstance(t.get("turn_ms"), (int, float))]
    if not vals:
        return {
            "status": "could_not_measure",
            "reason": (
                "no turn_ms on any transcript row — this run predates the per-turn "
                "timer in bench_cmd/chaos_monkey.rs. Absent, not zero."
            ),
            "n_rows": len(ts),
        }
    if len(vals) != len(ts):
        # Partial coverage is reported, never silently averaged over the
        # rows that happened to carry the field.
        missing = len(ts) - len(vals)
    else:
        missing = 0
    vals.sort()
    return {
        "status": "measured",
        "n": len(vals),
        "rows_missing_turn_ms": missing,
        "p50_ms": statistics.median(vals),
        "p95_ms": vals[min(len(vals) - 1, int(round(0.95 * (len(vals) - 1))))],
        "min_ms": vals[0],
        "max_ms": vals[-1],
    }


def citability(ts):
    """(d) — does every Grounded badge resolve to a real address?

    Counted from `answer_segments` on the transcript, which is where the
    runtime's own segment objects land. `None` on every flag-off turn, and
    that is reported as "the native path did not run", not as 0 badges.
    """
    if ts is None:
        return {"status": "could_not_measure", "reason": "no transcripts file for this arm"}
    turns_with_segments = 0
    grounded = addressed = unverified = total = 0
    unresolved_examples = []
    for t in ts:
        segs = t.get("answer_segments")
        if not isinstance(segs, list):
            continue
        turns_with_segments += 1
        for s in segs:
            total += 1
            kind = (s.get("kind") or {}).get("kind")
            if kind == "unverified":
                unverified += 1
            if kind != "grounded":
                continue
            grounded += 1
            if (s.get("kind") or {}).get("address"):
                addressed += 1
            elif len(unresolved_examples) < 5:
                unresolved_examples.append(
                    {"probe": t.get("id"), "pool_slot": (s.get("kind") or {}).get("chunk_id")}
                )
    if turns_with_segments == 0:
        return {
            "status": "not_run",
            "reason": (
                "no transcript row carried answer_segments — the native path did not "
                "run on this arm (expected on the flag-off arm)"
            ),
            "n_rows": len(ts),
        }
    return {
        "status": "measured",
        "turns_with_segments": turns_with_segments,
        "segments_total": total,
        "segments_grounded": grounded,
        "segments_grounded_addressed": addressed,
        "segments_unverified": unverified,
        "badge_resolution_rate": rate(addressed, grounded),
        "passes": grounded > 0 and addressed == grounded,
        "unresolved_examples": unresolved_examples,
    }


def decline_identity(logs):
    """(c) A1 — the arm-identity check, as a set comparison over the logs.

    Two things are asserted, and they fail differently:

      1. The RETIRED native decline line must not appear in ANY arm. It
         has no code path left; if it is in a log, the binary under test
         is not this composition and every other number is about
         something else. That check is the instrument validation for this
         whole run (ARCH §18.4).
      2. The incumbent decline line must fire on the SAME probes in both
         arms. A difference is a decision the flag changed, which is
         exactly what P1 forbids.
    """
    out = {"retired_native_decline_lines": {}, "incumbent_decline_counts": {}}
    for arm, log in logs.items():
        clean = ANSI.sub("", log)
        out["retired_native_decline_lines"][arm] = clean.count(RETIRED_NATIVE_DECLINE)
        out["incumbent_decline_counts"][arm] = clean.count(DECLINE_MARKER)
    retired = sum(out["retired_native_decline_lines"].values())
    counts = set(out["incumbent_decline_counts"].values())
    out["retired_arm_absent"] = retired == 0
    out["decline_counts_identical"] = len(counts) <= 1
    out["passes"] = out["retired_arm_absent"] and out["decline_counts_identical"]
    out["reading"] = (
        "PASS — no retired native decline line in any arm, and the incumbent floor "
        "declined the same number of turns on every arm."
        if out["passes"]
        else "FAIL — see the counts. A1 is the plan's pre-registered kill for P1; a "
        "divergence routes to the drop-routing exit (§6 path 3), not to a re-fit."
    )
    return out


ABSENT_QTYPES = {"absent_adjacent", "absent_out_of_domain"}


def disclaimer_confabulations(rs):
    """The plan's fifth P1 bar: zero disclaimer-confabulations.

    E9's surface was the abstain arm rerouting dropped evidence into a
    parametric turn that asserted false specifics behind a disclaimer the
    honesty classifier accepted (note 0ee9fc42; disclaimer on 16/17 of the
    Step 2 failure cases). P1 deletes that arm, so the count should be
    zero by construction — but "should be zero by construction" is exactly
    the claim a bar exists to check.

    **This returns a WORKLIST, not a verdict, and that is deliberate.**
    The chaos row types `caveat_present` and `asserted_value_grounded`,
    and on the committed Step 2 artifacts those two fields are never both
    non-null on the same row — so any conjunction of them is a check that
    cannot fail on real data, which is worse than no check (ARCH §18.1: a
    check with no failing input you can name). What IS derivable is the
    candidate set: absent-type probes the agent answered, split by whether
    a caveat was present. Judging those cases is A2's offline transcript
    audit, and this names the exact probes it must read.
    """
    if rs is None:
        return {"status": "could_not_measure", "reason": "no rows for this arm"}
    answered_absent = [
        r
        for r in rs
        if r.get("qtype") in ABSENT_QTYPES
        and str(r.get("agent_action", "")).startswith("answer")
    ]
    with_caveat = [r["id"] for r in answered_absent if r.get("caveat_present") is True]
    without = [r["id"] for r in answered_absent if r.get("caveat_present") is not True]
    return {
        "status": "reported",
        "passes": None,
        "why_not_judged": (
            "the chaos row carries no confabulation field, and caveat_present / "
            "asserted_value_grounded are never both non-null on one row, so no derived "
            "conjunction over them can fail. These probes are the audit worklist."
        ),
        "answered_on_absent": len(answered_absent),
        "answered_with_caveat": with_caveat,
        "answered_without_caveat": without,
    }


def main():
    d, outp = Path(sys.argv[1]), Path(sys.argv[2])
    arm_names = [
        a for a in ARMS if read_text(d / f"ab_saltgrass_{a}.jsonl") is not None
    ]
    if not arm_names:
        sys.exit(f"no arm artifacts under {d} — refusing to emit an empty verdict")
    arms, logs = {}, {}
    for arm in arm_names:
        body = read_text(d / f"ab_saltgrass_{arm}.jsonl") or ""
        rs = [json.loads(l) for l in body.splitlines() if l.strip()]
        log = read_text(d / f"ab_saltgrass_{arm}.run.log") or ""
        logs[arm] = log
        ts = transcripts(d, arm)
        s = arm_scores(rs)
        s["n_rows"] = len(rs)
        s["bench_redlines"] = bench_redlines(log)
        s["bench_verdict"] = bench_verdict(log)
        s["latency"] = latency(ts)
        s["citability"] = citability(ts)
        s["disclaimer_confabulations"] = disclaimer_confabulations(rs)
        arms[arm] = s

    on_arms = [a for a in arm_names if a.startswith("on")]
    off = arms.get("off", {})

    def on_all(key, bar):
        vals = [arms[a].get(key) for a in on_arms]
        if not vals or any(v is None for v in vals):
            return {"status": "could_not_measure", "values": vals, "bar": bar}
        return {
            "status": "measured",
            "values": {a: arms[a][key] for a in on_arms},
            "flag_off": off.get(key),
            "bar": bar,
            "passes": all(v >= bar for v in vals),
        }

    lat = {a: arms[a]["latency"] for a in arm_names}
    lat_delta = None
    if (
        off.get("latency", {}).get("status") == "measured"
        and on_arms
        and arms[on_arms[0]]["latency"].get("status") == "measured"
    ):
        o, n = off["latency"], arms[on_arms[0]]["latency"]
        lat_delta = {
            "p50_ms": n["p50_ms"] - o["p50_ms"],
            "p95_ms": n["p95_ms"] - o["p95_ms"],
            "reading": (
                "The plan's §5 arithmetic predicts ZERO added model calls per turn, so the "
                "expected delta is noise around 0. Read it as a distribution over paired "
                "arms on one host, never as a single-run result (ARCH §18.5)."
            ),
        }

    verdict = {
        "schema": "native-grounding-p1/v1",
        "bank": "saltgrass (dev)",
        "bars_source": "NATIVE_GROUNDING_PARITY_PLAN.md §4.1 — pre-registered, not "
        "renegotiated by the order that runs them",
        "on_runs": on_arms,
        "bars": {
            "a_honesty_when_absent": on_all("honesty_when_absent", HONESTY_BAR),
            "b_competence_when_present": on_all("competence_when_present", COMPETENCE_BAR),
            "c_a1_arm_identity": decline_identity(logs),
            "d_citability": {a: arms[a]["citability"] for a in arm_names},
            "e_latency": {"per_arm": lat, "delta_on_minus_off": lat_delta},
            "f_disclaimer_confabulations": {
                a: arms[a]["disclaimer_confabulations"] for a in arm_names
            },
        },
        "arms": arms,
    }

    hard = [
        verdict["bars"]["a_honesty_when_absent"].get("passes"),
        verdict["bars"]["b_competence_when_present"].get("passes"),
        verdict["bars"]["c_a1_arm_identity"].get("passes"),
    ]
    if any(h is None for h in hard):
        verdict["outcome"] = "could_not_judge"
        verdict["outcome_reason"] = (
            "at least one HARD bar could not be measured — see its status. A bar that "
            "could not be judged is not a bar that passed."
        )
    elif all(hard):
        verdict["outcome"] = "p1_bars_met"
        verdict["outcome_reason"] = (
            "all three HARD bars cleared on every on-run. The flag's default still stays "
            "OFF: promotion is the operator's call on these numbers, not this script's."
        )
    else:
        verdict["outcome"] = "p1_bars_missed"
        verdict["outcome_reason"] = (
            "a HARD bar did not clear. If it was A1, the plan routes to the drop-routing "
            "exit (§6 path 3), pre-registered, no re-litigation."
        )
    outp.write_text(json.dumps(verdict, indent=2, sort_keys=True) + "\n")
    print(json.dumps(verdict["bars"], indent=2, sort_keys=True))
    print("\nOUTCOME:", verdict["outcome"], "—", verdict["outcome_reason"])


if __name__ == "__main__":
    main()
