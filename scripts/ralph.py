#!/usr/bin/env python3
"""ralph.py — the ralph state machine, in one place.

The queue is `ralph/STATE.md`; the file protocol is unchanged from the shell
drivers this replaces (`ralph/NEEDS_HUMAN.md`, `ralph/STOP`, `ralph/DONE`,
`ralph/.heartbeat`, `ralph/waiting`, `ralph/models.env`). Every terminal state
is DONE, an operator stop, or an escalation — the machine cannot resolve to
quietly stuck.

Subcommands:
  run        the serial campaign driver: one unit per session
  supervise  wrap a campaign command: bounded resolutions, progress by unit
  watch      the watchdog: needs-human, down, stalled, disk-low
  stop       write the operator STOP and wait for the loop to go down
  start      clear the operator STOP and bootstrap the installed job
  models     show or set ralph/models.env and kickstart the loaded job
  plan       print the queue's head and the model it routes to
  promote    make a staged campaign (ralph/next/<name>/) the active one
"""
from __future__ import annotations

import argparse
import dataclasses
import enum
import hashlib
import os
import pathlib
import re
import signal
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor

HASH_RE = re.compile(r"^[0-9a-f]{7,40}$")
ROW_RE = re.compile(
    r"^- \[(?P<mark>[x~ ])\] (?P<head>.*?) — depends \[(?P<deps>[^\]]*)\](?P<rest>.*)$"
)


def say(msg: str) -> None:
    print(f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} {msg}", flush=True)


def notify(title: str, body: str, enabled: bool = True) -> None:
    if not enabled:
        return
    try:
        subprocess.run(
            ["/usr/bin/osascript", "-e",
             f'display notification "{body}" with title "ralph: {title}"'],
            capture_output=True, timeout=10)
    except Exception:
        pass


def file_hash(path) -> str:
    p = pathlib.Path(path)
    if not p.exists():
        return ""
    return hashlib.sha256(p.read_bytes()).hexdigest()


def head_of(workdir) -> str:
    r = subprocess.run(["git", "-C", str(workdir), "rev-parse", "HEAD"],
                       capture_output=True, text=True)
    return r.stdout.strip()


def first_line(path) -> str:
    p = pathlib.Path(path)
    try:
        return p.read_text().splitlines()[0]
    except (OSError, IndexError):
        return ""


def halt(paths, reason, *, notifier=notify, notify_enabled=True):
    """The one halt: a package, a reason in STOP, a notification. Shared by
    the campaign and the pool so neither can invent a quieter stop."""
    pkg = paths.p(paths.needs_human)
    pkg.parent.mkdir(parents=True, exist_ok=True)
    pkg.write_text(f"# {reason}\n\nresolve by hand, then remove "
                   f"{paths.stop} {paths.needs_human}\n")
    if not pkg.stat().st_size:
        say(f"HALT could not write {pkg} (disk full?) — no decision package exists")
        notifier("halt-unwritable", reason, notify_enabled)
    paths.p(paths.stop).write_text(f"halt: {reason}\n")
    say(f"HALT: {reason}")
    notifier("HALT", reason, notify_enabled)
    return Result(Outcome.HALT, reason)


def guarded(fn, paths, *, notifier=notify, notify_enabled=True):
    """A driver's I/O error must become a package, not a traceback. The disk
    ceiling on 2026-09-15 crashed the supervisor mid-resolution (OSError 28)
    and left the job down with no decision package."""
    try:
        return fn()
    except OSError as e:
        halt(paths, f"unhandled I/O error: {e}", notifier=notifier,
             notify_enabled=notify_enabled)
        return 3


def wait_for_marker(paths, marker_timeout):
    """None to proceed, "wait" to yield this tick, or a reason string to halt."""
    waiting = paths.p(paths.waiting)
    if not waiting.exists():
        return None
    m = re.search(r"[A-Za-z0-9._/-]+\.done", waiting.read_text())
    if not m:
        say("ralph/waiting names no *.done marker — ignoring it")
        waiting.unlink()
        return None
    marker = paths.p(m.group(0))
    if marker.exists():
        say(f"{marker} present — resuming")
        waiting.unlink()
        return None
    age = int(time.time() - waiting.stat().st_mtime)
    if age >= marker_timeout:
        return (f"waiting on {marker} for {age}s (limit {marker_timeout}s) "
                "— the detached run never wrote its marker")
    say(f"waiting on {marker} (no session this tick, {age}s)")
    return "wait"


def resolver_prompt(paths, attempt, resolve_max, reason, charter=None):
    """The resolver's instruction. With a charter the session is the operator's
    delegate and decides; without one it defers design forks, as before."""
    head = (f"SUPERVISOR RESOLUTION (attempt {attempt} of {resolve_max}).\n\n"
            f"The campaign stopped short of DONE. Reason:\n  {reason}\n\n")
    if charter:
        return head + (
            "You are the DIRECTOR: the operator's delegate under the charter below.\n"
            f"Read the package (`{paths.needs_human}`), verify its facts, and DECIDE — do not\n"
            "defer a fork the charter covers. Reproduce every claim you rely on.\n\n"
            f"1. Read `{paths.needs_human}`, `{paths.state}`, `git status`, and the charter.\n"
            "   Campaign logs are under `~/.svrnmesh/ralph/` and `target/ralph/`.\n"
            "2. Apply the smallest change that makes the campaign flow: correct the row or\n"
            "   the code, with its source order corrected together when a premise was false.\n"
            "3. Record the decision in `ralph/DECISIONS.md` (date, unit, fork, choice,\n"
            "   evidence, what would falsify it; tag `REVIEW-AFTER:` when the charter did\n"
            "   not clearly cover it) and land it with the change.\n"
            f"4. Remove `{paths.needs_human}` so the campaign resumes, and commit.\n"
            f"   The supervisor cleared the old blocker STOP; a NEW `{paths.stop}` is an\n"
            "   operator request and you must not remove it.\n"
            "5. If the fork is one the charter leaves to the operator, say so in the\n"
            "   package — the options, their costs, and your recommendation — and stop. An\n"
            "   honest package beats a guessed decision.\n\n"
            "=== CHARTER ===\n" + charter)
    return head + (
        "You are the resolution session. Diagnose and fix so the campaign flows again:\n"
        f"1. Read `{paths.needs_human}`, `{paths.state}`, and `git status`. Campaign logs are\n"
        "   under `~/.svrnmesh/ralph/` and `target/ralph/`.\n"
        "2. Fix the blocker. A false premise may be corrected only from verified code or\n"
        "   consumer evidence, with the row and its source order corrected together.\n"
        "3. Do NOT weaken a PASS BAR and do not mark a unit [x] that has not earned it.\n"
        f"   Never approve or mark a HUMAN- row. If this is a genuine design fork, leave\n"
        f"   a clear `{paths.needs_human}` for the operator and stop.\n"
        f"4. When fixed: remove `{paths.needs_human}` so the campaign resumes, and commit.\n"
        f"   The supervisor cleared the old blocker STOP; a NEW `{paths.stop}` is an\n"
        "   operator request and you must not remove it.\n")


class Status(enum.Enum):
    PENDING = " "
    ACTIVE = "~"
    DONE = "x"


class Outcome(enum.Enum):
    DONE = "done"
    OPERATOR_STOP = "operator-stop"
    NEEDS_HUMAN = "needs-human"
    HALT = "halt"


@dataclasses.dataclass(frozen=True)
class Row:
    id: str
    status: Status
    deps: tuple
    hash: str | None
    lineno: int
    line: str


