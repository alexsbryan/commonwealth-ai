#!/usr/bin/env python3
"""90-min measurement run — quantify the class-A grounding + leak fixes (Fixes 1,2,F
landed 2026-07-08) against the 8h baseline (raw 82% / calibrated 88%). Same
discipline as launch-representative-run.py: representative mix, NO FORCE_LONG,
committed defaults (no env overrides), full-evidence capture. Detached double-fork
+ setsid (PPID 1, reaper-immune). Writes a stamped journal + .DONE sentinel;
--attach --spawn wanders the resident corpora and drives the 35B on :9741 as SUT +
brain/judge.
"""
import os
import subprocess
import shutil

CRATE = "/Users/alexsbryan/dev/commonwealth-ai/sovereign/crates/sovereign-desktop"
CHAOS = CRATE + "/tests/e2e/scripts/chaos.mjs"
SCORE_CLI = "/Users/alexsbryan/dev/commonwealth-ai/target/debug/sovereign-cli-llm"
JOURNAL = CRATE + "/test-artifacts/chaos-journal.jsonl"
OUTDIR = CRATE + "/test-artifacts/qa-iterations"
STAMP = "run90-2026-07-08"
MINUTES = 90
CONSOLE = OUTDIR + f"/{STAMP}.console.log"
DEST = OUTDIR + f"/{STAMP}.jsonl"
DONE = OUTDIR + f"/{STAMP}.DONE"

if os.fork() > 0:
    os._exit(0)
os.setsid()
if os.fork() > 0:
    os._exit(0)

logfd = os.open(CONSOLE, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
os.dup2(logfd, 1)
os.dup2(logfd, 2)
os.dup2(os.open(os.devnull, os.O_RDONLY), 0)
os.chdir(CRATE)
if os.path.exists(DONE):
    os.remove(DONE)

env = dict(os.environ)
env["SOVEREIGN_SCORE_CLI"] = SCORE_CLI
# committed defaults — no EXACTVAL_FIX / SHORT_SPECIFICS_SCAN overrides.
print(f"=== 90m run start (measurement, Fixes 1/2/F), minutes={MINUTES} ===", flush=True)
r = subprocess.run(["node", CHAOS, "--attach", "--spawn", "--minutes", str(MINUTES)], env=env)
print(f"=== 90m chaos exited {r.returncode} ===", flush=True)
try:
    shutil.copyfile(JOURNAL, DEST)
    print(f"=== journal -> {DEST} ===", flush=True)
except Exception as e:
    print(f"!! copy failed: {e}", flush=True)
with open(DONE, "w") as f:
    f.write(f"done rc={r.returncode}\n")
print("=== 90m COMPLETE ===", flush=True)
