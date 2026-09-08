#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""daemon-concurrency-soak — does the daemon survive two users at once?

THE ONE OPS LENS THIS REPO TRUSTS. Eight soak-family scripts existed before
this and every one ran `by-hand`, `cost_secs = "unmeasured"`, most with
`negative_control = "none"` — an intention with a shebang, not an instrument
(ARCH §18.1, §18.4). None of the eight drove two concurrent chat turns at a
real daemon, which is the smallest multi-user shape there is and the shape
that actually kills it here.

WHAT IT ASSERTS. The daemon that was up when the run started is the same
process that is up when it ends. A run where the daemon exited FAILS, and the
failure names the daemon's own death class — never a bare transport error.
`product-defects-batch` entry 3 is the reproduction it was watched red on:
under two in-flight turns the resident daemon reached ~40 GB RSS on a 64 GB
box and macOS jetsam SIGTERM'd it; sixteen consecutive client turns then died
in ~40 ms on a closed socket, which from the client side is indistinguishable
from a refusal.

WHAT IT REUSES (ARCH §19 — the inventory outranks the plan).
`scripts/admission_shed_probe.py` already drives exactly this shape and
already classifies a turn's death from the CLIENT side. Its driver (`ask`),
its question bank, its concurrent-load question, its load covariate and its
log-file accessor are IMPORTED here, not copied. What this file adds is the
half that script does not have and should not grow: the DAEMON-side death
class, an RSS trajectory, and an exit code.

THE TWO SIDES ARE DIFFERENT QUESTIONS and are kept apart on purpose. The
client side answers "what did the user get" (`death_kind`, sniffed out of an
error string — the §2.4 shape this repo keeps deleting, kept only because the
daemon gives a client no better signal today). The daemon side answers "what
happened to the process", out of the daemon's OWN stable log vocabulary plus
the pidfile, and it is the side the verdict is taken from.

Usage:
  scripts/daemon-concurrency-soak.py --self-test          # offline, no daemon
  scripts/daemon-concurrency-soak.py --minutes 30
  scripts/daemon-concurrency-soak.py --minutes 2 --inject-death sigkill \\
      --inject-at 20 --expect-death crash          # the negative control

Exit — FOUR verdicts, not two (ARCH §18.2):
  0  PASSED — the daemon survived the window (or, with --expect-death, the
     injected death was detected as the class named)
  1  FAILED — the daemon died of something this run can pin on the load:
     jetsam (corroborated by the kernel), rss_hard_limit, host_headroom,
     listener_lost, crash. The verdict names which
  2  COULD-NOT-JUDGE — no daemon at the start, or the daemon died of
     something this run CANNOT attribute: a bare SIGTERM with no kernel
     memory-kill behind it is an operator, a peer session or a supervisor,
     and scoring one as a caught defect would make this instrument worse
     than none. Never a pass; re-run
  3  the negative control was not caught — something broke the daemon on
     purpose and this instrument did not notice, or named the wrong class

Contention is the condition, not a precondition. There is no host-quiet gate:
the 1-minute load is recorded per turn and reported as a covariate, because a
number minted on an artificially quiet machine measures a state that never
occurs in use.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import pathlib
import signal
import statistics
import subprocess
import sys
import threading
import time
from collections import Counter

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

# The driver, the bank, the contention question, the load covariate and the
# log-file set — one implementation each, owned by the probe (ARCH §10.6).
from admission_shed_probe import (  # noqa: E402
    BANK,
    CLI,
    DAEMON_ERR,
    DEATH_DAEMON_GONE,
    LOAD_QUESTION,
    TS_RE,
    ask,
    load1,
    log_files,
)

import re  # noqa: E402

# The daemon writes tracing fields wrapped in colour escapes, so `pid=13844`
# is NOT literal in the bytes — it is `pid<ESC>[0m<ESC>[2m=<ESC>[0m13844`. A
# naive `pid=(\d+)` matches nothing and the classifier silently loses every
# jetsam corroboration. Strip first, then match.
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
# `(?<!p)` so `ppid=1` cannot answer for `pid=`.
PID_FIELD_RE = re.compile(r"(?<!p)pid=(\d+)")


def receipt_pid(line: str) -> int | None:
    m = PID_FIELD_RE.search(ANSI_RE.sub("", line))
    return int(m.group(1)) if m else None

SVRN_ROOT = DAEMON_ERR.parent.parent
PIDFILE = SVRN_ROOT / "daemon.pid"

