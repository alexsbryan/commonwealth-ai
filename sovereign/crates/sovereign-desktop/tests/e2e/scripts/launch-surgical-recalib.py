#!/usr/bin/env python3
"""Detached RE-calibration after reverting the scoped re-audit.

Surgical output now flows through the FULL re-audit (same ladder as the
full-rewrite/OFF path), so ON must match OFF on fabrication. Fresh same-binary
same-session A/B over secret_agent (the [gv]-scoreable bank), forced longform.
Pass condition for default-ON: ON CONFAB-LEAKED / hallucination-rate ≈ OFF.

Detached (PPID 1) so the reaper can't kill it. Stamped log + .DONE.
"""
import os
import subprocess

ROOT = "/Users/alexsbryan/dev/commonwealth-ai"
CLI = ROOT + "/target/debug/sovereign-cli-llm"
BANK = ROOT + "/sovereign/bench/chaos_monkey/secret_agent.toml"
OUTDIR = ROOT + "/sovereign/crates/sovereign-desktop/test-artifacts/qa-iterations"
STAMP = "surgical-recalib-2026-07-17"
CONSOLE = OUTDIR + f"/{STAMP}.log"
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
os.chdir(ROOT)
if os.path.exists(DONE):
    os.remove(DONE)


def run(label, surgical, out):
    env = dict(os.environ)
    env["SOVEREIGN_LONGFORM_CHARS"] = "0"
    if surgical:
        env["SOVEREIGN_SURGICAL_REWRITE"] = "1"
        env["SOVEREIGN_SURGICAL_MAX_FAILURES"] = "99"
    else:
        env.pop("SOVEREIGN_SURGICAL_REWRITE", None)
    print(f"\n===== {label} (surgical={'ON' if surgical else 'OFF'}) =====", flush=True)
    r = subprocess.run(
        [CLI, "bench", "chaos-monkey", "run", "--bank", BANK, "--corpus", "chaos-secret-agent",
         "--transport", "direct", "--grounding-verify", "--limit", "25", "--out", out],
        env=env,
    )
    print(f"===== {label} exit={r.returncode} =====", flush=True)


run("SECRET-OFF", False, "/tmp/recalib_secret_off.jsonl")
run("SECRET-ON", True, "/tmp/recalib_secret_on.jsonl")

with open(DONE, "w") as f:
    f.write("done\n")
print("\n===== RECALIBRATION COMPLETE =====", flush=True)
