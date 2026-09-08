#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""admission_shed_probe — N chat turns under one concurrent `chat ask`.

THE MEASUREMENT the order `admission-continuation` re-runs, made repeatable.
It was ad hoc when it produced note `d6e13797` (32 turns, 5 of 5
`judge_failed_open` exits `queue_shed` with zero judging calls answered, plus
5 turns that died whole at the draft), which is why the fix could not be
judged against it without re-taking both sides.

WHAT IT RECORDS, per turn: wall clock, exit status, the gate's action, the
turn's `judge_failure` block (`reason` / `calls_attempted` / `calls_answered`),
the epistemic ledger's verification tallies, and the host's 1-minute load at
the moment the turn started. Plus, per ARM, the daemon log's own census of
`inference.queue: SHED` and `inference.queue: PARK` lines emitted inside the
arm's window.

HOW TO READ IT (seat direction, 2026-09-07). Run the two arms BACK TO BACK on
whatever contention the host has, and compare them to EACH OTHER — never to a
stored number from another day, and never wait for a quiet host: a baseline
minted on an artificially quiet machine measures a condition that never
occurs in use. Load cancels out of a paired comparison. The headline is the
COUNTERS (`queue_shed`, whole-turn deaths, judge calls answered), which do not
move with load; the wall-clock ratio is a supporting row, reported with the
load each arm actually saw.

Usage:
  scripts/admission_shed_probe.py --arm before --turns 32 \\
      --out target/admission-probe/before.jsonl
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import pathlib
import re
import statistics
import subprocess
import sys
import time

REPO = pathlib.Path(__file__).resolve().parent.parent
CLI = REPO / "target" / "debug" / "sovereign-cli"
DAEMON_ERR = pathlib.Path.home() / ".svrnmesh" / "logs" / "daemon.err"

# Eight questions, asked round-robin. Each is answerable from `sep` and each
# reaches the grounding gate — the point of the probe is the gate's calls, so
# a bank of one-liners the router sends to a Simple handler would measure
# nothing. Fixed here rather than passed in, so two arms cannot be run
# against different work.
BANK = [
    "According to the passages, what is compatibilism and what is the main objection to it?",
    "According to the passages, what is the Consequence Argument and who formulated it?",
    "Explain what the passages say about the principle of alternate possibilities.",
    "According to the passages, how do libertarians about free will answer determinism?",
    "What do the passages say about moral responsibility and the ability to do otherwise?",
    "Explain what the passages say about Frankfurt-style cases and what they are meant to show.",
    "According to the passages, what is the difference between leeway and source incompatibilism?",
    "What do the passages say about the relationship between determinism and predictability?",
]

# The concurrent load: ONE `chat ask` in a loop, which is the condition the
# original measurement ran under. Deliberately a real turn rather than a
# synthetic request generator — it is the primary slot's own workload that
# makes the queue deep enough to shed.
LOAD_QUESTION = (
    "Explain in detail everything the passages say about free will, determinism, "
    "moral responsibility, and the arguments on each side."
)


def load1() -> float:
    try:
        return os.getloadavg()[0]
    except OSError:
        return float("nan")


# The log's own vocabulary. Counted by TIMESTAMP window, not by byte offset:
# the daemon copy-truncates its log past a size cap, and the first run of this
# probe (2026-09-07) rotated mid-arm, which silently zeroed one arm's census
# and made the other read a stale window. An instrument whose reading depends
# on a file not being rotated is not one (ARCH §18.4).
LOG_EVENTS = {
    "shed_pre_park": "inference.queue: SHED — predicted wait",
    "shed_after_park": "inference.queue: SHED after parking",
    "park_admitted": "inference.queue: PARK",
    "contended_park": "inference.queue: slot busy, waiting for permit",
    "turn_admitted": "inference.admission: turn admitted",
    "turn_unadmitted": "inference.admission: turn opened with no foreground",
    # A daemon that DIED inside the arm invalidates every turn after it — see
    # `DeathKind.DAEMON_GONE`.
    "daemon_shutdown": "daemon: shutdown signal received",
    "daemon_up": "svrn daemon is running",
}

TS_RE = re.compile(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})")