# ── The death classes ────────────────────────────────────────────────
#
# A CLOSED set (ARCH §2), and every member is matched on a string the daemon
# itself emits at a site that documents the spelling as stable — not on a
# guess about what the failure looked like. The pattern, the emitting site and
# what the class means to an operator, in one table so a reader can check any
# row against the source:
#
#   jetsam          daemon_cmd/lifecycle.rs::log_shutdown_context — SIGTERM
#                   arriving with peak RSS >= 24 GiB. The OS killed it. The
#                   daemon has no ceiling of its own by default
#                   (SOVEREIGN_RSS_HARD_LIMIT_MB is off), so this is the
#                   class entry 3 is about.
#   rss_hard_limit  memory_watch.rs — the daemon's OWN ceiling fired and it
#                   asked for a relaunch (exit 102). Opt-in; not the default.
#   host_headroom   memory_watch.rs — the BOX ran out, daemon stood down first.
#   listener_lost   listener_watch.rs — the process lived but stopped
#                   accepting on the client port (exit 104, phantom-Running).
#   signal          lifecycle.rs, the non-jetsam branch — an ordinary SIGTERM
#                   or SIGINT. An operator `daemon stop`, or a supervisor.
#   crash           the pid changed and NO shutdown receipt was written. A
#                   SIGKILL, a panic, or a kernel OOM that left no trail.
#
# `crash` is the residual and it is a real verdict, not a bucket to hide in:
# it is the only class derived from the pidfile alone, and the negative
# control below exists to prove it can be reached.
DEATH_JETSAM = "jetsam"
# A SIGTERM the instrument CANNOT ATTRIBUTE. Not a bucket and not a synonym
# for jetsam: it is could-not-judge on the class, and the run that produced
# it is could-not-judge overall. Watched necessary on 2026-09-08 — a peer
# session ran `daemon stop` at 14:25:55Z and the daemon logged it as
# "peak RSS suggests possible jetsam/OOM trigger", because its own test is
# `SIGTERM && peak_rss >= 24 GiB` and this daemon idles at 22-45 GB on a
# 64 GB box. Every operator stop here wears the jetsam costume. Scoring one
# as a caught defect would make this instrument worse than no instrument.
DEATH_SIGTERM_UNATTRIBUTED = "sigterm_unattributed"
# This run pulled the trigger. Kept apart from every other class so the
# negative control cannot be mistaken for a finding.
DEATH_SELF_STOP = "self_stop"
DEATH_RSS_HARD = "rss_hard_limit"
DEATH_HOST_HEADROOM = "host_headroom"
DEATH_LISTENER_LOST = "listener_lost"
DEATH_SIGNAL = "signal"
DEATH_CRASH = "crash"
ALIVE = "clean"

# Ordered most specific first: the jetsam line CONTAINS the plain shutdown
# receipt's text, so a set would resolve it by dict order and that is not a
# thing to leave to chance.
DEATH_PATTERNS = [
    (DEATH_JETSAM, "peak RSS suggests possible jetsam/OOM trigger"),
    (DEATH_RSS_HARD, "memory-watch: HARD limit breached"),
    (DEATH_HOST_HEADROOM, "memory-watch: HOST HEADROOM below floor"),
    (DEATH_LISTENER_LOST, "listener-watch: client listener LOST"),
    (DEATH_SIGNAL, "daemon: shutdown signal received"),
]

DEATH_CLASSES = [p[0] for p in DEATH_PATTERNS] + [
    DEATH_CRASH,
    DEATH_SIGTERM_UNATTRIBUTED,
    DEATH_SELF_STOP,
]

# Classes this soak may report as a CAUGHT DEFECT (exit 1). Everything else
# that is not `clean` is could-not-judge (exit 2): the daemon died, and this
# run cannot show the load did it.
DEATH_ATTRIBUTABLE = {
    DEATH_JETSAM,
    DEATH_RSS_HARD,
    DEATH_HOST_HEADROOM,
    DEATH_LISTENER_LOST,
    DEATH_CRASH,
}


def read_pid() -> int | None:
    try:
        return int(PIDFILE.read_text().strip())
    except (OSError, ValueError):
        return None


