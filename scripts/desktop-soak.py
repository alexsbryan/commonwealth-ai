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
                             the [models] slots in ~/.svrnmesh/config.toml, then
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
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import threading
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
    """Resolve the `sovereign` CLI — THIS REPO'S build first, PATH second.

    THE ORDER OF THESE TWO CANDIDATES IS LOAD-BEARING, and it used to be
    the other way round. `~/.local/bin/sovereign` is a symlink into
    whichever checkout was installed — normally the operator's. When the
    soak runs from that same checkout the two candidates are the same
    file and the preference is invisible. When it runs from a SECOND
    worktree (a feature branch under test, which is the whole point of
    isolating one) they are different files, and PATH-first meant:

        step 1  --build compiles the binaries FROM THE TREE UNDER TEST
        step 2  --restart starts the daemon FROM THE OTHER CHECKOUT

    i.e. the soak measured a daemon that did not contain the change it
    was convened to measure. Observed 2026-08-11 on the native-grounding
    flip: stage 1 built sovereign-cli-daemon at 14:45:32 in the flip
    worktree, and the restart 45s later brought up the operator's
    checkout binary dated 2026-08-10 18:50 — a build predating the flip.
    The failure is silent: the daemon is healthy, models load, the brain
    probe passes, turns answer. Only the feature is missing.

    REPO-first is correct because --build's contract is "test HEAD of
    this tree". `assert_daemon_is_ours()` then proves the daemon that
    actually came up is the one we built, because a resolution
    preference is a hope and a post-condition is a check (ARCH §18.4).
    """
    local = os.path.join(REPO, "target/debug/sovereign-cli")
    if os.path.exists(local):
        return local
    found = shutil.which("sovereign")
    if found:
        log(f"WARNING: no {local}; falling back to PATH {found}. If that is a "
            f"DIFFERENT checkout, this run will not test this tree.")
        return found
    return "sovereign"  # last resort; will error loudly if truly missing


def daemon_exe_path():
    """Filesystem path of the running daemon's executable, or None."""
    try:
        pid = subprocess.run(
            ["pgrep", "-f", "sovereign-cli-daemon daemon run"],
            capture_output=True, text=True, timeout=10).stdout.split()
        if not pid:
            return None
        out = subprocess.run(["ps", "-o", "comm=", "-p", pid[0]],
                             capture_output=True, text=True, timeout=10)
        p = out.stdout.strip()
        return p or None
    except Exception:  # noqa: BLE001
        return None


def assert_daemon_is_ours():
    """Refuse to soak a daemon built from a different tree.

    The post-condition for the resolution fix above. Without it, the only
    symptom of testing the wrong binary is "the feature did not show up",
    which reads as a product finding rather than a harness fault — the
    single most expensive misreading a soak report can contain.
    """
    exe = daemon_exe_path()
    if exe is None:
        log("DAEMON PROVENANCE: could not determine the running daemon's "
            "executable — NOT MEASURED, continuing (the health gate already "
            "proved it answers).")
        return
    if os.path.realpath(exe).startswith(os.path.realpath(REPO) + os.sep):
        log(f"DAEMON PROVENANCE ok: {exe} is inside this tree")
        return
    log("=" * 72)
    log("REFUSING TO SOAK: the running daemon is NOT built from this tree.")
    log(f"  running : {exe}")
    log(f"  this repo: {REPO}")
    log("  A soak measures the binary that serves the turns. This one would")
    log("  report the change under test as absent, which reads as a product")
    log("  failure rather than the harness fault it is.")
    log("  Fix: point ~/.local/bin/sovereign at this tree, or stop the other")
    log("  daemon and re-run so --restart starts this tree's build.")
    log("=" * 72)
    sys.exit(9)


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
    # Provenance LAST, after the daemon is proven live. Deliberately here
    # and not inside do_restart: stage 2 runs --no-restart and must be
    # held to the same standard, and preflight is the one path both take
    # (ARCH §10.6 — one check, one place).
    assert_daemon_is_ours()


def swap_free_gb():
    """Free swap in GB, or None. macOS only; None elsewhere (not measured).

    Load-bearing for the abort rule, because "reclaimable" memory is only
    reclaimable if there is somewhere to reclaim TO. Dirty anonymous
    pages must be written to swap before their frames can be handed out;
    if swap is full they cannot be evicted at all. A box showing 6GB
    "reclaimable" and 0.9GB free swap does not have 6GB of headroom.
    """
    if sys.platform != "darwin":
        return None
    try:
        out = subprocess.run(["sysctl", "-n", "vm.swapusage"],
                             capture_output=True, text=True, timeout=10)
        m = re.search(r"free\s*=\s*([\d.]+)M", out.stdout)
        return (float(m.group(1)) / 1024) if m else None
    except Exception:  # noqa: BLE001
        return None


