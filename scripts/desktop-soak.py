#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Desktop chaos + persona soak — the canonical, parameterized regression runner.

ONE command to exercise the full desktop QA harness end-to-end against your
resident daemon + corpora. No more spelunking through tests/e2e/scripts to
remember which .mjs to run with which flags:

    scripts/desktop-soak.py 150                # 150-min DUAL soak (chaos + personas)
    scripts/desktop-soak.py 90 --mode chaos    # 90-min chaos-only (the "brain" examiner)
    scripts/desktop-soak.py 60 --mode persona  # 60-min persona-only (personas + coach turns)
    scripts/desktop-soak.py 40 --no-build --no-restart --foreground   # quick attached run

RUN IT INSIDE THE `sovereign-vulkan` TOOLBOX, NOT ON THE FEDORA HOST:

    toolbox run -c sovereign-vulkan scripts/desktop-soak.py 480

The host cannot compile (llama-cpp-sys-4 finds no clang) AND cannot run the
desktop app: libappindicator-sys panics at startup because neither
libayatana-appindicator3 nor libappindicator3 is installed there. The failure
is silent-shaped — preflight passes against the resident daemon, the phase
logs START, then every spawn dies in the app log while the runner waits out
its full 4-min bridge timeout and moves on to the next phase. Cost the
operator an 8-hour run on 2026-08-06. The toolbox has the library and
inherits DISPLAY/WAYLAND_DISPLAY, so the app comes up there.

Roles (this is the "coach, brain, etc" harness):
  brain   -> chaos.mjs      : the single most demanding user. Invents hard,
                              specific questions and JUDGES the answer from the
                              user's seat. A hallucination (value asserted but
                              absent from evidence) is the cardinal sin. This is
                              the adversarial examiner that finds real bugs.
  coach   -> personas.mjs   : six standing personas with corpus-sourced goals,
  personas                    plus COACH teaching turns and the gap/refusal/
                              web-search boundary. The real-user surface.
  eyes    -> the glassbox    : every emitted event + trace-level app log + the
                              actual answer the user got back.
  oracle  -> the bench's own grounding verdict (assess_asserted_value), shared
                              with the live grounding gate — not a fuzzer's idea
                              of "wrong."

Modes:
  dual    (default) chaos for the first --split fraction, personas for the rest.
  chaos             chaos.mjs for the whole duration.
  persona           personas.mjs for the whole duration.

What it does, every step logged (glassbox — you can read exactly what happened):
  1. --build   (default on)  rebuild the soak binaries from your CURRENT tree so
                             the run tests HEAD, not a stale target/debug. Skip
                             with --no-build if you just built.
  2. --restart (default on)  restart the daemon so it loads the fresh binary AND
                             the [models] slots in ~/.sovereign/config.toml, then
                             GATE the run on /healthz + a live /v1/chat/completions
                             brain probe. A soak never starts against a dead brain
                             or a model that failed to load.
  3. run the phase(s) --attach (your resident daemon + corpora as both SUT and
                             brain) --spawn (each phase gets its own scratch
                             desktop bridge on :9745). Read-only against corpora.
  4. render the scorecard(s) and drop a .DONE sentinel carrying the return codes.

Detached by default (double-fork + setsid, PPID 1, reaper-immune) so a long run
survives your shell — or Claude Code's session — closing. The parent prints the
console-log path and a tail command, then exits. Pass --foreground to run inline.

Artifacts (all under sovereign/crates/sovereign-desktop/test-artifacts/):
    qa-iterations/<stamp>.console.log     full run log (build, restart, phases)
    qa-iterations/<stamp>-chaos.jsonl     chaos field journal (dual/chaos)
    qa-iterations/<stamp>-personas.jsonl  persona field journal (dual/persona)
    qa-iterations/<stamp>.DONE            sentinel: chaos_rc / personas_rc / epoch
