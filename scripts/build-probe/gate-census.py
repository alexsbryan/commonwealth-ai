#!/usr/bin/env python3
"""gate-census — what quality checking actually cost the sessions.

Streams the Claude Code transcripts for this repo (~/.claude/projects/<repo>/
*.jsonl, subagents included) modified in the last N days, finds every Bash
tool call that runs a gate, pairs it with its tool_result by id, and reports
per shape: how many runs, seconds total, median and p90, and how many
sessions ran it. The duration is result-timestamp minus call-timestamp, i.e.
what the session actually waited, tail and all.

Shapes: lint-scoped / lint-full (sovereign-lint.sh), test-scoped / test-full
(sovereign-test.sh), cargo-raw (bare cargo build/check/test/clippy/nextest),
pre-push, xtask (cargo xtask …), quality-check (svrn/sovereign quality check),
bench (ci-bench / bench lanes).

    python3 scripts/build-probe/gate-census.py [--days 30] [--json]
"""
import glob, json, os, re, statistics, sys, time
from collections import defaultdict

DAYS = int(sys.argv[sys.argv.index("--days") + 1]) if "--days" in sys.argv else 30
AS_JSON = "--json" in sys.argv
TDIR = os.path.expanduser("~/.claude/projects/-home-alexbryan-dev-commonwealth-ai")

SHAPES = [
    ("lint-full", re.compile(r"sovereign-lint\.sh[^|;&\n]*--full")),
    ("lint-scoped", re.compile(r"sovereign-lint\.sh")),
    ("test-scoped", re.compile(r"sovereign-test\.sh[^|;&\n]*(--package|--filter|-p )")),
    ("test-full", re.compile(r"sovereign-test\.sh")),
    ("pre-push", re.compile(r"pre-push\.sh")),
    ("quality-check", re.compile(r"\b(svrn|sovereign)\s+quality\s+check")),
    ("bench", re.compile(r"(sovereign-ci-bench\.sh|\bbench\b.*--lane|\bsvrn bench|\bsovereign bench)")),
    ("xtask", re.compile(r"cargo\s+xtask")),
    ("cargo-raw", re.compile(r"\bcargo\s+(build|check|test|clippy|nextest)\b")),
]


READS = re.compile(r"\b(cat|sed|grep|head|tail|less|awk|wc|rg|bat|diff|python3 -|shellcheck|bash -n)\b[^|;&\n]*$")


def shape_of(cmd):
    for name, pat in SHAPES:
        m = pat.search(cmd)
        if m:
            # a command that only READS the gate script (sed -n, grep, bash -n)
            # is not a gate run; look at the shell segment the match sits in
            seg = re.split(r"[|;&\n]", cmd[: m.start()])[-1] + cmd[m.start(): m.end()]
            if READS.search(seg):
                return None
            return name
    return None


def ts(s):
    # 2026-09-17T18:41:25.123Z
    try:
        return time.mktime(time.strptime(s[:19], "%Y-%m-%dT%H:%M:%S"))
    except Exception:
        return None


def main():
    cutoff = time.time() - DAYS * 86400
    files = [f for f in glob.glob(TDIR + "/**/*.jsonl", recursive=True) if os.path.getmtime(f) >= cutoff]
    runs = []  # (shape, seconds, session, cmd)
    sessions_seen = set()
    for f in files:
        sid = os.path.basename(f).split(".")[0]
        pending = {}
        try:
            fh = open(f, errors="replace")
        except OSError:
            continue
        for line in fh:
            if '"tool_use"' not in line and '"tool_result"' not in line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            msg = rec.get("message") or {}
            content = msg.get("content") if isinstance(msg, dict) else None
            if not isinstance(content, list):
                continue
            t = ts(rec.get("timestamp", "")) if rec.get("timestamp") else None
            for c in content:
                if not isinstance(c, dict):
                    continue
                if c.get("type") == "tool_use" and c.get("name") == "Bash":
                    cmd = (c.get("input") or {}).get("command", "")
                    sh = shape_of(cmd)
                    if sh and t:
                        pending[c["id"]] = (sh, t, cmd)
                elif c.get("type") == "tool_result" and c.get("tool_use_id") in pending and t:
                    sh, t0, cmd = pending.pop(c["tool_use_id"])
                    if t >= t0:
                        runs.append((sh, t - t0, sid, cmd))
                        sessions_seen.add(sid)
    by = defaultdict(list)
    for sh, s, sid, cmd in runs:
        by[sh].append((s, sid))
    total = sum(s for _, s, _, _ in runs)
    out = {"days": DAYS, "transcripts": len(files), "sessions_with_gates": len(sessions_seen),
           "gate_runs": len(runs), "gate_seconds": round(total), "shapes": {}}
    for sh in sorted(by, key=lambda k: -sum(s for s, _ in by[k])):
        secs = sorted(s for s, _ in by[sh])
        out["shapes"][sh] = {"runs": len(secs), "seconds": round(sum(secs)), "median_s": round(statistics.median(secs), 1),
                             "p90_s": round(secs[int(len(secs) * 0.9) - 1] if len(secs) > 1 else secs[0], 1),
                             "sessions": len({sid for _, sid in by[sh]}), "share": round(sum(secs) / total, 3) if total else 0}
    if AS_JSON:
        print(json.dumps(out, indent=1))
        return
    print(f"last {DAYS} days: {len(files)} transcripts, {len(sessions_seen)} sessions ran a gate, "
          f"{len(runs)} gate runs, {total/3600:.1f} h waited on gates")
    print(f"{'shape':14s} {'runs':>5s} {'hours':>6s} {'share':>6s} {'median':>7s} {'p90':>7s} {'sessions':>8s}")
    for sh, v in out["shapes"].items():
        print(f"{sh:14s} {v['runs']:5d} {v['seconds']/3600:6.1f} {v['share']*100:5.0f}% {v['median_s']:7.0f} {v['p90_s']:7.0f} {v['sessions']:8d}")


if __name__ == "__main__":
    main()