def alive(pid: int | None) -> bool:
    if pid is None:
        return False
    try:
        os.kill(pid, 0)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def rss_kb(pid: int | None) -> int | None:
    """Resident set of the daemon, in KiB, right now.

    Recorded because the daemon's own jetsam WARN reports a suspicion and not
    a trajectory: it prints the peak at the moment it dies, so nobody can say
    whether the process climbed to it or arrived there at boot. `ps` is used
    rather than a library so the reading needs nothing installed.
    """
    if pid is None:
        return None
    try:
        out = subprocess.run(
            ["ps", "-o", "rss=", "-p", str(pid)],
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout.strip()
        return int(out) if out else None
    except (subprocess.SubprocessError, ValueError):
        return None


def node_name() -> str:
    """This box's MESH name — the one peers coordinate on.

    `hostname -s` is a SECOND name for the same machine that no peer would
    ever match (`BeefyMac` vs `Alexs-MacBook-Pro-2`), so a scope keyed on it
    collides with nobody, which is the exact failure the check exists to
    prevent. `scripts/run-if-stale.sh::node_name` resolves it the same way and
    the two MUST agree; if you change one, change the other.
    """
    try:
        out = subprocess.run(
            [str(CLI), "mesh", "status"],
            capture_output=True,
            text=True,
            timeout=30,
            env=os.environ | {"SOVEREIGN_NO_STALE_WARN": "1"},
        ).stdout
        for line in out.splitlines():
            if line.rstrip().endswith(" *"):
                parts = line.split()
                if len(parts) >= 2:
                    return parts[1]
    except (subprocess.SubprocessError, OSError):
        pass
    import socket

    print(
        "daemon-concurrency-soak: mesh node name unavailable — the peer-claim "
        "reading falls back to the host name and will match no peer's scope",
        file=sys.stderr,
    )
    return socket.gethostname().split(".")[0]


def daemon_claim_verdict(scope: str) -> str:
    """held | expired | free | unknown — is a PEER about to churn this daemon?

    Read-only (`claim may-i`, never `take`): a soak that took the claim and
    was then SIGKILLed by the very defect it hunts would leave a claim nobody
    releases, and a commons full of ghosts is worse than one nobody consults.
    Recorded in the summary so an unattributed death can be read against
    whether anyone had DECLARED they would touch the daemon.
    """
    try:
        r = subprocess.run(
            [str(CLI), "claim", "may-i", scope, "--format", "json"],
            capture_output=True,
            text=True,
            timeout=30,
            env=os.environ | {"SOVEREIGN_NO_STALE_WARN": "1"},
        )
        return (json.loads(r.stdout) or {}).get("verdict", "unknown")
    except (subprocess.SubprocessError, OSError, json.JSONDecodeError, TypeError):
        return "unknown"


def port_serving() -> bool | None:
    """Is the client port actually SERVING, not merely bound?

    `/v1/models` with an EXPLICIT status check. There is no `/healthz` on
    :9741 — it 404s — and a `curl … && …` liveness probe written the obvious
    way reads that 404 as healthy
    (`invariant_daemon_health_endpoint_not_healthz`, 2026-07-20). On this
    instrument that trap turns a real death into a clean run, which is the
    one failure that makes a soak worthless while looking green.

    This is also the ONLY detector that can see `listener_lost`: a
    phantom-Running daemon has a live process, a valid pidfile and a dead
    port, so process liveness alone cannot reach that class and a class no
    input can reach is the §18.1 smell.

    `None` means the probe itself could not run — reported, not defaulted.
    """
    import urllib.error
    import urllib.request

    try:
        with urllib.request.urlopen("http://127.0.0.1:9741/v1/models", timeout=10) as r:
            return r.status == 200
    except urllib.error.HTTPError as e:
        return e.code == 200
    except (urllib.error.URLError, OSError, TimeoutError):
        return False
    except Exception:  # noqa: BLE001 — a probe must never take the run down
        return None


class DaemonWatch(threading.Thread):
    """Polls the pidfile and the RSS while the turns run.

    Sampling rather than before/after: a daemon that dies and is relaunched
    inside the window has the SAME pidfile shape at both ends if it dies
    twice, and a peak that only the middle of the run saw is exactly the
    number entry 3 says nobody has.
    """

    def __init__(self, interval: float = 5.0):
        super().__init__(daemon=True)
        self.interval = interval
        self.stop_flag = threading.Event()
        self.pid_transitions: list[dict] = []
        self.rss_samples: list[int] = []
        # A THIRD detector, and the one that catches the nastiest shape: a
        # daemon SIGKILLed while its pidfile still names it. The file is
        # written by the process that died, so nothing updates it until a
        # replacement boots — a reader trusting the pidfile alone sees a
        # steady pid across a hole in service and calls the run clean.
        self.pid_not_alive_samples = 0
        # Live process, dead port. See `port_serving`.
        self.phantom_running_samples = 0
        self.first_pid = read_pid()
        self.last_pid = self.first_pid

    def run(self) -> None:
        while not self.stop_flag.is_set():
            pid = read_pid()
            if pid != self.last_pid:
                self.pid_transitions.append(
                    {
                        "at_utc": datetime.datetime.now(datetime.timezone.utc).strftime(
                            "%Y-%m-%dT%H:%M:%S"
                        ),
                        "from": self.last_pid,
                        "to": pid,
                    }
                )
                self.last_pid = pid
            proc_up = alive(pid)
            if not proc_up:
                self.pid_not_alive_samples += 1
            elif port_serving() is False:
                self.phantom_running_samples += 1
            r = rss_kb(pid)
            if r:
                self.rss_samples.append(r)
            self.stop_flag.wait(self.interval)

    def stop(self) -> None:
        self.stop_flag.set()


def os_confirms_memory_kill(pid: int, at_utc: str) -> tuple[bool | None, str]:
    """Did the OPERATING SYSTEM record killing this pid for memory?

    THE DAEMON'S OWN JETSAM VERDICT IS NOT EVIDENCE and this function exists
    because of that. `log_shutdown_context` calls a shutdown jetsam whenever
    a SIGTERM arrives with peak RSS >= 24 GiB, which on a box where the
    daemon idles above that threshold is true of every operator `daemon
    stop`. A guard that asserts on a field the subject supplies is the §18.1
    smell; this reads a third party instead.

    Returns (True | False | None, detail). `None` is "no authority to ask" —
    an unsupported platform or a query that failed — and is reported as such,
    never collapsed into False (ARCH §18.3).
    """
    if sys.platform != "darwin":
        return None, f"no OS memory-kill oracle wired for {sys.platform}"
    try:
        t = datetime.datetime.strptime(at_utc, "%Y-%m-%dT%H:%M:%S").replace(
            tzinfo=datetime.timezone.utc
        )
    except ValueError:
        return None, f"unparseable death timestamp {at_utc!r}"
    # `log show` speaks LOCAL time; the daemon logs UTC. A 90-second collar
    # each side covers clock skew between the two clocks without widening
    # far enough to catch an unrelated kill.
    lo = (t - datetime.timedelta(seconds=90)).astimezone().strftime("%Y-%m-%d %H:%M:%S")
    hi = (t + datetime.timedelta(seconds=90)).astimezone().strftime("%Y-%m-%d %H:%M:%S")
    try:
        r = subprocess.run(
            [
                "/usr/bin/log",
                "show",
                "--style",
                "compact",
                "--start",
                lo,
                "--end",
                hi,
                "--predicate",
                # `senderImagePath` pins the KERNEL as the speaker. Without
                # it this query answers itself: `log show`'s own invocation is
                # logged with its full argv, that argv contains the literal
                # "memorystatus: killing" and the pid, and the line lands
                # inside the window whenever the death was recent — i.e. in
                # every live run. The offline self-test caught it on its first
                # execution; it is the §18.1 smell (a guard matching data the
                # query itself supplies) hiding inside the fix for a different
                # instance of the same smell.
                f'senderImagePath CONTAINS "kernel" '
                f'AND eventMessage CONTAINS "memorystatus: killing" '
                f'AND eventMessage CONTAINS "{pid}"',
            ],
            capture_output=True,
            text=True,
            timeout=180,
        )
    except (subprocess.SubprocessError, OSError) as e:
        return None, f"`log show` failed: {e}"
    if r.returncode != 0:
        return None, f"`log show` exited {r.returncode}: {r.stderr.strip()[:200]}"
    for line in r.stdout.splitlines():
        # Belt and braces on the same trap: the compact format names the
        # emitting process, and only `kernel[...]` counts as the kernel
        # saying it.
        if "kernel[" in line and "memorystatus: killing" in line and str(pid) in line:
            return True, line.strip()[:400]
    # AN EMPTY ANSWER IS NOT A NEGATIVE ANSWER. macOS `memorystatus` lines are
    # debug-level; whether they survive to the persisted store is a system
    # setting and a retention window, not a constant. So before reading
    # silence as "no kill happened", check the oracle was awake at all in
    # this window — otherwise a log-retention gap quietly exonerates a real
    # jetsam (ARCH §18.3, absence is reported and never defaulted).
    awake = subprocess.run(
        [
            "/usr/bin/log", "show", "--style", "compact",
            "--start", lo, "--end", hi,
            "--predicate",
            'senderImagePath CONTAINS "kernel" AND eventMessage CONTAINS "memorystatus"',
        ],
        capture_output=True,
        text=True,
        timeout=180,
    )
    lines = [
        ln
        for ln in awake.stdout.splitlines()
        if "kernel[" in ln and "memorystatus" in ln
    ]
    if not lines:
        return None, (
            f"no memorystatus lines AT ALL in {lo}..{hi} (local) — the kernel "
            "oracle has no coverage of this window, so its silence about pid "
            f"{pid} means nothing"
        )
    return False, (
        f"kernel memorystatus was live in {lo}..{hi} (local) — {len(lines)} "
        f"line(s), none a kill of pid {pid}"
    )


def classify_daemon_death(
    lo_utc: str, hi_utc: str, self_stopped: bool
) -> tuple[str | None, str | None, dict]:
    """What happened to the daemon inside this window, and on whose word.

    Returns (class, evidence line, corroboration). Reads every rotated copy,
    because the daemon truncates its log past a size cap and a soak whose
    reading depends on the log not rotating is not an instrument (§18.4).

    A suspected jetsam is not returned as one until the kernel agrees. When
    the kernel has no record, or there is no oracle to ask, the class is
    `sigterm_unattributed` and the RUN is could-not-judge — because the
    alternative is to hand the campaign a peer's `daemon stop` dressed as the
    defect this instrument was built to catch.
    """
    hit: tuple[str, str] | None = None
    death_pid: int | None = None
    for f in log_files():
        if not f.exists():
            continue
        try:
            with f.open(encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    m = TS_RE.search(line)
                    if not m or not (lo_utc <= m.group(1) < hi_utc):
                        continue
                    for cls, pat in DEATH_PATTERNS:
                        if pat in line:
                            # Last receipt in the window wins: a run that saw
                            # two deaths is named by the one it ended on, and
                            # the count is carried separately.
                            hit = (cls, line.strip()[:400])
                            death_pid = receipt_pid(line)
                            break
        except OSError:
            continue
    if hit is None:
        return None, None, {}
    cls, line = hit
    if self_stopped:
        return DEATH_SELF_STOP, line, {"attributed_to": "this run's --inject-death"}
    if cls != DEATH_JETSAM:
        return cls, line, {"attributed_to": "the daemon's own receipt"}
    if death_pid is None:
        return (
            DEATH_SIGTERM_UNATTRIBUTED,
            line,
            {"os_confirms": None, "detail": "no pid field on the receipt"},
        )
    confirmed, detail = os_confirms_memory_kill(death_pid, TS_RE.search(line).group(1))
    if confirmed:
        return DEATH_JETSAM, line, {"os_confirms": True, "detail": detail}
    return (
        DEATH_SIGTERM_UNATTRIBUTED,
        line,
        {"os_confirms": confirmed, "detail": detail},
    )


def inject(kind: str, pid: int | None) -> dict:
    """Break the daemon on purpose. The negative control's whole body.

    A `control` instrument that only ever ran green is indistinguishable from
    one that cannot see anything (ARCH §18.1) — so this exists to make the
    soak above go red on demand, and `--expect-death` turns "went red" into
    the control's pass.
    """
    if pid is None:
        return {"kind": kind, "sent": False, "why": "no pid to signal"}
    sig = {"sigkill": signal.SIGKILL, "sigterm": signal.SIGTERM}[kind]
    try:
        os.kill(pid, sig)
        return {"kind": kind, "sent": True, "pid": pid}
    except OSError as e:
        return {"kind": kind, "sent": False, "why": str(e)}


def self_test() -> int:
    """Watch the classifier tell the classes apart, with no daemon involved.

    The `--expect-death` control needs a live daemon and three minutes; this
    needs neither, so there is no excuse for the closed set going unchecked.
    Every case is a receipt this daemon actually writes, and the cases that
    matter are the two that look identical in the log and are not: a jetsam
    the kernel confirms, and one it does not.
    """
    import tempfile

    global log_files
    real_log_files = log_files
    real_oracle = globals()["os_confirms_memory_kill"]
    fails = 0

    def case(name, line, expect, *, self_stopped=False, oracle=(False, "stub")):
        nonlocal fails
        with tempfile.TemporaryDirectory() as d:
            f = pathlib.Path(d) / "daemon.err"
            f.write_text(line + "\n" if line else "")
            globals()["log_files"] = lambda: [f]
            globals()["os_confirms_memory_kill"] = lambda *_a, **_k: oracle
            got, _ev, _corr = classify_daemon_death(
                "2026-01-01T00:00:00", "2030-01-01T00:00:00", self_stopped
            )
        ok = got == expect
        fails += 0 if ok else 1
        print(f"  {'PASS' if ok else 'FAIL'}  {name}: expected {expect}, got {got}")

    stamp = "2026-09-08T05:38:35.444899Z"
    jetsam_line = (
        f"{stamp}  WARN daemon: shutdown signal received — peak RSS suggests "
        'possible jetsam/OOM trigger signal="SIGTERM" pid=86984 rss_mb=44899'
    )
    print("── classifier self-test ────────────────────────────────────")
    # THE PAIR THIS INSTRUMENT EXISTS FOR. One byte of evidence apart in the
    # daemon's log — nothing — and opposite verdicts, because the kernel is
    # asked and the daemon is not believed.
    case("jetsam, kernel CONFIRMS", jetsam_line, DEATH_JETSAM,
         oracle=(True, "memorystatus: killing_specific_process pid 86984"))
    case("jetsam, kernel DENIES", jetsam_line, DEATH_SIGTERM_UNATTRIBUTED,
         oracle=(False, "no kill of pid 86984"))
    case("jetsam, kernel HAS NO COVERAGE", jetsam_line, DEATH_SIGTERM_UNATTRIBUTED,
         oracle=(None, "no memorystatus lines at all"))
    case("this run pulled the trigger", jetsam_line, DEATH_SELF_STOP,
         self_stopped=True)
    case("the daemon's own ceiling", f"{stamp}  WARN memory-watch: HARD limit "
         "breached — initiating graceful restart", DEATH_RSS_HARD)
    case("the box ran out", f"{stamp}  WARN memory-watch: HOST HEADROOM below "
         "floor — initiating graceful", DEATH_HOST_HEADROOM)
    case("phantom-Running", f"{stamp}  WARN listener-watch: client listener "
         "LOST (phantom-Running) — initiating", DEATH_LISTENER_LOST)
    case("an ordinary stop", f'{stamp}  INFO daemon: shutdown signal received '
         'signal="SIGTERM" pid=1 rss_mb=900', DEATH_SIGNAL)
    case("nothing happened", "", None)

    globals()["log_files"] = real_log_files
    globals()["os_confirms_memory_kill"] = real_oracle

    # And the ORACLE itself, unstubbed: a pid that cannot exist must come back
    # unconfirmed, with a detail saying whether it was even watching. An
    # oracle that answers "no" the same way whether it looked or not is not an
    # oracle (ARCH §18.3).
    confirmed, detail = os_confirms_memory_kill(
        4194303, datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
    )
    ok = confirmed is not True and ("memorystatus" in detail or "no OS" in detail)
    fails += 0 if ok else 1
    print(f"  {'PASS' if ok else 'FAIL'}  live oracle on an impossible pid: "
          f"confirmed={confirmed} — {detail[:90]}")

    print()
    print("── SELF-TEST: " + ("PASS" if not fails else f"FAIL ({fails})"))
    return 1 if fails else 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--self-test",
        action="store_true",
        help="check the death classes are told apart, offline, with no daemon",
    )
    ap.add_argument("--minutes", type=float, default=30.0)
    ap.add_argument("--corpus", default="sep")
    ap.add_argument(
        "--concurrency",
        type=int,
        default=1,
        help="background `chat ask` loops. 1 (default) = TWO turns in flight, "
        "which is entry 3's shape and the smallest multi-user one there is",
    )
    ap.add_argument("--timeout", type=int, default=600, help="per-turn client cap")
    ap.add_argument("--out", default="target/daemon-concurrency-soak")
    ap.add_argument(
        "--tail-turns",
        type=int,
        default=8,
        help="turns to keep driving AFTER the daemon is seen dead, to record "
        "what a client in flight actually gets. The window is a ceiling, not "
        "a quota: once the subject is gone there is nothing left to measure",
    )
    ap.add_argument("--inject-death", choices=["sigkill", "sigterm"])
    ap.add_argument(
        "--no-restore",
        action="store_true",
        help="leave the daemon down after an injected death. The default is to "
        "bring it back: launchd here declares KeepAlive {SuccessfulExit: false} "
        "and did NOT relaunch after an injected SIGKILL, so a control that does "
        "not restore leaves the operator's daemon dead — and a negative control "
        "nobody dares schedule is one that never runs",
    )
    ap.add_argument("--inject-at", type=float, default=20.0, help="seconds in")
    ap.add_argument(
        "--expect-death",
        choices=DEATH_CLASSES,
        help="negative-control mode: pass ONLY if a death of this class was "
        "detected. Without it, any death is a failure",
    )
    a = ap.parse_args()

    if a.self_test:
        return self_test()

    if not CLI.exists():
        print(f"could-not-judge: missing {CLI} — build it first", file=sys.stderr)
        return 2

    pid0 = read_pid()
    if not alive(pid0):
        print(
            f"could-not-judge: no daemon running (pidfile {PIDFILE} -> {pid0}). "
            "A soak with no subject verified nothing.",
            file=sys.stderr,
        )
        return 2

    out = pathlib.Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    turns_path = out / "turns.jsonl"

    watch = DaemonWatch()
    watch.start()

    loaders = []
    for _ in range(max(0, a.concurrency)):
        loaders.append(
            subprocess.Popen(
                [
                    "/bin/sh",
                    "-c",
                    f'while :; do "{CLI}" chat ask --corpus {a.corpus} '
                    f'--format json "{LOAD_QUESTION}" >/dev/null 2>&1 || true; done',
                ],
                env=os.environ | {"SOVEREIGN_NO_STALE_WARN": "1"},
                start_new_session=True,
            )
        )
    if loaders:
        time.sleep(2)

    claim_scope = f"daemon:{node_name()}:restart"
    claim_at_start = daemon_claim_verdict(claim_scope)
    lo = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
    t0 = time.monotonic()
    deadline = t0 + a.minutes * 60.0
    injected: dict | None = None
    rows: list[dict] = []
    tail_left: int | None = None
    i = 0
    try:
        with turns_path.open("w") as fh:
            while time.monotonic() < deadline:
                if (
                    a.inject_death
                    and injected is None
                    and time.monotonic() - t0 >= a.inject_at
                ):
                    injected = inject(a.inject_death, read_pid())
                    print(f"[control] injected {injected}", flush=True)
                rec = ask(BANK[i % len(BANK)], a.corpus, a.timeout)
                rec["i"] = i
                rows.append(rec)
                fh.write(json.dumps(rec) + "\n")
                fh.flush()
                print(
                    f"[soak] {i} {rec['wall_ms']}ms exit={rec['exit']} "
                    f"died={rec['died']} kind={rec.get('death_kind')} "
                    f"load={rec['load1_at_start']:.1f}",
                    flush=True,
                )
                i += 1
                # A dead daemon answers every turn in ~40 ms, so an unpaced
                # loop after a kill burns thousands of turns saying one thing.
                if rec["died"] and rec.get("death_kind") == DEATH_DAEMON_GONE:
                    if tail_left is None:
                        tail_left = a.tail_turns
                        print(
                            f"[soak] daemon gone at turn {i - 1} — recording "
                            f"{a.tail_turns} more turns, then rendering",
                            flush=True,
                        )
                    tail_left -= 1
                    if tail_left <= 0:
                        break
                    time.sleep(5)
    finally:
        for p in loaders:
            try:
                os.killpg(os.getpgid(p.pid), signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                p.terminate()
        watch.stop()
        watch.join(timeout=10)

    hi = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
    elapsed = time.monotonic() - t0

    # Put back what the control broke, BEFORE the verdict is rendered, so a
    # scheduled control never hands the box back without a daemon. Only ever
    # after a death this run caused: a daemon that died on its own is
    # evidence, and restarting it would destroy the state the next reader
    # needs. Reported in the summary either way.
    # Read BEFORE any restore. `daemon_up_at_end` answers "did anything bring
    # it back on its own", and a restore this script performed would answer
    # yes to a question nobody asked.
    up_at_end = alive(read_pid())
    restored: bool | None = None
    if injected and injected.get("sent") and not a.no_restore:
        subprocess.run(
            [str(CLI), "daemon", "start"],
            capture_output=True,
            text=True,
            timeout=300,
            env=os.environ | {"SOVEREIGN_NO_STALE_WARN": "1"},
        )
        for _ in range(30):
            if port_serving():
                break
            time.sleep(5)
        restored = bool(port_serving()) and alive(read_pid())
        print(f"[control] daemon restored: {restored}", flush=True)

    log_class, evidence, corroboration = classify_daemon_death(
        lo, hi, self_stopped=bool(injected and injected.get("sent"))
    )
    restarts = len(watch.pid_transitions)
    # THREE independent detectors, deliberately. The log sees a death the
    # pid cannot (a receipt written between two polls, the process replaced
    # before the next one); a pid TRANSITION sees a death the log cannot
    # (SIGKILL, panic, kernel OOM — nothing is written); and a pid that is
    # simply NOT ALIVE sees the window between the kill and the relaunch,
    # when the pidfile still names a corpse. Reporting any one of them alone
    # is a blind spot in whichever direction it was chosen.
    gone = watch.pid_not_alive_samples > 0
    phantom = watch.phantom_running_samples > 0
    died = bool(log_class) or restarts > 0 or gone or phantom
    death_class = log_class or (
        DEATH_CRASH
        if (restarts or gone)
        else (DEATH_LISTENER_LOST if phantom else None)
    )
    # A death this run cannot pin on the load is COULD-NOT-JUDGE, not a
    # finding — the fourth verdict, and the one that keeps a peer's
    # `daemon stop` from being scored as the defect (ARCH §18.2).
    could_not_judge = died and death_class not in DEATH_ATTRIBUTABLE

    kinds = Counter(r.get("death_kind", "unclassified") for r in rows if r["died"])
    loads = sorted(r["load1_at_start"] for r in rows)
    walls = sorted(r["wall_ms"] for r in rows if not r["died"])
    peak_rss_mb = round(max(watch.rss_samples) / 1024) if watch.rss_samples else None

    summary = {
        "verdict": "died" if died else ALIVE,
        "death_class": death_class,
        "death_attributable": bool(died and not could_not_judge),
        "death_corroboration": corroboration,
        # Did any PEER declare they would touch this daemon? A `held` reading
        # on either end is the reason an unattributed SIGTERM stays
        # unattributed instead of being read as this run's finding.
        "peer_daemon_claim_scope": claim_scope,
        "peer_daemon_claim_at_start": claim_at_start,
        "peer_daemon_claim_at_end": daemon_claim_verdict(claim_scope),
        "death_evidence": evidence,
        "expected_death": a.expect_death,
        "injected": injected,
        "window_utc": [lo, hi],
        "elapsed_s": round(elapsed, 1),
        "minutes_requested": a.minutes,
        "concurrent_turns_in_flight": 1 + max(0, a.concurrency),
        "turns_driven": len(rows),
        "turns_answered": len(walls),
        "turns_died": sum(1 for r in rows if r["died"]),
        # The user-visible cost of the daemon's death, kept apart from the
        # daemon-side verdict above: this is what a client actually saw.
        "client_death_kinds": dict(kinds),
        "daemon_pid_at_start": pid0,
        "daemon_pid_at_end": read_pid(),
        "daemon_pid_transitions": watch.pid_transitions,
        "daemon_pid_not_alive_samples": watch.pid_not_alive_samples,
        "daemon_phantom_running_samples": watch.phantom_running_samples,
        # Whether anything BROUGHT IT BACK. Reported apart from the death
        # class because they are different failures with different owners: a
        # daemon that dies and returns in 90 s is a latency event, one that
        # dies and stays down is an outage, and a verdict that says only
        # "died" cannot tell an operator which they have.
        "daemon_up_at_end": up_at_end,
        "ended_early_on_death": tail_left is not None,
        "daemon_restored_after_injection": restored,
        "daemon_peak_rss_mb": peak_rss_mb,
        "daemon_rss_samples": len(watch.rss_samples),
        "load1_median": statistics.median(loads) if loads else None,
        "load1_min": loads[0] if loads else None,
        "load1_max": loads[-1] if loads else None,
        "wall_p50_ms": walls[len(walls) // 2] if walls else None,
    }
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    print("\n── daemon concurrency soak ─────────────────────────────────")
    print(f"  window            {lo} .. {hi}  ({elapsed:.0f}s)")
    print(f"  in flight         {summary['concurrent_turns_in_flight']} turns")
    print(f"  turns             {len(rows)} driven, {len(walls)} answered")
    print(f"  load1             median {summary['load1_median']} (covariate)")
    print(f"  daemon peak RSS   {peak_rss_mb} MB")
    if a.expect_death:
        if death_class == a.expect_death:
            print(f"  CONTROL PASSED    injected death seen as `{death_class}`")
            print(f"  evidence          {evidence or '(pidfile transition only)'}")
            print(json.dumps(summary, indent=2))
            return 0
        print(
            f"  CONTROL FAILED    expected `{a.expect_death}`, "
            f"detected `{death_class or 'nothing'}`"
        )
        print(json.dumps(summary, indent=2))
        return 3
    if could_not_judge:
        print(f"  COULD-NOT-JUDGE   the daemon died as `{death_class}`")
        print(f"  evidence          {evidence or '(no receipt)'}")
        print(f"  corroboration     {corroboration}")
        print(
            "  meaning           the daemon went down, and this run cannot show "
            "the load did it.\n"
            "                    Not a caught defect. Re-run."
        )
        print(json.dumps(summary, indent=2))
        return 2
    if died:
        print(f"  FAILED            the daemon died: `{death_class}`")
        print(f"  evidence          {evidence or '(no receipt — pidfile changed)'}")
        print(f"  corroboration     {corroboration}")
        print(f"  restarts          {restarts}, came back: {summary['daemon_up_at_end']}")
        print(f"  clients saw       {dict(kinds)}")
        print(json.dumps(summary, indent=2))
        return 1
    print("  PASSED            same daemon up at the end as at the start")
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