class Queue:
    """The queue grammar, typed. Both id-then-hash and legacy hash-then-id."""

    def __init__(self, path):
        self.path = pathlib.Path(path)
        self.rows = self._parse()
        ids = [r.id for r in self.rows]
        if len(ids) != len(set(ids)):
            raise ValueError(f"{self.path}: duplicate row ids")
        by = {r.id: r for r in self.rows}
        for r in self.rows:
            for d in r.deps:
                if d not in by:
                    raise ValueError(f"{self.path}:{r.lineno}: unknown dependency {d!r}")

    def _parse(self):
        rows = []
        for n, line in enumerate(self.path.read_text().splitlines(), 1):
            m = ROW_RE.match(line)
            if not m:
                continue
            tokens = m.group("head").split()
            row_id = next((t for t in tokens if not HASH_RE.fullmatch(t)), None)
            if row_id is None:
                raise ValueError(f"{self.path}:{n}: row has no id")
            commit = next((t for t in tokens if HASH_RE.fullmatch(t)), None)
            deps = tuple(d.strip() for d in m.group("deps").split(",") if d.strip())
            rows.append(Row(row_id, Status(m.group("mark")), deps, commit, n, line))
        return rows

    def by_id(self):
        return {r.id: r for r in self.rows}

    def done_count(self):
        return sum(1 for r in self.rows if r.status is Status.DONE)

    def deps_met(self, row):
        by = self.by_id()
        return all(by[d].status is Status.DONE for d in row.deps)

    def current(self):
        for r in self.rows:
            if r.status is Status.ACTIVE:
                return r
        for r in self.rows:
            if r.status is Status.PENDING and self.deps_met(r):
                return r
        return None

    def status_of(self, row_id):
        return self.by_id()[row_id].status

    def set_status(self, row_id, status):
        """Rewrite one row's checkbox, preserving the rest of the line."""
        row = self.by_id()[row_id]
        lines = self.path.read_text().splitlines()
        old = lines[row.lineno - 1]
        if old[:5] != f"- [{row.status.value}]":
            raise ValueError(f"{self.path}:{row.lineno}: row moved under set_status")
        lines[row.lineno - 1] = f"- [{status.value}]" + old[5:]
        self.path.write_text("\n".join(lines) + "\n")
        self.rows = self._parse()

    def all_done(self):
        return all(r.status is Status.DONE for r in self.rows)

    def first_ready_review(self):
        for r in self.rows:
            if (r.status in (Status.PENDING, Status.ACTIVE)
                    and r.id.startswith("REVIEW-") and self.deps_met(r)):
                return r
        return None

    def pick_wave(self, lanes, conflicts, heavy=frozenset()):
        """Ready non-review units, up to `lanes`, no conflicting pair.
        `[~]` rows are resumable lanes: a killed session leaves one behind.
        At most ONE heavy row (ralph/heavy.txt, loaded by the Pool) per wave —
        the big moves run sequentially (2026-09-17, operator direction: derisk
        first); the other lane takes non-heavy rows."""
        wave = []
        for r in self.rows:
            if len(wave) >= lanes:
                break
            if r.status not in (Status.PENDING, Status.ACTIVE):
                continue
            if r.id.startswith("REVIEW-"):
                continue
            if r.id.startswith("HUMAN-"):
                continue          # operator-only; the run loop asks for it
            if not self.deps_met(r):
                continue
            if r.id in heavy and any(w in heavy for w in wave):
                continue
            if any(frozenset((r.id, w)) in conflicts for w in wave):
                continue
            wave.append(r.id)
        return wave


MODEL_KEYS = ("MODEL", "REVIEW_MODEL", "RESOLVE_MODEL", "VARIANT")


def load_models(path):
    """Strict KEY=value data; unknown keys and comments are ignored."""
    out = {}
    p = pathlib.Path(path)
    if not p.exists():
        return out
    for line in p.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        if key.strip() in MODEL_KEYS:
            out[key.strip()] = value.strip()
    return out


def select_model_args(row_id, model, review_model, variant):
    chosen = review_model if (review_model and "review" in row_id.lower()) else model
    args = []
    if chosen:
        args += ["--model", chosen]
    if variant:
        args += ["--variant", variant]
    return args


@dataclasses.dataclass
class Paths:
    workdir: pathlib.Path
    state: str = "ralph/STATE.md"
    prompt: str = "ralph/PROMPT.md"
    done: str = "ralph/DONE"
    stop: str = "ralph/STOP"
    needs_human: str = "ralph/NEEDS_HUMAN.md"
    waiting: str = "ralph/waiting"
    models: str = "ralph/models.env"
    heartbeat: str = "ralph/.heartbeat"

    def p(self, rel):
        return self.workdir / rel


# A driver heartbeat younger than this means a loop is live. One number for the
# watchdog's "stalled" and promote's "still running".
STALL_SECS = 300


def job_name(paths, label):
    return f"dev.ralph.{paths.workdir.name}-{label}"


def job_running(paths, label):
    r = subprocess.run(["launchctl", "print", f"gui/{os.getuid()}/{job_name(paths, label)}"],
                       capture_output=True, text=True)
    return "state = running" in r.stdout


def sessions_under(workdir):
    """PIDs of `opencode run` processes whose cwd is under `workdir` — the
    strays a bootout can leave when the code predates the SIGTERM handler."""
    pids = []
    r = subprocess.run(["pgrep", "-f", "opencode run"], capture_output=True, text=True)
    for pid in r.stdout.split():
        c = subprocess.run(["lsof", "-a", "-d", "cwd", "-p", pid, "-Fn"],
                           capture_output=True, text=True)
        for line in c.stdout.splitlines():
            if line.startswith("n") and line[1:].startswith(str(workdir)):
                pids.append(int(pid))
                break
    return pids


# The Popen of every running session, so a SIGTERM (launchd bootout, Ctrl-C)
# can take the sessions down with the process — they run in their own process
# groups (start_new_session) and otherwise survive a bootout as orphans
# (2026-09-16).
_ACTIVE_SESSIONS = set()


def _install_signal_handlers():
    def _term(signum, _frame):
        for proc in list(_ACTIVE_SESSIONS):
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                pass
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, _term)
    signal.signal(signal.SIGINT, _term)


class Session:
    """One opencode session in its own process group, with a wall-clock
    timeout, a STOP check, a heartbeat, and permission-reject detection."""

    def __init__(self, paths, *, timeout=3600, opencode=None, poll=30,
                 notifier=notify, notify_enabled=True, cwd=None, env=None):
        self.paths = paths
        self.timeout = timeout
        self.opencode = opencode or os.environ.get("RALPH_OPENCODE_BIN", "opencode")
        self.poll = poll
        self.notifier = notifier
        self.notify_enabled = notify_enabled
        self.cwd = pathlib.Path(cwd) if cwd else paths.workdir
        self.env = env or {}

    def heartbeat(self, context):
        try:
            self.paths.p(self.paths.heartbeat).write_text(f"{int(time.time())} {context}\n")
        except OSError:
            pass

    def run(self, model_args, prompt_text, log_path):
        workdir = str(self.cwd)
        status = subprocess.run(["git", "-C", workdir, "status", "--porcelain"],
                                capture_output=True, text=True).stdout
        note = ""
        if status:
            note = ("NOTE: the tree holds uncommitted work from a prior session:\n"
                    + status
                    + "Inspect it and continue from it; do not discard work already done. "
                      "Commit it as you go.\n\n")
        log = pathlib.Path(log_path)
        log.parent.mkdir(parents=True, exist_ok=True)
        with open(log, "w") as fh:
            proc = subprocess.Popen(
                [self.opencode, "run", *model_args, note + prompt_text],
                cwd=workdir, env={**os.environ, **self.env},
                stdout=fh, stderr=subprocess.STDOUT, start_new_session=True)
        _ACTIVE_SESSIONS.add(proc)
        waited = 0
        while proc.poll() is None and waited < self.timeout:
            time.sleep(min(self.poll, max(1, self.timeout - waited)))
            waited += self.poll
            self.heartbeat(f"session {self.paths.workdir.name} {waited}s")
            if self.paths.p(self.paths.stop).exists():
                say("STOP requested — killing the session group")
                self._kill(proc)
                break
        if proc.poll() is None:
            say(f"session exceeded {self.timeout}s — killing its group")
            self.notifier("timeout", f"killed at {self.timeout}s", self.notify_enabled)
            self._kill(proc)
        rc = proc.wait()
        self.heartbeat(f"session-end {self.paths.workdir.name}")
        try:
            rejects = log.read_text(errors="replace").count("auto-rejecting")
        except OSError:
            rejects = 0
        if rejects:
            say(f"WARNING: {rejects} permission auto-rejections — extend opencode.json")
            self.notifier("permissions", f"{rejects} auto-rejections", self.notify_enabled)
        _ACTIVE_SESSIONS.discard(proc)
        return rc

    @staticmethod
    def _kill(proc):
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
        except ProcessLookupError:
            return
        time.sleep(5)
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except ProcessLookupError:
            pass


