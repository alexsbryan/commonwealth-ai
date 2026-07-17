#!/usr/bin/env python3
"""8h DUAL-MODE robustness run (2026-07-16) — pre-launch hardening signal.

Two sequential 4h phases against the SAME already-running dev daemon on :9741
(--attach), each spawning its own scratch desktop bridge (--spawn):

  PHASE 1 — chaos.mjs : the "demanding frontier-LLM power user" (BRAIN_SYSTEM).
            Open-domain, invents hard questions, stresses every surface. Proven
            8h backbone (see launch-8h-run.py). minutes=240.
  PHASE 2 — personas.mjs : six standing personas whose goals are CORPUS-SOURCED
            (in_corpus goals drawn from the resident catalog corpora). Time-bound
            (--sessions 0), minutes=240.

Same discipline as launch-8h-run.py: committed defaults, no env overrides beyond
SOVEREIGN_SCORE_CLI, detached double-fork + setsid (PPID 1, reaper-immune). Each
phase writes its OWN stamped journal; a single .DONE sentinel lands at the end
with both return codes. The daemon is unsupervised — a mid-run crash is an
operator concern (restart it so the in-flight phase keeps a live SUT).
"""
import os
import subprocess
import shutil
import time

CRATE = "/Users/alexsbryan/dev/commonwealth-ai/sovereign/crates/sovereign-desktop"
CHAOS = CRATE + "/tests/e2e/scripts/chaos.mjs"
PERSONAS = CRATE + "/tests/e2e/scripts/personas.mjs"
SCORE_CLI = "/Users/alexsbryan/dev/commonwealth-ai/target/debug/sovereign-cli-llm"
CHAOS_JOURNAL = CRATE + "/test-artifacts/chaos-journal.jsonl"
PERSONA_JOURNAL = CRATE + "/test-artifacts/persona-journal.jsonl"
OUTDIR = CRATE + "/test-artifacts/qa-iterations"
STAMP = "run8h-2026-07-16"
PHASE1_MIN = 240
PHASE2_MIN = 240
CONSOLE = OUTDIR + f"/{STAMP}.console.log"
CHAOS_DEST = OUTDIR + f"/{STAMP}-chaos.jsonl"
PERSONA_DEST = OUTDIR + f"/{STAMP}-personas.jsonl"
DONE = OUTDIR + f"/{STAMP}.DONE"

# --- detach: double-fork + setsid so the harness reaper can't SIGKILL us (PPID 1)
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
# committed defaults — no EXACTVAL_FIX / SHORT_SPECIFICS_SCAN / FORCE_LONG overrides.


def run_phase(name, cmd, journal, dest, minutes):
    # Truncate the source journal so the stamped copy is PURE this phase.
    # (personas.mjs wipes its own on start; chaos.mjs appends — truncate both.)
    try:
        open(journal, "w").close()
    except Exception as e:  # noqa: BLE001
        print(f"!! {name}: could not truncate {journal}: {e}", flush=True)
    print(f"=== {name} START minutes={minutes} epoch={int(time.time())} ===", flush=True)
    r = subprocess.run(cmd, env=env)
    print(f"=== {name} EXIT rc={r.returncode} epoch={int(time.time())} ===", flush=True)
    try:
        shutil.copyfile(journal, dest)
        print(f"=== {name} journal -> {dest} ===", flush=True)
    except Exception as e:  # noqa: BLE001
        print(f"!! {name}: journal copy failed: {e}", flush=True)
    return r.returncode


rc1 = run_phase(
    "PHASE1-chaos-frontier",
    ["node", CHAOS, "--attach", "--spawn", "--minutes", str(PHASE1_MIN)],
    CHAOS_JOURNAL,
    CHAOS_DEST,
    PHASE1_MIN,
)

rc2 = run_phase(
    "PHASE2-personas-corpus",
    ["node", PERSONAS, "--attach", "--spawn", "--sessions", "0", "--minutes", str(PHASE2_MIN)],
    PERSONA_JOURNAL,
    PERSONA_DEST,
    PHASE2_MIN,
)

with open(DONE, "w") as f:
    f.write(f"done chaos_rc={rc1} personas_rc={rc2} epoch={int(time.time())}\n")
print(f"=== 8H DUAL COMPLETE chaos_rc={rc1} personas_rc={rc2} ===", flush=True)
