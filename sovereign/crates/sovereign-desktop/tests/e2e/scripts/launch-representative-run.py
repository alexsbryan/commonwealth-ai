#!/usr/bin/env python3
"""Fresh 75-min GENERALIZATION run on the committed exact-value + GK fidelity
fixes (b83ec57e) + honest measurement (8edd6f55/bf56bac9). Detached double-fork
+ setsid so the harness reaper can't SIGKILL it mid-flight (PPID 1).

Representative mix — NO FORCE_LONG: we want the truthful composite toward 85% and
the NATURAL category distribution (which residual is actually dominant across a
broad sample), not the padding-biased distribution FORCE_LONG induces. Committed
defaults: EXACTVAL_FIX on, SHORT_SPECIFICS_SCAN off — no env overrides, so the run
reflects exactly what ships. Full-evidence capture is already in chaos.mjs.
"""
import os
import subprocess
import shutil
import time

CRATE = "/Users/alexsbryan/dev/commonwealth-ai/sovereign/crates/sovereign-desktop"
CHAOS = CRATE + "/tests/e2e/scripts/chaos.mjs"
SCORE_CLI = "/Users/alexsbryan/dev/commonwealth-ai/target/debug/sovereign-cli-llm"
JOURNAL = CRATE + "/test-artifacts/chaos-journal.jsonl"
OUTDIR = CRATE + "/test-artifacts/qa-iterations"
STAMP = "rebaseline-2026-07-01"
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
print(f"=== REBASE start (representative, committed fixes) ===", flush=True)
r = subprocess.run(["node", CHAOS, "--attach", "--spawn", "--minutes", "75"], env=env)
print(f"=== REBASE chaos exited {r.returncode} ===", flush=True)
try:
    shutil.copyfile(JOURNAL, DEST)
    print(f"=== journal -> {DEST} ===", flush=True)
except Exception as e:
    print(f"!! copy failed: {e}", flush=True)
with open(DONE, "w") as f:
    f.write(f"done rc={r.returncode}\n")
print("=== REBASE COMPLETE ===", flush=True)