@dataclasses.dataclass
class Result:
    outcome: Outcome
    reason: str = ""


class Campaign:
    """The serial driver. Returns a Result; never exits the process."""

    def __init__(self, paths, *, session_run, notifier=notify, notify_enabled=True,
                 sleep=time.sleep, max_stall=3, max_iter=200, marker_timeout=7200,
                 wait_poll=120, model="", review_model="", variant=""):
        self.paths = paths
        self.session_run = session_run
        self.notifier = notifier
        self.notify_enabled = notify_enabled
        self.sleep = sleep
        self.max_stall = max_stall
        self.max_iter = max_iter
        self.marker_timeout = marker_timeout
        self.wait_poll = wait_poll
        self.model = model
        self.review_model = review_model
        self.variant = variant

    def halt(self, reason):
        return halt(self.paths, reason, notifier=self.notifier,
                    notify_enabled=self.notify_enabled)

    def run(self):
        stall = 0
        iteration = 0
        while iteration < self.max_iter:
            iteration += 1
            self._beat(f"iteration {iteration}")
            if self.paths.p(self.paths.stop).exists():
                return Result(Outcome.OPERATOR_STOP, "stop file present")
            if self.paths.p(self.paths.done).exists():
                return Result(Outcome.DONE, "done file present")
            if self.paths.p(self.paths.needs_human).exists() \
                    and self.paths.p(self.paths.needs_human).stat().st_size:
                return Result(Outcome.NEEDS_HUMAN,
                              first_line(self.paths.p(self.paths.needs_human)))
            marker = wait_for_marker(self.paths, self.marker_timeout)
            if marker is not None and marker != "wait":
                return self.halt(marker)
            if marker == "wait":
                self.sleep(self.wait_poll)
                iteration -= 1
                continue
            # Re-read every iteration: the worker mutates the queue as it goes.
            unit = Queue(self.paths.p(self.paths.state)).current()
            if unit is None:
                return self.halt(f"no ready unit in {self.paths.state}; check dependencies")
            if unit.id.startswith("HUMAN-"):
                return self.halt(f"operator approval required: {unit.id}")
            model_args = select_model_args(unit.id, self.model, self.review_model, self.variant)
            say(f"unit {unit.id} — {' '.join(model_args) or 'configured default'}")
            before = head_of(self.paths.workdir)
            note = (f"Your unit: {unit.id} — its row in ralph/STATE.md is the [~] row, "
                    "or the first ready [ ] row. Open only that row; do not scan the "
                    "queue for another.\n\n")
            self.session_run(model_args, note + self._prompt_text(), self._log_path(iteration))
            after = head_of(self.paths.workdir)
            if after != before:
                stall = 0
            else:
                stall += 1
                say(f"no commit this iteration (stall {stall}/{self.max_stall})")
                if stall >= self.max_stall:
                    return self.halt(f"{self.max_stall} iterations without a commit")
        return self.halt(f"MAX_ITER={self.max_iter} reached")

    def _beat(self, context):
        try:
            self.paths.p(self.paths.heartbeat).write_text(f"{int(time.time())} {context}\n")
        except OSError:
            pass

    def _prompt_text(self):
        return self.paths.p(self.paths.prompt).read_text()

    def _log_path(self, iteration):
        return str(self.paths.workdir / "target" / "ralph" / f"iter-{iteration}.out")


class Supervisor:
    """Runs a campaign command; a stop short of DONE is either terminal, an
    operator escalation, or a bounded resolution. Progress is a unit completed."""

    def __init__(self, paths, *, run_inner, resolver_run, notifier=notify,
                 notify_enabled=True, resolve_max=4, state_path=None):
        self.paths = paths
        self.run_inner = run_inner
        self.resolver_run = resolver_run
        self.notifier = notifier
        self.notify_enabled = notify_enabled
        self.resolve_max = resolve_max

    def _queue(self):
        # A worker may be mid-write; the frequent checks tolerate that, the
        # campaign's own read does not.
        try:
            return Queue(self.paths.p(self.paths.state))
        except (OSError, ValueError):
            return None

    def _record_director_range(self, before, after, attempt, reason):
        """Name the director's commits so the morning review can revert one:
        `git revert <sha>` works because a decision is its own commit."""
        log = self.paths.p("ralph/.director-commits")
        try:
            with log.open("a") as fh:
                fh.write(f"{int(time.time())} attempt={attempt} {before}..{after} — {reason}\n")
        except OSError:
            pass

    def terminal_stop(self):
        if self.paths.p(self.paths.done).exists():
            say("supervisor: campaign DONE")
            self.notifier("DONE", "campaign complete", self.notify_enabled)
            return 0
        stop = self.paths.p(self.paths.stop)
        pkg = self.paths.p(self.paths.needs_human)
        # An EMPTY STOP is the operator's and wins over any package beside it:
        # requiring the absence of a package made a stop unhonored, which kept
        # the supervisor dispatching resolutions (2026-09-16).
        if stop.exists() and not stop.stat().st_size:
            say("supervisor: operator STOP — leaving it stopped")
            self.notifier("STOP", "operator stop preserved", self.notify_enabled)
            return 0
        if stop.exists() and stop.stat().st_size and not (pkg.exists() and pkg.stat().st_size):
            pkg.write_text(f"{stop.read_text()}\nresolve by hand, then remove "
                           f"{self.paths.stop} {self.paths.needs_human}\n")
            say(f"supervisor: halt package was missing — wrote one from {self.paths.stop}")
        queue = self._queue()
        unit = queue.current() if queue else None
        if unit is not None and unit.id.startswith("HUMAN-"):
            say(f"supervisor: operator approval required — {unit.id} (no resolution session)")
            self.notifier("NEEDS_HUMAN", f"approval required: {unit.id}", self.notify_enabled)
            return 2
        return None

    def run(self):
        last_done = self._queue().done_count()
        attempt = 0
        while True:
            stop = self.terminal_stop()
            if stop is not None:
                return stop
            self.run_inner()
            stop = self.terminal_stop()
            if stop is not None:
                return stop
            done_now = self._queue().done_count()
            if done_now > last_done:
                attempt = 0
                last_done = done_now
            pkg = self.paths.p(self.paths.needs_human)
            reason = first_line(pkg) if pkg.exists() and pkg.stat().st_size else "campaign exited"
            say(f"supervisor: campaign stopped — {reason}")
            attempt += 1
            if attempt > self.resolve_max:
                say(f"supervisor: {self.resolve_max} resolution attempts did not clear it "
                    "— leaving it to the operator")
                self.notifier("supervisor", f"unresolved after {self.resolve_max}: {reason}",
                              self.notify_enabled)
                return 2
            pkg_before = file_hash(pkg)
            head_before = head_of(self.paths.workdir)
            stop_file = self.paths.p(self.paths.stop)
            if pkg.exists() and pkg.stat().st_size:
                stop_file.unlink(missing_ok=True)
            say(f"supervisor: dispatching resolution session {attempt} — {reason}")
            self.notifier("resolving", f"attempt {attempt}: {reason}", self.notify_enabled)
            self.resolver_run(attempt, reason)
            head_after = head_of(self.paths.workdir)
            if head_after and head_after != head_before:
                self._record_director_range(head_before, head_after, attempt, reason)
            if stop_file.exists():
                say("supervisor: operator STOP during resolution — leaving it stopped")
                self.notifier("STOP", "resolution interrupted; operator stop preserved",
                              self.notify_enabled)
                return 0
            if pkg.exists() and pkg.stat().st_size:
                if file_hash(pkg) == pkg_before and head_of(self.paths.workdir) == head_before:
                    say(f"supervisor: resolution {attempt} changed nothing — escalating")
                    self.notifier("escalate", f"resolution achieved nothing: {reason}",
                                  self.notify_enabled)
                    return 2
                say(f"supervisor: resolution {attempt} left NEEDS_HUMAN — retrying")
            else:
                say(f"supervisor: resolution {attempt} cleared the halt — resuming the campaign")


