#!/usr/bin/env python3
"""Deterministic temp-0 replay of the full rebaseline-2026-07-01 bank (36 unique
questions) against the citation-fidelity fix (snap garbled [Source:] labels /
ID-token veto in citation_attribution.rs). Detached double-fork + setsid so the
harness reaper can't SIGKILL it mid-flight (PPID 1).

Paired replay per the methodology: same questions as the 65% rebaseline, fixed
app, temp 0. Gate trace + synth.citation targets ON so snap/strip events are
observable in the app log (glassbox proof of WHICH mechanism fired).
"""
import os
import subprocess
import shutil

CRATE = "/Users/alexsbryan/dev/commonwealth-ai/sovereign/crates/sovereign-desktop"
CHAOS = CRATE + "/tests/e2e/scripts/chaos.mjs"
SCORE_CLI = "/Users/alexsbryan/dev/commonwealth-ai/target/debug/sovereign-cli-llm"
JOURNAL = CRATE + "/test-artifacts/chaos-journal.jsonl"
APPLOG = CRATE + "/test-artifacts/chaos-app.log"
OUTDIR = CRATE + "/test-artifacts/qa-iterations"
BANK = OUTDIR + "/citefix-replay.bank.jsonl"
STAMP = "citefix-replay-2026-07-01"
CONSOLE = OUTDIR + f"/{STAMP}.console.log"
DEST = OUTDIR + f"/{STAMP}.jsonl"
DEST_APPLOG = OUTDIR + f"/{STAMP}.app.log"
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
env["SOVEREIGN_CHAOS_REPLAY"] = BANK
env["SOVEREIGN_SYNTH_TEMP"] = "0"
env["SOVEREIGN_AGENTIC_KQ_DEBUG"] = "1"
env["RUST_LOG"] = (
    "sovereign_desktop=info,sovereign_core=info,grounding_gate=debug,"
    "synth.citation=info,gate.lifecycle=info,sovereign_inference=info"
)
print("=== CITEFIX REPLAY start (temp-0, full rebaseline bank, gate trace) ===", flush=True)
r = subprocess.run(["node", CHAOS, "--attach", "--spawn", "--minutes", "120"], env=env)
print(f"=== CITEFIX REPLAY chaos exited {r.returncode} ===", flush=True)
for src, dst in ((JOURNAL, DEST), (APPLOG, DEST_APPLOG)):
    try:
        shutil.copyfile(src, dst)
        print(f"=== {src} -> {dst} ===", flush=True)
    except Exception as e:
        print(f"!! copy failed ({src}): {e}", flush=True)
with open(DONE, "w") as f:
    f.write(f"done rc={r.returncode}\n")
print("=== CITEFIX REPLAY COMPLETE ===", flush=True)
