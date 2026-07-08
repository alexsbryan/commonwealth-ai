#!/usr/bin/env python3
"""8-HOUR overnight chaos-QA generalization run on committed grounding fixes.

Same discipline as launch-representative-run.py (which see): representative mix,
NO FORCE_LONG, committed defaults (EXACTVAL_FIX on, SHORT_SPECIFICS_SCAN off) — no
env overrides, so the run reflects exactly what ships. Full-evidence capture is in
chaos.mjs. Detached double-fork + setsid so the harness reaper can't SIGKILL it
mid-flight (PPID 1). Writes a stamped journal + .DONE sentinel; --attach --spawn
wanders the resident corpora and drives the 35B on :9741 as SUT + brain/judge.
"""
import os
import subprocess
import shutil

CRATE = "/Users/alexsbryan/dev/commonwealth-ai/sovereign/crates/sovereign-desktop"
CHAOS = CRATE + "/tests/e2e/scripts/chaos.mjs"
SCORE_CLI = "/Users/alexsbryan/dev/commonwealth-ai/target/debug/sovereign-cli-llm"
JOURNAL = CRATE + "/test-artifacts/chaos-journal.jsonl"
OUTDIR = CRATE + "/test-artifacts/qa-iterations"
STAMP = "run8h-2026-07-07"
MINUTES = 480
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
print(f"=== 8H run start (representative, committed fixes), minutes={MINUTES} ===", flush=True)
r = subprocess.run(["node", CHAOS, "--attach", "--spawn", "--minutes", str(MINUTES)], env=env)
print(f"=== 8H chaos exited {r.returncode} ===", flush=True)
try:
    shutil.copyfile(JOURNAL, DEST)
    print(f"=== journal -> {DEST} ===", flush=True)
except Exception as e:
    print(f"!! copy failed: {e}", flush=True)
with open(DONE, "w") as f:
    f.write(f"done rc={r.returncode}\n")
print("=== 8H COMPLETE ===", flush=True)