class Pool:
    """The parallel driver: waves of ready units in git worktrees, serial
    merges, a conflict halts (never auto-resolved). REVIEW rows run serially
    in the main tree. Progress is the same file protocol as the serial flow."""

    def __init__(self, paths, *, session_for, notifier=notify, notify_enabled=True,
                 lanes=2, base_branch="", conflicts="ralph/conflicts.txt",
                 prompt="ralph/PROMPT.md", state="ralph/STATE.md",
                 marker_timeout=7200, wait_poll=120, sleep=time.sleep,
                 model="", review_model="", variant="", max_review_attempts=3,
                 max_lane_failures=3):
        self.paths = paths
        self.session_for = session_for
        self.notifier = notifier
        self.notify_enabled = notify_enabled
        self.lanes = lanes
        self.base_branch = base_branch
        self.conflicts = conflicts
        self.prompt = prompt
        self.state = state
        self.marker_timeout = marker_timeout
        self.wait_poll = wait_poll
        self.sleep = sleep
        self.model = model
        self.review_model = review_model
        self.variant = variant
        self.max_review_attempts = max_review_attempts
        self.max_lane_failures = max_lane_failures
        self._lane_failures = {}

    def _git(self, *args, cwd=None):
        return subprocess.run(["git", "-C", str(cwd or self.paths.workdir), *args],
                              capture_output=True, text=True)

    def _queue(self):
        try:
            return Queue(self.paths.p(self.state))
        except (OSError, ValueError):
            return None

    def _conflict_pairs(self):
        pairs = set()
        p = self.paths.p(self.conflicts)
        if p.exists():
            for line in p.read_text().splitlines():
                parts = line.split()
                if len(parts) >= 2:
                    pairs.add(frozenset(parts[:2]))
        return pairs

    def _heavy(self):
        """The heavy rows (ralph/heavy.txt): at most one per wave."""
        p = self.paths.p("ralph/heavy.txt")
        if not p.exists():
            return set()
        out = set()
        for line in p.read_text().splitlines():
            line = line.split("#")[0].strip()
            if line:
                out.add(line)
        return out

    def _halt(self, reason):
        halt(self.paths, reason, notifier=self.notifier, notify_enabled=self.notify_enabled)
        return 3

    def _prompt_text(self):
        return self.paths.p(self.prompt).read_text()

    def run(self):
        # `ralph/lanes/` not `ralph/done/`: on a case-insensitive filesystem
        # (macOS) the lane-marker directory and `ralph/DONE` are one path, and
        # the completion marker could never be written.
        (self.paths.workdir / "ralph" / "lanes").mkdir(parents=True, exist_ok=True)
        (self.paths.workdir / ".ralph" / "wt").mkdir(parents=True, exist_ok=True)
        say(f"pool: lanes={self.lanes} base={self.base_branch}")
        while True:
            if self.paths.p(self.paths.stop).exists():
                say("pool: STOP")
                return 0
            queue = self._queue()
            if queue is None:
                self.sleep(60)
                continue
            if queue.all_done():
                self.paths.p(self.paths.done).write_text("")
                say("pool: DONE — all units [x]")
                self.notifier("DONE", "pool complete", self.notify_enabled)
                return 0
            marker = wait_for_marker(self.paths, self.marker_timeout)
            if marker is not None and marker != "wait":
                return self._halt(marker)
            if marker == "wait":
                self.sleep(self.wait_poll)
                continue
            unit = queue.current()
            if unit is not None and unit.id.startswith("HUMAN-"):
                return self._halt(f"operator approval required: {unit.id}")
            review = queue.first_ready_review()
            if review is not None:
                result = self.run_review(review)
                if result is not None:
                    return result
                continue
            wave = queue.pick_wave(self.lanes, self._conflict_pairs(), self._heavy())
            if not wave:
                say("pool: no ready unit and no ready review — waiting (dependencies unmet?)")
                self.sleep(60)
                continue
            result = self.run_wave(wave)
            if result is not None:
                return result

    def run_review(self, review):
        session = self.session_for(self.paths.workdir)
        model_args = select_model_args(review.id, self.model, self.review_model, self.variant)
        log = str(self.paths.workdir / "target" / "ralph" / f"review-{review.id}.out")
        # The session must be told which row it owns: without the note it
        # follows PROMPT §1 and picks the first ready row, which for domains is
        # a minted dm- row above the review — the 2026-09-16 halt
        # (REVIEW-mint-mesh-rest, 3 attempts, worked other rows and never
        # marked itself [x]).
        note = (f"Your unit: {review.id} — the pool selected it as the ready review "
                "row. Open only that row in ralph/STATE.md; do not scan the queue for "
                "another.\n\n")
        for attempt in range(1, self.max_review_attempts + 1):
            if self.paths.p(self.paths.stop).exists():
                say("pool: operator STOP — leaving the review")
                return 0
            say(f"pool: serial review {review.id} (main tree) attempt {attempt}")
            note = (f"Your unit: {review.id} — the pool selected it as the ready review "
                    "row. Open only that row in ralph/STATE.md; do not scan the queue for "
                    "another.\n\n")
            if attempt > 1:
                # A retried review re-derived its whole analysis every attempt
                # until 2026-09-17 (REVIEW-audit-daemon-1, four hours): the
                # delta was in the tree, the session started from scratch. On a
                # retry, say so.
                note += (f"This is attempt {attempt}: the prior attempt(s) left their work "
                         "uncommitted in the tree (the note above lists it). Finish it — "
                         "run the row's checks, fix only what is red, commit the delta BY "
                         "NAME, record what you have, mark [x]. Do not re-derive the "
                         "analysis.\n\n")
            session.run(model_args, note + self._prompt_text(), log)
            if self.paths.p(self.paths.stop).exists():
                say("pool: operator STOP — leaving the review")
                return 0
            queue = self._queue()
            if queue is not None and queue.status_of(review.id) is Status.DONE:
                return None
            # A worker that stops per PROMPT §6 leaves its package for the
            # director; without this check the pool re-runs the row to the
            # attempt limit (2026-09-16, REVIEW-build-mesh-api-decouple). The
            # package is NOT rewritten — it is the worker's evidence.
            pkg = self.paths.p(self.paths.needs_human)
            if pkg.exists() and pkg.stat().st_size:
                say(f"pool: review {review.id} left NEEDS_HUMAN.md — stopping for the director")
                self.notifier("NEEDS_HUMAN", first_line(pkg), self.notify_enabled)
                return 3
            marker = wait_for_marker(self.paths, self.marker_timeout)
            if marker == "wait":
                return None          # handed off to a detached run; resume the loop
            if marker is not None:
                return self._halt(marker)
            say(f"pool: review {review.id} did not mark [x] (attempt {attempt}) — resuming")
        return self._halt(f"review {review.id} did not finish after "
                          f"{self.max_review_attempts} attempts")

    def run_lane(self, unit):
        wt = self.paths.workdir / ".ralph" / "wt" / unit
        branch = f"ralph/{unit}"
        if not wt.exists():
            r = self._git("worktree", "add", "-q", "-b", branch, str(wt), self.base_branch)
            if r.returncode != 0:
                say(f"pool: worktree add failed for {unit}: {r.stderr.strip()}")
                return
            say(f"pool: lane start {unit} (worktree {wt})")
        else:
            say(f"pool: lane {unit} resuming in its existing worktree")
            # A lane worktree is created once from the base branch and kept
            # across waves, so a harness fix that lands on the base never
            # reaches it. dm-daemon-api-edge burned its three waves on the
            # stale `.opencode/opencode.json` that `bdb0f24e0` fixed on the
            # base minutes after the lane exhausted (2026-09-17). Fast-forward
            # a lane that has no commits of its own onto the base; a lane with
            # work is left alone (--ff-only refuses to rewrite it).
            own = self._git("rev-list", "--count", f"{self.base_branch}..HEAD", cwd=wt)
            if own.returncode == 0 and own.stdout.strip() == "0":
                ff = self._git("merge", "--ff-only", self.base_branch, cwd=wt)
                if ff.returncode == 0:
                    say(f"pool: lane {unit} refreshed onto {self.base_branch}")
                else:
                    say(f"pool: lane {unit} not refreshed: {ff.stderr.strip()}")
        note = (f"POOL LANE: you are working unit {unit} in an isolated git worktree.\n"
                f"Commit your work here. When the unit passes its OWN tests, write "
                f"ralph/lanes/{unit}.done and commit it — the pool merges your branch then.\n"
                "Do NOT edit ralph/STATE.md; the pool marks the unit done after the merge.\n\n")
        model_args = select_model_args(unit, self.model, self.review_model, self.variant)
        # One lock per lane: a lane builds in its own worktree/target, so the
        # shared /tmp lock would only serialize lanes against each other and
        # against other campaigns (2026-09-16 speed order).
        lock_dir = f"/tmp/svrn-cargo-lock.{os.getuid()}.lane-{unit}"
        session = self.session_for(wt, env={"SVRN_CARGO_LOCK_DIR": lock_dir})
        session.run(model_args, note + self._prompt_text(),
                    str(self.paths.workdir / "target" / "ralph" / f"lane-{unit}.out"))

    def run_wave(self, wave):
        say(f"pool: wave {', '.join(wave)}")
        with ThreadPoolExecutor(max_workers=len(wave)) as ex:
            list(ex.map(self.run_lane, wave))
        if self.paths.p(self.paths.stop).exists():
            say("pool: operator STOP — leaving the lanes unmerged (their worktrees resume)")
            return 0
        for unit in wave:
            wt = self.paths.workdir / ".ralph" / "wt" / unit
            branch = f"ralph/{unit}"
            if not wt.exists():
                continue
            lane_pkg = wt / "ralph" / "NEEDS_HUMAN.md"
            if lane_pkg.exists() and lane_pkg.stat().st_size:
                # A lane writes its package in ITS worktree — the main-tree
                # check never saw it, so the wave re-ran the row to the failure
                # limit with the package sitting right there (2026-09-17,
                # dm-daemon-api-edge). Surface it where the operator and the
                # director look, then stop.
                main_pkg = self.paths.p(self.paths.needs_human)
                main_pkg.write_text(f"# lane {unit} left this package "
                                    f"({lane_pkg})\n\n" + lane_pkg.read_text())
                say(f"pool: lane {unit} left NEEDS_HUMAN.md — stopping for the director")
                self.notifier("NEEDS_HUMAN", first_line(lane_pkg), self.notify_enabled)
                return 3
            if not (wt / "ralph" / "lanes" / f"{unit}.done").exists():
                # A lane that keeps ending without its marker would otherwise be
                # re-run forever (2026-09-17: ~50 sessions over 2.5h on
                # dm-daemon-api-edge). Bound it and hand the row to the director.
                n = self._lane_failures.get(unit, 0) + 1
                self._lane_failures[unit] = n
                say(f"pool: lane {unit} ended without ralph/lanes/{unit}.done "
                    f"(failure {n}/{self.max_lane_failures}) — branch {branch} kept")
                if n >= self.max_lane_failures:
                    return self._halt(f"lane {unit} failed {n} waves — see "
                                      f"target/ralph/lane-{unit}.out and branch {branch}")
                continue
            self._lane_failures.pop(unit, None)
            say(f"pool: lane {unit} finished — merging {branch}")
            r = self._git("merge", "--no-ff", "-m", f"merge {unit}", branch)
            if r.returncode != 0:
                self._git("merge", "--abort")
                return self._halt(f"merge conflict merging {branch} — resolve in the "
                                  "main tree, then resume")
            queue = self._queue()
            if queue is not None:
                queue.set_status(unit, Status.DONE)
                self._git("add", self.state)
                self._git("commit", "-q", "-m", f"{unit}: merged (pool)")
            self._git("worktree", "remove", "--force", str(wt))
            self._git("branch", "-D", branch)
            say(f"pool: lane {unit} merged and marked [x]")
        if self._lane_failures:
            self.sleep(60)          # backoff between failed waves, never a hot loop
        return None