def free_ram_gb():
    """Return (free_gb, avail_gb, why) — STRICT free first, reclaimable second.

    PLATFORM-EXPLICIT on purpose. The KV ceiling is a live constraint —
    this box runs 3.5-5GB free of 64 under bench load — so a long soak
    needs a free-RAM series, and the 2026-08-11 flip soak carries a hard
    "sustained < 2GB is abort-and-report" rule. A sampler that silently
    returned nothing on an unrecognised platform would turn that rule
    into decoration: no samples and healthy samples would look identical
    ("never tripped"). Every branch names its own instrument, and an
    unsupported platform reports NOT-MEASURED rather than None-as-fine
    (ARCH §18.3).

    WHY TWO NUMBERS, AND WHY THE ABORT GATES ON THE STRICT ONE. The first
    draft of this function returned only the reclaimable-inclusive
    figure, and measuring it before trusting it (ARCH §18.4) is what
    caught the problem. On this host, the same instant:

        free + speculative                  =  4.97 GB   <- strict
        free + speculative + inactive + purgeable = 26.08 GB   <- reclaimable
        `top` PhysMem "unused"              ~  5-6 GB

    Strict agrees with `top` and with the documented 3.5-5GB operating
    band; the reclaimable figure is 21GB of "inactive" on top of it. The
    2GB threshold was written against the operating band, so gating on
    the reclaimable number would mean the abort could essentially never
    fire — a threshold that cannot trip is not a safeguard. Both are
    journalled, because a low strict reading WITH plenty reclaimable is a
    different situation from both being low, and the reader needs to tell
    them apart.

    Linux note: MemFree is the strict analogue, but it reads low on a
    healthy box because of page cache, so the 2GB band may need
    re-calibrating if this ever gates a Linux run. MemAvailable is
    carried as the reclaimable figure.
    """
    if sys.platform == "darwin":
        try:
            out = subprocess.run(["vm_stat"], capture_output=True, text=True, timeout=10)
            if out.returncode != 0:
                return None, None, f"vm_stat rc={out.returncode}"
            m = re.search(r"page size of (\d+) bytes", out.stdout)
            page = int(m.group(1)) if m else 4096

            def pages(label):
                mm = re.search(rf"{label}:\s+(\d+)\.", out.stdout)
                return int(mm.group(1)) if mm else 0

            free = pages("Pages free") + pages("Pages speculative")
            avail = free + pages("Pages inactive") + pages("Pages purgeable")
            return (free * page) / (1024 ** 3), (avail * page) / (1024 ** 3), None
        except Exception as e:  # noqa: BLE001
            return None, None, f"vm_stat failed: {e}"
    if sys.platform.startswith("linux"):
        try:
            with open("/proc/meminfo") as f:
                txt = f.read()
            mf = re.search(r"MemFree:\s+(\d+) kB", txt)
            ma = re.search(r"MemAvailable:\s+(\d+) kB", txt)
            if not mf:
                return None, None, "no MemFree in /proc/meminfo"
            return (int(mf.group(1)) / (1024 ** 2),
                    int(ma.group(1)) / (1024 ** 2) if ma else None,
                    None)
        except Exception as e:  # noqa: BLE001
            return None, None, f"meminfo read failed: {e}"
    return None, None, f"NOT-MEASURED: unsupported platform {sys.platform}"