"""
import argparse
import os
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
import urllib.error

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
CRATE = os.path.join(REPO, "sovereign/crates/sovereign-desktop")
E2E = os.path.join(CRATE, "tests/e2e/scripts")
CHAOS = os.path.join(E2E, "chaos.mjs")
PERSONAS = os.path.join(E2E, "personas.mjs")
CHAOS_SCORE = os.path.join(E2E, "chaos-scorecard.mjs")
PERSONA_SCORE = os.path.join(E2E, "persona-scoreboard.mjs")
HONEST_SCORE = os.path.join(E2E, "honest-scorecard.mjs")
OUTDIR = os.path.join(CRATE, "test-artifacts/qa-iterations")
CHAOS_JOURNAL = os.path.join(CRATE, "test-artifacts/chaos-journal.jsonl")
PERSONA_JOURNAL = os.path.join(CRATE, "test-artifacts/persona-journal.jsonl")
SCORE_CLI = os.path.join(REPO, "target/debug/sovereign-cli-llm")

DAEMON = "http://127.0.0.1:9741"
BRIDGE_PORT = 9745
# The binaries the soak actually launches / shells out to. Rebuilt by --build.
SOAK_BINS = ["sovereign-cli-daemon", "sovereign-desktop", "sovereign-cli", "sovereign-cli-llm"]


def log(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def sovereign_bin():
    """Resolve the `sovereign` CLI: PATH symlink first, then target/debug."""
    for cand in (shutil.which("sovereign"), os.path.join(REPO, "target/debug/sovereign-cli")):
        if cand and os.path.exists(cand):
            return cand
    return "sovereign"  # last resort; will error loudly if truly missing


def http_get(path, timeout=5):
    try:
        with urllib.request.urlopen(DAEMON + path, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except Exception as e:  # noqa: BLE001
        return None, str(e)


def daemon_healthy():
    # NB: the daemon does NOT serve /healthz or /health (those 404). /v1/models
    # is the real liveness route — 200 once the HTTP server is up — and it's the
    # same endpoint the runner's own harness (discoverBrainModel) probes.
    status, _ = http_get("/v1/models", timeout=5)
    return status == 200


def port_free(port):
    """True iff nothing is listening on 127.0.0.1:<port>.

    A loopback connect, NOT `lsof` — the soak's only supported runtime is the
    `sovereign-vulkan` toolbox (the desktop app needs libayatana-appindicator3,
    which is present there and absent on the Fedora host), and that container
    ships no lsof. Shelling out raised FileNotFoundError inside the one
    environment where the run can actually work. This mirrors harness.mjs's
    `portInUse` so both halves of the harness answer "is this port taken?"
    the same way, with no external binary.
    """
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(1.5)
        return s.connect_ex(("127.0.0.1", port)) != 0


def resident_models():
    import json
    status, body = http_get("/v1/models", timeout=10)
    if status != 200:
        return []
    try:
        return [m["id"] for m in json.loads(body).get("data", [])]
    except Exception:  # noqa: BLE001
        return []


def brain_probe(timeout=8):
    """Return True iff the primary slot answers a trivial completion — proves the
    model actually LOADED, not just that /healthz is up."""
    import json
    body = json.dumps({
        "model": "primary",
        "messages": [{"role": "user", "content": "Say OK"}],
        "max_tokens": 8,
    }).encode()
    req = urllib.request.Request(DAEMON + "/v1/chat/completions", data=body,
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            d = json.loads(r.read())
            return bool(d.get("choices", [{}])[0].get("message"))
    except Exception:  # noqa: BLE001
        return False


def do_build():
    # `--features sovereign-cli/dev-tools` is NOT optional, even though nothing
    # the soak runs needs it. Without the flag cargo resolves sovereign-cli with
    # default features and overwrites target/debug/sovereign-cli — which is what
    # ~/.local/bin/sovereign symlinks — with an end-user binary carrying no
    # `notes`, `code`, `project`, `atos` or `tools` verbs. The soak would still
    # run; the operator's code intelligence would break, minutes later, on an
    # unrelated command, reading as a missing feature rather than "the soak
    # downgraded your install." Building a superset here costs nothing and the
    # blast radius of omitting it is off-screen (.claude/CLAUDE.md states this
    # as a standing hazard).
    log(f"BUILD: cargo build -p {' -p '.join(SOAK_BINS)} "
        f"--features sovereign-cli/dev-tools (debug)")
    r = subprocess.run(["cargo", "build"] + sum([["-p", b] for b in SOAK_BINS], [])
                       + ["--features", "sovereign-cli/dev-tools"],
                       cwd=REPO)
    if r.returncode != 0:
        log(f"BUILD FAILED rc={r.returncode} — aborting soak")
        sys.exit(2)
    log("BUILD ok")


def do_restart(load_timeout):
    sov = sovereign_bin()
    log(f"RESTART: {sov} daemon stop")
    subprocess.run([sov, "daemon", "stop"], cwd=REPO)
    for _ in range(30):
        if port_free(9741):
            break
        time.sleep(1)
    log(f"RESTART: {sov} daemon start")
    subprocess.run([sov, "daemon", "start"], cwd=REPO)
    # Health gate — /healthz first, then the load-proving brain probe.
    deadline = time.time() + load_timeout
    healthy = False
    while time.time() < deadline:
        if daemon_healthy():
            healthy = True
            break
        time.sleep(2)
    if not healthy:
        log(f"RESTART FAILED: no /healthz within {load_timeout}s — aborting")
        sys.exit(3)
    log(f"health up; resident models: {', '.join(resident_models()) or '(none)'}")
    log("RESTART: probing primary (proves the model loaded, not just health)…")
    while time.time() < deadline:
        if brain_probe(timeout=15):
            log("brain probe OK — primary is live")
            return
        time.sleep(5)
    log(f"RESTART FAILED: primary never answered within {load_timeout}s — aborting")
    sys.exit(4)


def preflight():
    # Reachability — retry briefly; a single-shot check turns any transient
    # hiccup into a 150-min-run abort.
    deadline = time.time() + 30
    while time.time() < deadline and not daemon_healthy():
        time.sleep(2)
    if not daemon_healthy():
        log("PREFLIGHT FAILED: daemon not reachable on :9741 (/v1/models) — start it or use --restart")
        sys.exit(5)
    if not port_free(BRIDGE_PORT):
        log(f"PREFLIGHT FAILED: bridge :{BRIDGE_PORT} in use — a stray desktop is running")
        sys.exit(6)
    # Brain probe — the primary may be UNLOADED (primary_idle_secs), so the first
    # completion pays a cold model load (17G for a 35B). Give it a generous
    # timeout and a retry window rather than a tight single shot.
    log("preflight: probing primary (may trigger a cold model reload)…")
    deadline = time.time() + 180
    ok = False
    while time.time() < deadline:
        if brain_probe(timeout=90):
            ok = True
            break
        time.sleep(3)
    if not ok:
        log("PREFLIGHT FAILED: primary slot not answering within 180s — model not loading")
        sys.exit(7)
    log(f"preflight ok — models: {', '.join(resident_models())}")


def run_phase(name, cmd, journal, dest, minutes):
    # Truncate the source journal so the stamped copy is PURE this phase.
    try:
        open(journal, "w").close()
    except Exception as e:  # noqa: BLE001
        log(f"{name}: could not truncate {journal}: {e}")
    log(f"{name} START minutes={minutes} epoch={int(time.time())}")
    r = subprocess.run(cmd, cwd=CRATE)
    log(f"{name} EXIT rc={r.returncode} epoch={int(time.time())}")
    try:
        shutil.copyfile(journal, dest)
        log(f"{name} journal -> {dest}")
    except Exception as e:  # noqa: BLE001
        log(f"{name}: journal copy failed: {e}")
    return r.returncode


def render_scorecards(mode, chaos_dest, persona_dest):
    log("── scorecards ──")
    if mode in ("dual", "chaos") and os.path.exists(chaos_dest):
        subprocess.run(["node", CHAOS_SCORE, chaos_dest, "--label", os.path.basename(chaos_dest)],
                       cwd=CRATE)
    if mode in ("dual", "persona") and os.path.exists(persona_dest):
        subprocess.run(["node", PERSONA_SCORE, persona_dest], cwd=CRATE)
        if os.path.exists(HONEST_SCORE):
            subprocess.run(["node", HONEST_SCORE, persona_dest], cwd=CRATE)


def child_main(a, stamp, console):
    logfd = os.open(console, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    os.dup2(logfd, 1)
    os.dup2(logfd, 2)
    os.dup2(os.open(os.devnull, os.O_RDONLY), 0)
    os.chdir(CRATE)

    env = dict(os.environ)
    env["SOVEREIGN_SCORE_CLI"] = SCORE_CLI
    os.environ["SOVEREIGN_SCORE_CLI"] = SCORE_CLI

    log(f"=== desktop-soak stamp={stamp} mode={a.mode} minutes={a.minutes} "
        f"split={a.split} build={a.build} restart={a.restart} ===")

    if a.build:
        do_build()
    if a.restart:
        do_restart(a.load_timeout)
    preflight()

    chaos_dest = os.path.join(OUTDIR, f"{stamp}-chaos.jsonl")
    persona_dest = os.path.join(OUTDIR, f"{stamp}-personas.jsonl")
    done = os.path.join(OUTDIR, f"{stamp}.DONE")

    if a.mode == "dual":
        chaos_min = max(1, round(a.minutes * a.split))
        persona_min = max(1, a.minutes - chaos_min)
    elif a.mode == "chaos":
        chaos_min, persona_min = a.minutes, 0
    else:
        chaos_min, persona_min = 0, a.minutes

    rc1 = rc2 = 0
    if chaos_min:
        rc1 = run_phase("PHASE-chaos-brain",
                        ["node", CHAOS, "--attach", "--spawn", "--minutes", str(chaos_min)],
                        CHAOS_JOURNAL, chaos_dest, chaos_min)
    if persona_min:
        rc2 = run_phase("PHASE-persona-coach",
                        ["node", PERSONAS, "--attach", "--spawn", "--sessions", "0",
                         "--minutes", str(persona_min)],
                        PERSONA_JOURNAL, persona_dest, persona_min)

    render_scorecards(a.mode, chaos_dest, persona_dest)

    with open(done, "w") as f:
        f.write(f"done mode={a.mode} chaos_rc={rc1} personas_rc={rc2} epoch={int(time.time())}\n")
    log(f"=== SOAK COMPLETE mode={a.mode} chaos_rc={rc1} personas_rc={rc2} ===")


def main():
    p = argparse.ArgumentParser(
        description="Parameterized desktop chaos + persona soak.",
        formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    p.add_argument("minutes", type=int, help="total wall-clock minutes for the soak")
    p.add_argument("--mode", choices=["dual", "chaos", "persona"], default="dual")
    p.add_argument("--split", type=float, default=0.5,
                   help="dual mode: fraction of time given to chaos (default 0.5)")
    p.add_argument("--build", dest="build", action="store_true", default=True,
                   help="rebuild soak binaries from HEAD first (default)")
    p.add_argument("--no-build", dest="build", action="store_false")
    p.add_argument("--restart", dest="restart", action="store_true", default=True,
                   help="restart daemon to load fresh binary + model slots (default)")
    p.add_argument("--no-restart", dest="restart", action="store_false")
    p.add_argument("--load-timeout", type=int, default=420,
                   help="seconds to wait for the daemon + primary model to come live (default 420)")
    p.add_argument("--foreground", action="store_true",
                   help="run attached instead of detaching (double-fork+setsid) into the background")
    p.add_argument("--stamp", default=None, help="artifact name prefix (default soak-<epoch>)")
    a = p.parse_args()

    os.makedirs(OUTDIR, exist_ok=True)
    stamp = a.stamp or f"soak-{int(time.time())}"
    console = os.path.join(OUTDIR, f"{stamp}.console.log")
    done = os.path.join(OUTDIR, f"{stamp}.DONE")
    if os.path.exists(done):
        os.remove(done)

    if a.foreground:
        child_main(a, stamp, "/dev/stdout")
        return

    # Detach: double-fork + setsid so no reaper (harness or shell) can SIGKILL us.
    print(f"desktop-soak: {a.mode} soak for {a.minutes} min — detaching (reaper-immune)")
    print(f"  stamp   : {stamp}")
    print(f"  console : {console}")
    print(f"  done    : {done}")
    print(f"  tail -f {console}")
    sys.stdout.flush()
    if os.fork() > 0:
        os._exit(0)
    os.setsid()
    if os.fork() > 0:
        os._exit(0)
    child_main(a, stamp, console)


if __name__ == "__main__":
    main()
