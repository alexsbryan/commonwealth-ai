#!/usr/bin/env bash
# Run-request 1: grounding-gate latency census repro (order grounding-gate-latency-census).
# SUBSTITUTION NAMED: BeefyMac's host-local ~/.sovereign/comaintainer/gate-census.py does
# not exist on RuggedFox; this implements the census note's own repro recipe
# (c5d16402): join `routing outcome` + `inference.complete` lines from the daemon log.
set -euo pipefail
python3 - "$HOME/.svrnmesh/logs/daemon.err" <<'PY'
import sys, re, collections
pat_route = re.compile(r"routing outcome .* oicp_request_id=(\S+) total_ms=Some\(([0-9.]+)")
pat_done  = re.compile(r"inference\.complete: .* tokens_used=(\S+) response_chars=(\S+)")
rows = collections.defaultdict(list)
for line in open(sys.argv[1], errors="ignore"):
    m = pat_route.search(line)
    if m: rows[m.group(1)].append(("route", float(m.group(2))))
    m = pat_done.search(line)
    if m: rows[m.group(1)].append(("done", m.group(1), m.group(2)))
n = sum(1 for k, v in rows.items() if any(x[0] == "route" for x in v))
print(f"gate-census repro: {n} timed routing calls joined in {sys.argv[1]}")
PY