def census_log(lo_utc: str, hi_utc: str) -> dict:
    """Count the queue's own events inside this arm's window.

    Read from the daemon's log rather than inferred from the client side: a
    turn that survived because its call PARKED and one that survived because
    the slot happened to be free look identical from outside.

    Reads the live log AND any `.bak` copies the rotation left, so a rotation
    inside the arm costs nothing.
    """
    counts = {k: 0 for k in LOG_EVENTS}
    files = [DAEMON_ERR] + sorted(DAEMON_ERR.parent.glob(DAEMON_ERR.name + ".*.bak"))
    seen_any = False
    for f in files:
        if not f.exists():
            continue
        seen_any = True
        with f.open(encoding="utf-8", errors="replace") as fh:
            for line in fh:
                m = TS_RE.search(line)
                if not m or not (lo_utc <= m.group(1) < hi_utc):
                    continue
                for k, pat in LOG_EVENTS.items():
                    if pat in line:
                        counts[k] += 1
    counts["log_readable"] = seen_any
    return counts


# How a turn failed. A closed set, matched on the daemon's own words rather
# than guessed at — and `other` is a real verdict, not a bucket to hide in.
DEATH_SHED = "shed"                  # the host refused: "host busy: ~N ms predicted wait"
DEATH_DAEMON_GONE = "daemon_gone"    # nothing was listening: the daemon died or is restarting
DEATH_TRANSPORT = "transport"        # the connection broke mid-turn
DEATH_CLIENT_TIMEOUT = "client_timeout"
DEATH_OTHER = "other"


def classify_death(err: str) -> str:
    low = err.lower()
    if "host busy" in low or "queue position" in low:
        return DEATH_SHED
    if "error sending request" in low or "connection refused" in low:
        return DEATH_DAEMON_GONE
    if "websocket" in low or "connection reset" in low or "broken pipe" in low:
        return DEATH_TRANSPORT
    if "client timeout" in low:
        return DEATH_CLIENT_TIMEOUT
    return DEATH_OTHER


def ask(question: str, corpus: str, timeout: int) -> dict:
    started = time.monotonic()
    load_at_start = load1()
    try:
        p = subprocess.run(
            [str(CLI), "chat", "ask", "--corpus", corpus, "--format", "json", question],
            capture_output=True,
            text=True,
            timeout=timeout,
            env=os.environ | {"SOVEREIGN_NO_STALE_WARN": "1"},
        )
        rc, out, err = p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        rc, out, err = 124, "", f"client timeout after {timeout}s"
    wall_ms = int((time.monotonic() - started) * 1000)

    rec = {
        "wall_ms": wall_ms,
        "exit": rc,
        "load1_at_start": load_at_start,
        "question": question[:60],
    }
    if rc != 0 or not out.strip():
        # A turn that produced no answer at all — and WHICH KIND, because a
        # shed and a dead daemon take different fixes and the first run of
        # this probe proved they do not report themselves apart. A jetsam
        # kill 16 turns into an arm produced 16 connection-refused failures
        # in 600 ms, and the summary counted every one as a "whole-turn
        # death" alongside the four real `host busy` refusals. That reading
        # would have credited the fix with 16 deaths it never caused
        # (ARCH §18.3 — never let one class wear another's name).
        rec["died"] = True
        tail = [ln for ln in err.strip().splitlines() if ln.strip()]
        rec["error"] = tail[-1][:300] if tail else "(no stderr)"
        rec["death_kind"] = classify_death(rec["error"])
        return rec

    rec["died"] = False
    try:
        d = json.loads(out)
    except json.JSONDecodeError as e:
        rec["died"] = True
        rec["error"] = f"unparseable answer json: {e}"
        return rec

    meta = d.get("metadata") or {}
    gate = meta.get("grounding_gate") or {}
    rec["gate_action"] = gate.get("action")
    rec["routed_intent"] = meta.get("routed_intent")
    jf = gate.get("judge_failure")
    if jf:
        rec["judge_failure"] = jf
    ep = d.get("epistemic_state") or {}
    holdings = ep.get("holdings") or []
    rec["holdings"] = len(holdings)
    rec["verifications"] = sorted({str(h.get("verification")) for h in holdings})
    rec["answer_chars"] = len(d.get("visible") or "")
    return rec