class Watch:
    """The watchdog conditions, in order of precedence."""

    def __init__(self, paths, *, label, running=None, disk_free_mb=None,
                 stall_secs=STALL_SECS, min_free_mb=5120, notifier=notify, dry=False):
        self.paths = paths
        self.label = label
        self.running = running or self._running
        self.disk_free_mb = disk_free_mb or self._disk_free_mb
        self.stall_secs = stall_secs
        self.min_free_mb = min_free_mb
        self.notifier = notifier
        self.dry = dry

    def _running(self):
        return job_running(self.paths, self.label)

    def _disk_free_mb(self):
        r = subprocess.run(["df", "-m", "/System/Volumes/Data"],
                           capture_output=True, text=True)
        lines = r.stdout.splitlines()
        if len(lines) < 2:
            return None
        try:
            return int(lines[1].split()[3])
        except (IndexError, ValueError):
            return None

    def condition(self):
        pkg = self.paths.p(self.paths.needs_human)
        if pkg.exists() and pkg.stat().st_size:
            return ("needs-human:" + file_hash(pkg)[:16], first_line(pkg))
        if self.paths.p(self.paths.done).exists():
            return None
        stop = self.paths.p(self.paths.stop)
        if stop.exists() and not stop.stat().st_size:
            return None
        if not self.running():
            return ("down", f"{self.paths.workdir.name}-{self.label} is not running and has "
                            "no DONE/operator-STOP")
        beat = self.paths.p(self.paths.heartbeat)
        if beat.exists():
            age = int(time.time() - beat.stat().st_mtime)
            if age > self.stall_secs:
                return ("stalled", f"{self.paths.workdir.name}-{self.label}: "
                                   f"no driver heartbeat for {age}s")
        free = self.disk_free_mb()
        if free is not None and free < self.min_free_mb:
            return ("disk-low", f"{self.paths.workdir.name}-{self.label}: {free}MB free")
        return None

    def run(self, state_file, nag_secs=1800):
        condition = self.condition()
        last_cond, last_ts = "", 0
        sf = pathlib.Path(state_file)
        if sf.exists():
            parts = sf.read_text().split()
            if len(parts) >= 2:
                last_cond, last_ts = parts[0], int(parts[1])
        now = int(time.time())
        if condition is None:
            sf.write_text("")
            return None
        cond, body = condition
        if cond != last_cond or now - last_ts >= nag_secs:
            title = {"needs-human": "needs human", "down": "loop down",
                     "stalled": "loop stalled", "disk-low": "disk low"}[cond.split(":")[0]]
            if self.dry:
                print(f"notify: {title}: {body}")
            else:
                self.notifier(title, body, True)
            sf.write_text(f"{cond} {now}\n")
        return cond


