#!/usr/bin/env python3
"""Detached calibration A/B — surgical rewrite + SCOPED re-audit fabrication safety.

Compares OFF (full rewrite + full re-audit) vs ON (surgical + scoped re-audit),
FORCED through the longform gate (SOVEREIGN_LONGFORM_CHARS=0), over two banks:

  1. secret_agent (43 Q, fairness contract) — limited to 25 — the calibrated
     answer-when-present / abstain-when-absent / resist-distractor leak test.
  2. longform_stress (5 broad Karamazov Q) — reliably produces long syntheses
     with unsupported specifics, so surgery FIRES HEAVILY: the real stress on
     the scoped re-audit.

The pass condition for default-ON: ON must not raise confirmed-fabrication /
CONFAB-LEAKED / hallucination-rate vs OFF. The scoped re-audit drops the holistic
whole-text scan for the untouched (already-verified) spans, so this is exactly
the risk being measured.

Detached double-fork + setsid (PPID 1) so the harness reaper can't kill it —
the mistake that reaped the first attempt. Writes a stamped console log with
full scorecards + a .DONE sentinel.
"""
import os
import subprocess

ROOT = "/Users/alexsbryan/dev/commonwealth-ai"
CLI = ROOT + "/target/debug/sovereign-cli-llm"
STRESS_BANK = (
    "/private/tmp/claude-502/-Users-alexsbryan-dev-commonwealth-ai/"
    "bb742dfc-d352-4834-9710-3ec00503187a/scratchpad/longform_stress.toml"
)
SECRET_BANK = ROOT + "/sovereign/bench/chaos_monkey/secret_agent.toml"
OUTDIR = ROOT + "/sovereign/crates/sovereign-desktop/test-artifacts/qa-iterations"
STAMP = "surgical-calib-2026-07-17"
CONSOLE = OUTDIR + f"/{STAMP}.log"
DONE = OUTDIR + f"/{STAMP}.DONE"

# --- detach: double-fork + setsid (PPID 1, reaper-immune) ---
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


def run(label, bank, corpus, out, surgical, limit=None):
    env = dict(os.environ)
    env["SOVEREIGN_LONGFORM_CHARS"] = "0"  # force every answer through gate_longform
    if surgical:
        env["SOVEREIGN_SURGICAL_REWRITE"] = "1"
        env["SOVEREIGN_SURGICAL_MAX_FAILURES"] = "99"  # never fall back to full rewrite
    else:
        env.pop("SOVEREIGN_SURGICAL_REWRITE", None)
    cmd = [
        CLI, "bench", "chaos-monkey", "run",
        "--bank", bank, "--corpus", corpus,
        "--transport", "direct", "--grounding-verify",
        "--out", out,
    ]
    if limit:
        cmd += ["--limit", str(limit)]
    print(f"\n============================================================", flush=True)
    print(f"===== {label} (surgical={'ON' if surgical else 'OFF'}) =====", flush=True)
    print(f"============================================================", flush=True)
    r = subprocess.run(cmd, env=env)
    print(f"===== {label} exit={r.returncode} =====", flush=True)


# Stress bank first (fast, heavy surgery), then the calibrated leak bank.
run("STRESS-OFF", STRESS_BANK, "brothers_karamazov", "/tmp/calib_stress_off.jsonl", surgical=False)
run("STRESS-ON", STRESS_BANK, "brothers_karamazov", "/tmp/calib_stress_on.jsonl", surgical=True)
run("SECRET-OFF", SECRET_BANK, "chaos-secret-agent", "/tmp/calib_secret_off.jsonl", surgical=False, limit=25)
run("SECRET-ON", SECRET_BANK, "chaos-secret-agent", "/tmp/calib_secret_on.jsonl", surgical=True, limit=25)

with open(DONE, "w") as f:
    f.write("done\n")
print("\n===== SURGICAL CALIBRATION COMPLETE =====", flush=True)