# ── the memory gate: TWO TIERS, and why it is not one ───────────────
#
# The order's rule is "sustained free RAM < 2GB is abort-and-report".
# Implementing that literally on macOS turned out to be wrong, and the
# evidence that corrected it is worth keeping next to the constants.
#
# The 2GB line was calibrated against a documented "3.5-5GB free of 64
# under bench load" band. macOS strict free measured 4.97GB at one
# moment, which LOOKED like a match — but that band comes from the Linux
# bench host, and MemFree and vm_stat's free pages are not commensurable
# quantities. The match was a coincidence, and a threshold resting on a
# coincidence is not calibrated.
#
# Measured on this host 2026-08-11 with the model zoo resident, three
# reads 5s apart, i.e. sustained and not a transient:
#
#     strict free      0.48 - 3.8 GB      (swings with compressor activity)
#     reclaimable      ~6.0 GB
#     swap             30798M used of 31744M  ->  ~0.95GB FREE
#     wired            45 GB               (UNRECLAIMABLE - the resident models)
#     daemon RSS       31.4 GB
#
# Three conclusions. (1) Gating on strict free alone would abort this run
# almost immediately, on a metric macOS deliberately keeps near zero.
# (2) Gating on free+reclaimable is WORSE, not better: it would treat
# ~6GB as headroom while swap — the only place those dirty pages can go —
# has under 1GB left, so most of that "reclaimable" cannot actually be
# realised. (3) The binding constraint is neither number alone but "can
# the system find a page to hand out", which needs free frames OR swap to
# evict into.
#
# So: ABORT only when BOTH are gone (true OOM imminence, nowhere left to
# go), and WARN-journal at the order's original line so the condition is
# recorded on every sample without killing a 2h run. A warn is data; an
# abort is a verdict, and they should not share a threshold.
MEM_ABORT_GB = float(os.environ.get("SOVEREIGN_SOAK_MEM_ABORT_GB", "0.5"))
MEM_ABORT_SWAP_GB = float(os.environ.get("SOVEREIGN_SOAK_MEM_ABORT_SWAP_GB", "1.5"))
MEM_WARN_GB = float(os.environ.get("SOVEREIGN_SOAK_MEM_WARN_GB", "2.0"))
MEM_SAMPLE_SECS = int(os.environ.get("SOVEREIGN_SOAK_MEM_SAMPLE_SECS", "15"))
MEM_ABORT_CONSECUTIVE = int(os.environ.get("SOVEREIGN_SOAK_MEM_ABORT_SAMPLES", "8"))


def start_mem_sampler(mem_path, state):
    """Sample free RAM into a JSONL until state['stop'] is set.

    Writes EVERY sample, including unmeasurable ones with their reason,
    so the series can never be mistaken for "measured and fine."
    """
    def loop():
        low_streak = 0
        while not state.get("stop"):
            gb, avail, why = free_ram_gb()
            swap = swap_free_gb()
            rec = {"ts": int(time.time()),
                   "free_gb": None if gb is None else round(gb, 2),
                   "avail_gb": None if avail is None else round(avail, 2),
                   "swap_free_gb": None if swap is None else round(swap, 2),
                   "warn": (gb is not None and gb < MEM_WARN_GB),
                   "why": why, "phase": state.get("phase")}
            try:
                with open(mem_path, "a") as f:
                    f.write(json.dumps(rec) + "\n")
            except Exception:  # noqa: BLE001
                pass
            if gb is None:
                # Could-not-judge. Never counts toward the abort: an
                # absent instrument is not evidence of a healthy box —
                # nor of a sick one.
                low_streak = 0
                if why and state.get("warned_why") != why:
                    state["warned_why"] = why
                    log(f"MEM: NOT MEASURED — {why}")
            else:
                # ABORT tier: free frames gone AND nowhere to evict to.
                # Swap unknown (non-darwin) cannot satisfy the second
                # clause, so it can never abort on a platform where we
                # cannot see swap — could-not-judge is not could-abort.
                starved = gb < MEM_ABORT_GB
                no_swap = swap is not None and swap < MEM_ABORT_SWAP_GB
                swap_txt = "n/m" if swap is None else f"{swap:.2f}GB"
                if starved and no_swap:
                    low_streak += 1
                    log(f"MEM CRITICAL: free {gb:.2f}GB < {MEM_ABORT_GB}GB AND "
                        f"swap free {swap_txt} < {MEM_ABORT_SWAP_GB}GB "
                        f"({low_streak}/{MEM_ABORT_CONSECUTIVE} consecutive)")
                    if low_streak >= MEM_ABORT_CONSECUTIVE:
                        state["aborted"] = (
                            f"sustained OOM imminence for "
                            f"{low_streak * MEM_SAMPLE_SECS}s: free < "
                            f"{MEM_ABORT_GB}GB with swap free < {MEM_ABORT_SWAP_GB}GB")
                        log(f"MEM ABORT: {state['aborted']} — terminating phase")
                        proc = state.get("proc")
                        if proc is not None:
                            try:
                                proc.terminate()
                            except Exception:  # noqa: BLE001
                                pass
                        return
                else:
                    low_streak = 0
                    # WARN tier: journalled on every sample via rec["warn"];
                    # logged only on transitions so a 2h run at 1.9GB does
                    # not emit 480 identical lines.
                    if gb < MEM_WARN_GB and not state.get("warned_low"):
                        state["warned_low"] = True
                        log(f"MEM WARN: free {gb:.2f}GB < {MEM_WARN_GB}GB "
                            f"(reclaimable {avail:.2f}GB, swap free {swap_txt}) — "
                            f"journalling, NOT aborting; abort needs free < "
                            f"{MEM_ABORT_GB}GB with swap < {MEM_ABORT_SWAP_GB}GB")
                    elif gb >= MEM_WARN_GB and state.get("warned_low"):
                        state["warned_low"] = False
                        log(f"MEM recovered: free {gb:.2f}GB >= {MEM_WARN_GB}GB")
            time.sleep(MEM_SAMPLE_SECS)

    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t