def install_launchd(plist_label, program_args, workdir, log_path, interval=None):
    plist = pathlib.Path.home() / "Library" / "LaunchAgents" / f"{plist_label}.plist"
    args = "\n".join(f"    <string>{a}</string>" for a in program_args)
    schedule = (f"  <key>StartInterval</key><integer>{interval}</integer>\n"
                if interval else "  <key>RunAtLoad</key><true/>\n")
    plist.write_text(f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{plist_label}</string>
  <key>ProgramArguments</key><array>
{args}
  </array>
  <key>WorkingDirectory</key><string>{workdir}</string>
{schedule}  <key>EnvironmentVariables</key><dict>
    <key>HOME</key><string>{pathlib.Path.home()}</string>
    <key>PATH</key><string>{os.environ.get("PATH", "")}</string>
  </dict>
  <key>StandardOutPath</key><string>{log_path}</string>
  <key>StandardErrorPath</key><string>{log_path}</string>
</dict></plist>
""")
    return plist


def ensure_excludes(workdir, rel_paths):
    """Runtime markers must not dirty the tree the campaign commits into."""
    git_info = pathlib.Path(workdir) / ".git" / "info"
    if not git_info.parent.exists():
        return
    git_info.mkdir(parents=True, exist_ok=True)
    exclude = git_info / "exclude"
    existing = set(exclude.read_text().splitlines()) if exclude.exists() else set()
    with exclude.open("a") as fh:
        for rel in rel_paths:
            if rel not in existing:
                fh.write(f"{rel}\n")


def state_dir_for(paths, label):
    d = pathlib.Path.home() / ".svrnmesh" / "ralph" / f"{paths.workdir.name}-{label}"
    d.mkdir(parents=True, exist_ok=True)
    return d


RUNTIME_MARKERS = ("ralph/DONE", "ralph/STOP", "ralph/NEEDS_HUMAN.md",
                   "ralph/.heartbeat", "ralph/waiting", "ralph/models.env",
                   "ralph/log.txt", "ralph/.director-commits")


def cmd_plan(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    queue = Queue(paths.p(paths.state))
    models = load_models(paths.p(paths.models))
    model = args.model or models.get("MODEL", "")
    review = args.review_model or models.get("REVIEW_MODEL", "")
    variant = args.variant or models.get("VARIANT", "")
    unit = queue.current()
    print(f"prompt: {paths.prompt}  queue: {paths.state}")
    print(f"commits since review: n/a  head: {head_of(paths.workdir)[:9]}")
    print(f"done: {queue.done_count()}/{len(queue.rows)}")
    if unit is None:
        print("no ready unit")
        return 0
    routed = select_model_args(unit.id, model, review, variant)
    print(f"unit {unit.id} — {' '.join(routed) or 'configured default'}")
    return 0


# A staged campaign is a directory nothing in the loop reads until `promote`.
STAGED_DIR = "ralph/next"
ARCHIVE_DIR = "ralph/archive"
# What belongs to ONE campaign: authored before it runs, or its ledger while it
# runs. Per-host runtime files (models.env, log.txt, markers) are not in it.
CAMPAIGN_FILES = ("STATE.md", "PROMPT.md", "CHARTER.md", "heavy.txt", "conflicts.txt",
                  "DECISIONS.md", "REVIEW_FINDINGS.md", ".director-commits", "lanes")


def staged_campaigns(paths):
    root = paths.p(STAGED_DIR)
    if not root.is_dir():
        return []
    return sorted(d for d in root.iterdir() if (d / "STATE.md").is_file())


def cmd_promote(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    staged = paths.p(f"{STAGED_DIR}/{args.name}")
    missing = [f for f in ("STATE.md", "PROMPT.md") if not (staged / f).is_file()]
    if missing:
        print(f"promote: {STAGED_DIR}/{args.name} lacks {', '.join(missing)}", file=sys.stderr)
        return 2
    queue = Queue(staged / "STATE.md")
    if not queue.rows:
        print(f"promote: {STAGED_DIR}/{args.name}/STATE.md parses to zero rows", file=sys.stderr)
        return 2
    head = queue.current()
    print(f"staged {args.name}: {len(queue.rows)} rows, done {queue.done_count()}, "
          f"head {head.id if head else 'none'}")
    if args.dry_run:
        return 0
    pkg = paths.p(paths.needs_human)
    if pkg.exists() and pkg.stat().st_size:
        print(f"promote: {paths.needs_human} is unresolved — the active campaign is not over",
              file=sys.stderr)
        return 2
    if not (paths.p(paths.done).exists() or paths.p(paths.stop).exists()):
        print("promote: the active campaign has neither DONE nor an operator STOP — "
              "run `ralph.py stop` first", file=sys.stderr)
        return 2
    beat = paths.p(paths.heartbeat)
    beat_age = time.time() - beat.stat().st_mtime if beat.exists() else None
    if job_running(paths, args.label) or (beat_age is not None and beat_age < STALL_SECS):
        print(f"promote: a loop is still live (heartbeat {int(beat_age or 0)}s old) — "
              "wait for it to go down", file=sys.stderr)
        return 2
    archive = paths.p(f"{ARCHIVE_DIR}/{time.strftime('%Y%m%d-%H%M%S', time.gmtime())}"
                      f"-before-{args.name}")
    archive.mkdir(parents=True)
    for name in CAMPAIGN_FILES:
        if paths.p(f"ralph/{name}").exists():
            paths.p(f"ralph/{name}").rename(archive / name)
    for marker in (paths.done, paths.stop):
        if paths.p(marker).exists():
            paths.p(marker).unlink()
    for name in CAMPAIGN_FILES:
        if (staged / name).exists():
            (staged / name).rename(paths.p(f"ralph/{name}"))
    leftovers = sorted(p.name for p in staged.iterdir())
    if leftovers:
        say(f"promote: left in {STAGED_DIR}/{args.name}: {', '.join(leftovers)} "
            "(not campaign files)")
    else:
        staged.rmdir()
    say(f"promote: {args.name} is active; the previous campaign is in "
        f"{archive.relative_to(paths.workdir)}. Commit ralph/, then start the loop "
        f"with --label {args.name}")
    return 0


def cmd_models(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    file = paths.p(paths.models)
    current = load_models(file)
    if args.model or args.review_model or args.resolve_model or args.variant:
        current["MODEL"] = args.model or current.get("MODEL", "")
        current["REVIEW_MODEL"] = args.review_model or current.get("REVIEW_MODEL", "")
        current["RESOLVE_MODEL"] = args.resolve_model or current.get("RESOLVE_MODEL", "")
        current["VARIANT"] = args.variant or current.get("VARIANT", "")
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(
            "# ralph per-host model configuration (gitignored); written by scripts/ralph.py\n"
            f"MODEL={current.get('MODEL', '')}\n"
            f"REVIEW_MODEL={current.get('REVIEW_MODEL', '')}\n"
            f"RESOLVE_MODEL={current.get('RESOLVE_MODEL', '')}\n"
            f"VARIANT={current.get('VARIANT', '')}\n")
        if not args.no_restart and args.label:
            job = f"dev.ralph.{paths.workdir.name}-{args.label}"
            r = subprocess.run(["launchctl", "print", f"gui/{os.getuid()}/{job}"],
                               capture_output=True, text=True)
            if r.returncode == 0:
                subprocess.run(["launchctl", "kickstart", "-k", f"gui/{os.getuid()}/{job}"],
                               capture_output=True)
                print(f"restarted {job} (any in-flight session was killed)")
            else:
                print(f"{job} is not loaded — the change applies on the next start")
    print(f"models: {paths.models}")
    print(f"  MODEL={current.get('MODEL') or '<unset>'}")
    print(f"  REVIEW_MODEL={current.get('REVIEW_MODEL') or '<unset>'}")
    print(f"  RESOLVE_MODEL={current.get('RESOLVE_MODEL') or '<unset>'}")
    print(f"  VARIANT={current.get('VARIANT') or '<unset>'}")
    return 0


def cmd_pool(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    models = load_models(paths.p(paths.models))
    base = args.base_branch or subprocess.run(
        ["git", "-C", str(paths.workdir), "rev-parse", "--abbrev-ref", "HEAD"],
        capture_output=True, text=True).stdout.strip()

    def session_for(cwd, env=None):
        return Session(paths, timeout=args.session_timeout,
                       notify_enabled=args.notify, cwd=cwd, env=env)

    pool = Pool(paths, session_for=session_for, notify_enabled=args.notify,
                lanes=args.lanes, base_branch=base, conflicts=args.conflicts,
                prompt=args.prompt, state=args.state,
                marker_timeout=args.marker_timeout,
                model=args.model or models.get("MODEL", ""),
                review_model=args.review_model or models.get("REVIEW_MODEL", ""),
                variant=args.variant or models.get("VARIANT", ""))
    if args.install_launchd:
        ensure_excludes(paths.workdir, RUNTIME_MARKERS + (".ralph/",))
        inner = [sys.executable, str(pathlib.Path(__file__).resolve()), "pool",
                 "--workdir", str(paths.workdir), "--label", args.label,
                 "--prompt", args.prompt, "--state", args.state,
                 "--lanes", str(args.lanes)]
        if args.notify:
            inner.append("--notify")
        plist = install_launchd(f"dev.ralph.{paths.workdir.name}-{args.label}", inner,
                                paths.workdir,
                                str(state_dir_for(paths, args.label) / "launchd.log"))
        print(f"wrote {plist}")
        return 0
    return guarded(pool.run, paths, notify_enabled=args.notify)


def cmd_report(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    queue = Queue(paths.p(paths.state))
    done = queue.done_count()
    active = sum(1 for r in queue.rows if r.status is Status.ACTIVE)
    pending = sum(1 for r in queue.rows if r.status is Status.PENDING)
    print(f"queue: {len(queue.rows)} rows — done {done}, active {active}, pending {pending}")
    current = queue.current()
    print(f"current: {current.id if current else 'none'}")
    for d in staged_campaigns(paths):
        try:
            rows = f"{len(Queue(d / 'STATE.md').rows)} rows"
        except ValueError as e:
            # A staged queue that does not parse is a finding, not a crash: the
            # report is how the operator learns it before `promote` refuses.
            rows = f"DOES NOT PARSE — {e}"
        print(f"next up: {d.name} ({rows}, staged in {STAGED_DIR})")
    markers =[m for m in ("DONE", "STOP", "NEEDS_HUMAN.md") if paths.p(f"ralph/{m}").exists()]
    print(f"markers: {', '.join(markers) if markers else 'none'}")
    decisions = paths.p("ralph/DECISIONS.md")
    if decisions.exists():
        lines = [l for l in decisions.read_text().splitlines() if l.strip()]
        after = sum(1 for l in lines if "REVIEW-AFTER:" in l)
        print(f"\n=== ralph/DECISIONS.md — {after} REVIEW-AFTER, last {args.lines} lines ===")
        print("\n".join(lines[-args.lines:]))
    else:
        print("\nralph/DECISIONS.md: not written yet")
    ranges = paths.p("ralph/.director-commits")
    if ranges.exists():
        entries = [l for l in ranges.read_text().splitlines() if ".." in l]
        print(f"\n=== director commit ranges ({len(entries)}) — revert one with "
              "`git revert <sha>` ===")
        for line in entries[-args.lines:]:
            print(f"  {line}")
            rng = next((p for p in line.split() if ".." in p), "")
            if rng:
                out = subprocess.run(["git", "-C", str(paths.workdir), "log", "--oneline", rng],
                                     capture_output=True, text=True).stdout.strip()
                for l in out.splitlines()[:8]:
                    print(f"      {l}")
    log = subprocess.run(["git", "-C", str(paths.workdir), "log", "--oneline", "-12"],
                         capture_output=True, text=True).stdout.strip()
    print("\n=== recent commits ===")
    print(log)
    return 0


def cmd_watch(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    state_dir = state_dir_for(paths, args.label)
    watch = Watch(paths, label=args.label, dry=os.environ.get("RALPH_WATCH_DRY") == "1")
    if args.install_launchd:
        plist = install_launchd(
            f"dev.ralphwatch.{paths.workdir.name}-{args.label}",
            [sys.executable, str(pathlib.Path(__file__).resolve()),
             "watch", "--workdir", str(paths.workdir), "--label", args.label],
            paths.workdir, state_dir / "watch.log", interval=120)
        print(f"wrote {plist}")
        return 0
    watch.run(state_dir / "watch.state")
    return 0


def cmd_run(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    models = load_models(paths.p(paths.models))
    session = Session(paths, timeout=args.session_timeout,
                      notify_enabled=args.notify)
    campaign = Campaign(
        paths,
        session_run=lambda model_args, prompt, log: session.run(model_args, prompt, log),
        notify_enabled=args.notify,
        max_stall=args.max_stall, max_iter=args.max_iter,
        marker_timeout=args.marker_timeout,
        model=args.model or models.get("MODEL", ""),
        review_model=args.review_model or models.get("REVIEW_MODEL", ""),
        variant=args.variant or models.get("VARIANT", ""))
    if args.install_launchd:
        ensure_excludes(paths.workdir, RUNTIME_MARKERS)
        plist = install_launchd(
            f"dev.ralph.{paths.workdir.name}-{args.label}",
            [sys.executable, str(pathlib.Path(__file__).resolve()), "run",
             "--workdir", str(paths.workdir), "--label", args.label,
             "--prompt", args.prompt, "--state", args.state],
            paths.workdir, str(state_dir_for(paths, args.label) / "launchd.log"))
        print(f"wrote {plist}")
        return 0
    result = guarded(campaign.run, paths, notify_enabled=args.notify)
    if isinstance(result, int):
        return result
    print(f"campaign: {result.outcome.value} — {result.reason}")
    return {Outcome.DONE: 0, Outcome.OPERATOR_STOP: 0, Outcome.NEEDS_HUMAN: 2,
            Outcome.HALT: 3}[result.outcome]


def cmd_supervise(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    models = load_models(paths.p(paths.models))
    session = Session(paths, timeout=args.session_timeout, notify_enabled=args.notify)
    campaign = list(args.campaign)
    if campaign and campaign[0] == "--":
        campaign = campaign[1:]
    if not campaign:
        print("ralph supervise: the campaign command is required after --", file=sys.stderr)
        return 2
    charter_path = pathlib.Path(args.charter) if args.charter else paths.p("ralph/CHARTER.md")
    charter = charter_path.read_text() if charter_path.exists() else None
    if charter:
        say(f"supervisor: director charter loaded from {charter_path}")
    ensure_excludes(paths.workdir, RUNTIME_MARKERS)

    def run_inner():
        subprocess.run(campaign, cwd=str(paths.workdir))

    def resolver_run(attempt, reason):
        resolve_model = (args.resolve_model or models.get("RESOLVE_MODEL", "")
                         or args.review_model or models.get("REVIEW_MODEL", ""))
        resolve_variant = args.resolve_variant or args.variant or models.get("VARIANT", "")
        model_args = select_model_args("review", resolve_model, "", resolve_variant)
        prompt = resolver_prompt(paths, attempt, args.resolve_max, reason, charter)
        session.run(model_args, prompt, str(paths.workdir / "target" / "ralph"
                                           / f"supervise-{attempt}.out"))

    supervisor = Supervisor(paths, run_inner=run_inner, resolver_run=resolver_run,
                            notify_enabled=args.notify, resolve_max=args.resolve_max)
    if args.install_launchd:
        ensure_excludes(paths.workdir, RUNTIME_MARKERS)
        plist = install_launchd(
            f"dev.ralph.{paths.workdir.name}-{args.label}",
            [sys.executable, str(pathlib.Path(__file__).resolve()), "supervise",
             "--workdir", str(paths.workdir), "--label", args.label,
             "--session-timeout", str(args.session_timeout)] + campaign,
            paths.workdir, str(state_dir_for(paths, args.label) / "launchd.log"))
        print(f"wrote {plist}")
        return 0
    return guarded(supervisor.run, paths, notify_enabled=args.notify)


def cmd_stop(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    stop = paths.p(paths.stop)
    stop.parent.mkdir(parents=True, exist_ok=True)
    stop.write_text("")
    say(f"stop: wrote {paths.stop} — waiting up to {args.timeout}s for the loop to exit")
    deadline = time.time() + args.timeout
    while time.time() < deadline:
        if not job_running(paths, args.label):
            say("stop: the loop is down — stop complete")
            return 0
        time.sleep(3)
    if not args.hard:
        say("stop: still running — re-run with --hard to boot the job out "
            "(its SIGTERM handler takes the sessions with it)")
        return 1
    subprocess.run(["launchctl", "bootout",
                    f"gui/{os.getuid()}/{job_name(paths, args.label)}"],
                   capture_output=True, text=True)
    strays = sessions_under(paths.workdir)
    for pid in strays:
        try:
            os.killpg(os.getpgid(pid), signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            pass
    say(f"stop: booted out; {len(strays)} stray session(s) taken down")
    return 0


def cmd_start(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    plist = (pathlib.Path.home() / "Library" / "LaunchAgents"
             / f"{job_name(paths, args.label)}.plist")
    if not plist.exists():
        print(f"start: no plist at {plist} — install it first:\n"
              f"  python3 scripts/ralph.py supervise --workdir {paths.workdir} "
              f"--label {args.label} --install-launchd -- <campaign command>",
              file=sys.stderr)
        return 2
    stop = paths.p(paths.stop)
    if stop.exists():
        if stop.stat().st_size:
            print(f"start: {paths.stop} holds a halt reason — resolve it "
                  f"(read {paths.needs_human}) and remove the file first",
                  file=sys.stderr)
            return 2
        stop.unlink()
        say("start: cleared the operator STOP")
    job = job_name(paths, args.label)
    subprocess.run(["launchctl", "bootout", f"gui/{os.getuid()}/{job}"],
                   capture_output=True, text=True)
    r = subprocess.run(["launchctl", "bootstrap", f"gui/{os.getuid()}", str(plist)],
                       capture_output=True, text=True)
    if r.returncode != 0:
        print(f"start: bootstrap failed: {(r.stderr or r.stdout).strip()}", file=sys.stderr)
        return r.returncode
    say(f"start: {job} bootstrapped")
    return 0


def main(argv=None):
    _install_signal_handlers()
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    def common(p, notify_default=False):
        p.add_argument("--workdir", default=".")
        p.add_argument("--label", default="campaign")
        p.add_argument("--session-timeout", type=int, default=3600)
        p.add_argument("--marker-timeout", type=int, default=7200)
        p.add_argument("--notify", action="store_true", default=notify_default)
        p.add_argument("--model", default="")
        p.add_argument("--review-model", default="")
        p.add_argument("--variant", default="")

    p = sub.add_parser("run")
    common(p)
    p.add_argument("--prompt", default="ralph/PROMPT.md")
    p.add_argument("--state", default="ralph/STATE.md")
    p.add_argument("--max-stall", type=int, default=3)
    p.add_argument("--max-iter", type=int, default=200)
    p.add_argument("--install-launchd", action="store_true")
    p.set_defaults(fn=cmd_run)

    p = sub.add_parser("supervise")
    common(p, notify_default=True)
    p.add_argument("--resolve-model", default="")
    p.add_argument("--resolve-variant", default="")
    p.add_argument("--resolve-max", type=int, default=4)
    p.add_argument("--charter", default="", help="default: ralph/CHARTER.md when present")
    p.add_argument("--install-launchd", action="store_true")
    p.add_argument("campaign", nargs=argparse.REMAINDER)
    p.set_defaults(fn=cmd_supervise)

    p = sub.add_parser("pool")
    common(p)
    p.add_argument("--prompt", default="ralph/PROMPT.md")
    p.add_argument("--state", default="ralph/STATE.md")
    p.add_argument("--lanes", type=int, default=2)
    p.add_argument("--conflicts", default="ralph/conflicts.txt")
    p.add_argument("--base-branch", default="")
    p.add_argument("--install-launchd", action="store_true")
    p.set_defaults(fn=cmd_pool)

    p = sub.add_parser("watch")
    p.add_argument("--workdir", default=".")
    p.add_argument("--label", default="campaign")
    p.add_argument("--install-launchd", action="store_true")
    p.set_defaults(fn=cmd_watch)

    p = sub.add_parser("stop")
    p.add_argument("--workdir", default=".")
    p.add_argument("--label", default="campaign")
    p.add_argument("--timeout", type=int, default=180,
                   help="seconds to wait for the loop to go down before suggesting --hard")
    p.add_argument("--hard", action="store_true",
                   help="boot the job out and take stray sessions down")
    p.set_defaults(fn=cmd_stop)

    p = sub.add_parser("start")
    p.add_argument("--workdir", default=".")
    p.add_argument("--label", default="campaign")
    p.set_defaults(fn=cmd_start)

    p = sub.add_parser("models")
    p.add_argument("--workdir", default=".")
    p.add_argument("--label", default="")
    p.add_argument("--model", default="")
    p.add_argument("--review-model", default="")
    p.add_argument("--resolve-model", default="")
    p.add_argument("--variant", default="")
    p.add_argument("--no-restart", action="store_true")
    p.set_defaults(fn=cmd_models)

    p = sub.add_parser("report")
    p.add_argument("--workdir", default=".")
    p.add_argument("--lines", type=int, default=40)
    p.set_defaults(fn=cmd_report)

    p = sub.add_parser("plan")
    common(p)
    p.set_defaults(fn=cmd_plan)

    p = sub.add_parser("promote")
    p.add_argument("name", help="the staged campaign under ralph/next/")
    p.add_argument("--workdir", default=".")
    p.add_argument("--label", default="campaign", help="the ACTIVE loop's label")
    p.add_argument("--dry-run", action="store_true",
                   help="parse the staged queue and print its head; change nothing")
    p.set_defaults(fn=cmd_promote)

    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