def summarise(arm: str, rows: list[dict], log: dict, elapsed_s: float) -> dict:
    ok = [r for r in rows if not r["died"]]
    died = [r for r in rows if r["died"]]
    walls = sorted(r["wall_ms"] for r in ok)
    shed = [
        r
        for r in rows
        if (r.get("judge_failure") or {}).get("reason") == "queue_shed"
    ]
    unanswered = [
        r
        for r in rows
        if (r.get("judge_failure") or {}).get("calls_answered") == 0
    ]
    loads = sorted(r["load1_at_start"] for r in rows)

    def pct(xs, q):
        if not xs:
            return None
        i = min(len(xs) - 1, int(round(q * (len(xs) - 1))))
        return xs[i]

    from collections import Counter

    kinds = Counter(r["death_kind"] for r in died)
    # An UNPLANNED exit inside the window. The `daemon_up` line at the start
    # of an arm is the operator's own restart and must not read as a kill —
    # keying on it would mark every arm compromised, which is the same
    # blindness as marking none.
    restarts = log.get("daemon_shutdown", 0)
    return {
        "arm": arm,
        "turns": len(rows),
        # THE number the order calls "whole-turn deaths" is the shed one.
        # The others are reported beside it, never folded in.
        "died_shed": kinds.get(DEATH_SHED, 0),
        "died_daemon_gone": kinds.get(DEATH_DAEMON_GONE, 0),
        "died_transport": kinds.get(DEATH_TRANSPORT, 0),
        "died_other": kinds.get(DEATH_CLIENT_TIMEOUT, 0) + kinds.get(DEATH_OTHER, 0),
        "died_whole": len(died),
        # An arm the daemon did not survive did not measure what it says it
        # measured: every turn after the kill failed on a closed socket. The
        # arm is REPORTED as compromised rather than averaged (ARCH §18.3).
        "daemon_restarts_in_arm": restarts,
        "turns_that_reached_the_daemon": len(rows) - kinds.get(DEATH_DAEMON_GONE, 0),
        "arm_verdict": (
            f"compromised: the daemon exited {restarts} time(s) inside the window — "
            "every turn after a kill failed on a closed socket and measured nothing"
            if restarts
            else "clean"
        ),
        "judge_failed_open": sum(1 for r in rows if r.get("judge_failure")),
        "judge_failure_queue_shed": len(shed),
        "judge_failure_calls_answered_zero": len(unanswered),
        "judge_failure_reasons": sorted(
            {(r.get("judge_failure") or {}).get("reason") for r in rows if r.get("judge_failure")}
        ),
        "gate_actions": sorted({r.get("gate_action") for r in ok if r.get("gate_action")}),
        "wall_p50_ms": pct(walls, 0.50),
        "wall_p90_ms": pct(walls, 0.90),
        "wall_min_ms": walls[0] if walls else None,
        "wall_max_ms": walls[-1] if walls else None,
        "load1_median": statistics.median(loads) if loads else None,
        "load1_min": loads[0] if loads else None,
        "load1_max": loads[-1] if loads else None,
        "daemon_log": log,
        "elapsed_s": round(elapsed_s, 1),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", required=True, help="label for this side of the pair")
    ap.add_argument("--turns", type=int, default=32)
    ap.add_argument("--corpus", default="sep")
    ap.add_argument("--out", required=True)
    ap.add_argument("--timeout", type=int, default=300)
    ap.add_argument(
        "--no-load",
        action="store_true",
        help="skip the concurrent `chat ask` (a control arm, not the measurement)",
    )
    a = ap.parse_args()

    if not CLI.exists():
        print(f"missing {CLI} — build it first", file=sys.stderr)
        return 2

    out = pathlib.Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)

    loader = None
    if not a.no_load:
        # One concurrent `chat ask`, re-fired as soon as it returns. Its own
        # results are not scored; it is the contention.
        loader = subprocess.Popen(
            [
                "/bin/sh",
                "-c",
                f'while :; do "{CLI}" chat ask --corpus {a.corpus} '
                f'--format json "{LOAD_QUESTION}" >/dev/null 2>&1 || true; done',
            ],
            env=os.environ | {"SOVEREIGN_NO_STALE_WARN": "1"},
            start_new_session=True,
        )
        time.sleep(2)

    # UTC minute stamps bracket the arm; the daemon logs in UTC.
    arm_lo = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
    t0 = time.monotonic()
    rows: list[dict] = []
    try:
        with out.open("w") as fh:
            for i in range(a.turns):
                rec = ask(BANK[i % len(BANK)], a.corpus, a.timeout)
                rec["arm"] = a.arm
                rec["i"] = i
                rows.append(rec)
                fh.write(json.dumps(rec) + "\n")
                fh.flush()
                print(
                    f"[{a.arm}] {i + 1}/{a.turns} {rec['wall_ms']}ms "
                    f"exit={rec['exit']} gate={rec.get('gate_action')} "
                    f"jf={(rec.get('judge_failure') or {}).get('reason')} "
                    f"load={rec['load1_at_start']:.1f}",
                    flush=True,
                )
    finally:
        if loader is not None:
            try:
                os.killpg(os.getpgid(loader.pid), 15)
            except (ProcessLookupError, PermissionError):
                loader.terminate()

    arm_hi = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
    summary = summarise(a.arm, rows, census_log(arm_lo, arm_hi), time.monotonic() - t0)
    summary["window_utc"] = [arm_lo, arm_hi]
    out.with_suffix(".summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