def run_phase(name, cmd, journal, dest, minutes, state=None):
    # Truncate the source journal so the stamped copy is PURE this phase.
    try:
        open(journal, "w").close()
    except Exception as e:  # noqa: BLE001
        log(f"{name}: could not truncate {journal}: {e}")
    log(f"{name} START minutes={minutes} epoch={int(time.time())}")
    if state is not None:
        state["phase"] = name
    # Popen rather than subprocess.run so the memory sampler holds a
    # handle it can terminate when the abort threshold trips.
    r = subprocess.Popen(cmd, cwd=CRATE)
    if state is not None:
        state["proc"] = r
    r.wait()
    if state is not None:
        state["proc"] = None
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
    mem_dest = os.path.join(OUTDIR, f"{stamp}-mem.jsonl")
    done = os.path.join(OUTDIR, f"{stamp}.DONE")

    # Free-RAM series for the whole run, started AFTER the restart so the
    # cold model load is inside the window it is meant to observe.
    gb0, av0, why0 = free_ram_gb()
    sw0 = swap_free_gb()
    start_txt = (f"free {gb0:.2f}GB / reclaimable {av0:.2f}GB"
                 if gb0 is not None and av0 is not None
                 else (f"free {gb0:.2f}GB" if gb0 is not None
                       else f"NOT MEASURED ({why0})"))
    start_txt += f" / swap free {'n/m' if sw0 is None else f'{sw0:.2f}GB'}"
    log(f"MEM: sampler every {MEM_SAMPLE_SECS}s -> {mem_dest}")
    log(f"MEM: WARN below {MEM_WARN_GB}GB free (journalled, never aborts); "
        f"ABORT only when free < {MEM_ABORT_GB}GB AND swap free < "
        f"{MEM_ABORT_SWAP_GB}GB for {MEM_ABORT_CONSECUTIVE} consecutive "
        f"samples ({MEM_ABORT_CONSECUTIVE * MEM_SAMPLE_SECS}s)")
    log(f"MEM: start={start_txt}")
    mem_state = {"stop": False, "proc": None, "phase": "startup", "aborted": None}
    start_mem_sampler(mem_dest, mem_state)

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
                        CHAOS_JOURNAL, chaos_dest, chaos_min, mem_state)
    # A memory abort skips the remaining phase: pushing a second phase
    # onto a box that just ran out of RAM measures the box, not the code.
    if persona_min and not mem_state.get("aborted"):
        rc2 = run_phase("PHASE-persona-coach",
                        ["node", PERSONAS, "--attach", "--spawn", "--sessions", "0",
                         "--minutes", str(persona_min)],
                        PERSONA_JOURNAL, persona_dest, persona_min, mem_state)
    elif persona_min:
        log("PHASE-persona-coach SKIPPED — memory abort during the previous phase")

    mem_state["stop"] = True
    render_scorecards(a.mode, chaos_dest, persona_dest)

    aborted = mem_state.get("aborted")
    with open(done, "w") as f:
        f.write(f"done mode={a.mode} chaos_rc={rc1} personas_rc={rc2} "
                f"epoch={int(time.time())} mem_abort={aborted or 'none'}\n")
    log(f"=== SOAK COMPLETE mode={a.mode} chaos_rc={rc1} personas_rc={rc2} "
        f"mem_abort={aborted or 'none'} ===")
    if aborted:
        # Loud, non-zero: an aborted soak is not a completed soak, and the
        # sentinel alone is too easy to read past.
        log(f"=== SOAK ABORTED ON MEMORY: {aborted} ===")
        sys.exit(8)


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
