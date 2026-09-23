#!/usr/bin/env python3
"""ralph.py — the ralph state machine, in one place.

The queue is `ralph/STATE.md`; the file protocol is unchanged from the shell
drivers this replaces (`ralph/NEEDS_HUMAN.md`, `ralph/STOP`, `ralph/DONE`,
`ralph/.heartbeat`, `ralph/waiting`, `ralph/models.env`). Every terminal state
is DONE, an operator stop, or an escalation — the machine cannot resolve to
quietly stuck.

`--queue <name>` runs `ralph/next/<name>/` from its `queue.toml` instead: its
own state, prompt, charter, models, checks and control files (`ctl/` beside
the manifest), so two loops share a checkout. Without it every flag and
default is the legacy one. `audit_every = N` there makes `run` insert and
dispatch a `REVIEW-audit-` row once N units have landed without one.

Subcommands:
  run        the serial campaign driver: one unit per session
  supervise  wrap a campaign command: bounded resolutions, progress by unit
  watch      the watchdog: needs-human, down, stalled, disk-low
  stop       write the operator STOP and wait for the loop to go down
  start      clear the operator STOP and bootstrap the installed job
  models     show or set ralph/models.env and kickstart the loaded job
  plan       print the queue's head and the model it routes to
  promote    make a staged campaign (ralph/next/<name>/) the active one
  prompt     print the worker prompt a queue runs on (rendered or as written)
  check-argv the argv a queue's [checks] declares (scripts/ralph-check.sh asks)
"""
from __future__ import annotations

import argparse
import dataclasses
import enum
import hashlib
import itertools
import json
import os
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import time
import urllib.parse
from concurrent.futures import ThreadPoolExecutor

HASH_RE = re.compile(r"^[0-9a-f]{7,40}$")
ROW_RE = re.compile(
    r"^- \[(?P<mark>[x~ ])\] (?P<head>.*?) — depends \[(?P<deps>[^\]]*)\](?P<rest>.*)$"
)


def say(msg: str) -> None:
    print(f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} {msg}", flush=True)


class HostError(Exception):
    """The platform cannot do what was asked; the message names what is missing."""


class Host:
    """The two things a loop asks of the machine it runs on — a desktop
    notification and a detached job — behind one interface, so the FSM names
    neither osascript nor launchd. This base is the platform with no backend:
    it reports every request instead of dropping it (the old notify swallowed
    every failure, so a Linux host simply never heard from its loops)."""
    notifier = ""
    job_tools = ()

    def __init__(self, *, run=subprocess.run, which=shutil.which, home=None):
        self._run = run
        self._which = which
        self.home = pathlib.Path(home) if home else pathlib.Path.home()
        self._reported = set()

    def _absent(self, tools, kind):
        if not tools:
            return f"no {kind} backend for {sys.platform}"
        gone = [t for t in tools if not self._which(t)]
        return f"{', '.join(gone)} not found" if gone else ""

    def notify(self, title, body):
        absent = self._absent((self.notifier,) if self.notifier else (), "notification")
        if absent:
            say(f"notify ({absent}) — {title}: {body}")
            return False
        try:
            self._run(self._notify_argv(title, body), capture_output=True, timeout=10)
        except (OSError, subprocess.SubprocessError) as e:
            say(f"notify ({self.notifier} failed: {e}) — {title}: {body}")
            return False
        return True

    def require_jobs(self):
        absent = self._absent(self.job_tools, "job")
        if absent:
            raise HostError(f"{absent} — this host cannot run a detached ralph job")

    def job_running(self, name):
        try:
            self.require_jobs()
        except HostError as e:
            if name not in self._reported:
                self._reported.add(name)
                say(f"{e}: cannot tell whether {name} is running — treating it as not running")
            return False
        return self._job_running(name)

    def install_job(self, name, argv, workdir, log_path, interval=None):
        self.require_jobs()
        return self._install_job(name, [str(a) for a in argv], str(workdir), str(log_path),
                                 interval)

    def start_job(self, name):
        self.require_jobs()
        return self._start_job(name)

    def stop_job(self, name):
        self.require_jobs()
        return self._stop_job(name)

    def restart_job(self, name):
        """True when a loaded job was restarted, False when none is loaded."""
        self.require_jobs()
        return self._restart_job(name)


class MacHost(Host):
    notifier = "/usr/bin/osascript"
    job_tools = ("launchctl",)

    def _notify_argv(self, title, body):
        return [self.notifier, "-e", f'display notification "{body}" with title "ralph: {title}"']

    def _domain(self, name=""):
        return f"gui/{os.getuid()}" + (f"/{name}" if name else "")

    def job_file(self, name):
        return self.home / "Library" / "LaunchAgents" / f"{name}.plist"

    def _job_running(self, name):
        r = self._run(["launchctl", "print", self._domain(name)], capture_output=True, text=True)
        return "state = running" in r.stdout

    def _install_job(self, name, argv, workdir, log_path, interval):
        plist = self.job_file(name)
        args = "\n".join(f"    <string>{a}</string>" for a in argv)
        schedule = (f"  <key>StartInterval</key><integer>{interval}</integer>\n"
                    if interval else "  <key>RunAtLoad</key><true/>\n")
        plist.parent.mkdir(parents=True, exist_ok=True)
        plist.write_text(f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{name}</string>
  <key>ProgramArguments</key><array>
{args}
  </array>
  <key>WorkingDirectory</key><string>{workdir}</string>
{schedule}  <key>EnvironmentVariables</key><dict>
    <key>HOME</key><string>{self.home}</string>
    <key>PATH</key><string>{os.environ.get("PATH", "")}</string>
  </dict>
  <key>StandardOutPath</key><string>{log_path}</string>
  <key>StandardErrorPath</key><string>{log_path}</string>
</dict></plist>
""")
        return plist

    def _start_job(self, name):
        self._stop_job(name)
        r = self._run(["launchctl", "bootstrap", self._domain(), str(self.job_file(name))],
                      capture_output=True, text=True)
        if r.returncode != 0:
            raise HostError(f"bootstrap failed: {(r.stderr or r.stdout).strip()}")

    def _stop_job(self, name):
        self._run(["launchctl", "bootout", self._domain(name)], capture_output=True, text=True)

    def _restart_job(self, name):
        r = self._run(["launchctl", "print", self._domain(name)], capture_output=True, text=True)
        if r.returncode != 0:
            return False
        self._run(["launchctl", "kickstart", "-k", self._domain(name)], capture_output=True)
        return True


class LinuxHost(Host):
    """notify-send and a transient `systemd-run --user` unit. `install_job`
    records the command (as the plist does on macOS); `start_job` runs it."""
    notifier = "notify-send"
    job_tools = ("systemd-run", "systemctl")

    def _notify_argv(self, title, body):
        return [self.notifier, f"ralph: {title}", body]

    def job_file(self, name):
        return self.home / ".config" / "ralph" / "jobs" / f"{name}.json"

    def _units(self, name):
        return [f"{name}.service", f"{name}.timer"]

    def _job_running(self, name):
        return any(self._run(["systemctl", "--user", "is-active", "--quiet", unit],
                             capture_output=True, text=True).returncode == 0
                   for unit in self._units(name))

    def _install_job(self, name, argv, workdir, log_path, interval):
        import json
        spec = self.job_file(name)
        spec.parent.mkdir(parents=True, exist_ok=True)
        spec.write_text(json.dumps({"argv": argv, "workdir": workdir, "log": log_path,
                                    "interval": interval}, indent=2) + "\n")
        return spec

    def _start_job(self, name):
        import json
        job = json.loads(self.job_file(name).read_text())
        self._stop_job(name)
        timer = ([f"--on-active=1", f"--on-unit-active={job['interval']}"]
                 if job["interval"] else [])
        r = self._run(["systemd-run", "--user", "--unit", name, "--collect",
                       f"--working-directory={job['workdir']}",
                       f"--setenv=PATH={os.environ.get('PATH', '')}",
                       "-p", f"StandardOutput=append:{job['log']}",
                       "-p", f"StandardError=append:{job['log']}", *timer, *job["argv"]],
                      capture_output=True, text=True)
        if r.returncode != 0:
            raise HostError(f"systemd-run failed: {(r.stderr or r.stdout).strip()}")

    def _stop_job(self, name):
        self._run(["systemctl", "--user", "stop", *self._units(name)],
                  capture_output=True, text=True)

    def _restart_job(self, name):
        if not self._job_running(name):
            return False
        self._start_job(name)
        return True


def host_for(platform=None, **kwargs):
    platform = platform or sys.platform
    if platform == "darwin":
        return MacHost(**kwargs)
    if platform.startswith("linux"):
        return LinuxHost(**kwargs)
    return Host(**kwargs)


_HOST = None


def host():
    global _HOST
    if _HOST is None:
        _HOST = host_for()
    return _HOST


def notify(title: str, body: str, enabled: bool = True) -> None:
    """Titles carry the tier so a popup says who must act (2026-09-17):
    "OPERATOR — …" a human act is required; "auto — …" the loop is handling it
    (a director dispatch, a retry); "DONE" / "stopped" are terminal."""
    if enabled:
        host().notify(title, body)


def file_hash(path) -> str:
    p = pathlib.Path(path)
    if not p.exists():
        return ""
    return hashlib.sha256(p.read_bytes()).hexdigest()


def head_of(workdir) -> str:
    r = subprocess.run(["git", "-C", str(workdir), "rev-parse", "HEAD"],
                       capture_output=True, text=True)
    return r.stdout.strip()


def commit_state(paths, subject):
    """The state file alone, as ralph-mark.sh commits it: whatever else is
    staged in the checkout belongs to someone's unit."""
    git = ["git", "-C", str(paths.workdir)]
    subprocess.run([*git, "add", "--", paths.state], capture_output=True, text=True)
    return subprocess.run([*git, "commit", "-q", "-m", subject, "--", paths.state],
                          capture_output=True, text=True)


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
        notifier("OPERATOR — halt unwritable", reason, notify_enabled)
    paths.p(paths.stop).write_text(f"halt: {reason}\n")
    say(f"HALT: {reason}")
    notifier("auto — halted, director next", reason, notify_enabled)
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


def waiting_marker(root, waiting_rel):
    """The waiting file under `root` and the marker it names, or None when
    the file is absent or names no `*.done` marker (ignored and unlinked —
    the convention wait_for_marker has always enforced). One parse for the
    main-tree control file and for a lane worktree's `ralph/waiting`."""
    waiting = pathlib.Path(root) / waiting_rel
    if not waiting.exists():
        return None
    m = re.search(r"[A-Za-z0-9._/-]+\.done", waiting.read_text())
    if not m:
        say(f"{waiting_rel} names no *.done marker — ignoring it")
        waiting.unlink()
        return None
    return waiting, pathlib.Path(root) / m.group(0)


def wait_for_marker(paths, marker_timeout):
    """None to proceed, "wait" to yield this tick, or a reason string to halt."""
    parsed = waiting_marker(paths.workdir, paths.waiting)
    if parsed is None:
        return None
    waiting, marker = parsed
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
            f"   Campaign logs are under `~/.svrnmesh/ralph/` and `{paths.log_dir}/`.\n"
            "2. Apply the smallest change that makes the campaign flow: correct the row or\n"
            "   the code, with its source order corrected together when a premise was false.\n"
            "3. Record the decision: `scripts/ralph-decisions.py new <campaign>` prints an\n"
            "   entry file — fill it (date, unit, fork, choice, evidence, what would\n"
            "   falsify it; tag `REVIEW-AFTER:` when the charter did not clearly cover it),\n"
            "   run `scripts/ralph-decisions.py --write`, and land both with the change.\n"
            "   Never append to `ralph/DECISIONS.md` itself: it is generated.\n"
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
        f"   under `~/.svrnmesh/ralph/` and `{paths.log_dir}/`.\n"
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
    review: bool = False


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
            rows.append(Row(row_id, Status(m.group("mark")), deps, commit, n, line,
                            review=bool(REVIEW_TAG_RE.search(m.group("rest")))))
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

    def insert_before(self, row_id, line):
        """Add a row above another; the file is rewritten as set_status does it."""
        row = self.by_id()[row_id]
        lines = self.path.read_text().splitlines()
        if lines[row.lineno - 1] != row.line:
            raise ValueError(f"{self.path}:{row.lineno}: row moved under insert_before")
        lines.insert(row.lineno - 1, line)
        self.path.write_text("\n".join(lines) + "\n")
        self.rows = self._parse()

    def units_since_audit(self):
        """`[x]` rows below the last `[x]` audit row. An audit still open has
        reviewed nothing, and a HUMAN- row is the operator's act, not a unit."""
        n = 0
        for r in self.rows:
            if r.status is Status.DONE and not r.id.startswith("HUMAN-"):
                n = 0 if r.id.startswith(AUDIT_PREFIX) else n + 1
        return n

    def all_done(self):
        return all(r.status is Status.DONE for r in self.rows)

    def first_ready_review(self):
        for r in self.rows:
            if (r.status in (Status.PENDING, Status.ACTIVE)
                    and r.id.startswith("REVIEW-") and self.deps_met(r)):
                return r
        return None

    def pick_wave(self, lanes, conflicts, heavy=frozenset(), waiting=frozenset()):
        """Ready non-review units, up to `lanes`, no conflicting pair.
        `[~]` rows are resumable lanes: a killed session leaves one behind.
        At most ONE heavy row (ralph/heavy.txt, loaded by the Pool) per wave —
        the big moves run sequentially (2026-09-17, operator direction: derisk
        first); the other lane takes non-heavy rows. A `waiting` unit's lane
        sits on `ralph/waiting` for a detached run — the Pool respawns it when
        the marker lands, not as wave filler."""
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
            if r.id in waiting:
                continue
            if not self.deps_met(r):
                continue
            if r.id in heavy and any(w in heavy for w in wave):
                continue
            if any(frozenset((r.id, w)) in conflicts for w in wave):
                continue
            wave.append(r.id)
        return wave


AUDIT_PREFIX = "REVIEW-audit-"
# The one text of a cadence audit (`audit_every` in queue.toml; Campaign inserts
# the row). A queue's hand-written audit rows stay its author's.
AUDIT_ROW_BODY = (
    "audit the commits since the previous `REVIEW-audit-` row's hash (since this queue "
    "started if there is none) against `sovereign/ARCH_PRINCIPLES.md`; REUSE AND SIZE, WITH "
    "DATA: (1) paste a per-unit net-line ledger for product code over that range — "
    "`git log --numstat --format=%s <hash>..HEAD`, summed by the subject's unit id, src "
    "apart from tests; (2) run `target/debug/sovereign-cli code dry-report --scope <dir>` "
    "for each crate dir the range touched and paste every exact or near clone with a side "
    "that is a symbol ADDED in the range; (3) for every new `struct`/`enum`/`trait` in the "
    "range, `target/debug/sovereign-cli code converge noun <Name>`, and paste any noun "
    "with more than one definition; fix here what is behaviour-preserving and small, "
    "record the rest in `ralph/REVIEW_FINDINGS.md` with both file:line sites "
    "— read: `sovereign/ARCH_PRINCIPLES.md` — check: LINT")


def audit_row(row_id, after):
    return f"- [ ] {row_id} — depends [{after}] — {AUDIT_ROW_BODY}"


MODEL_KEYS = ("MODEL", "REVIEW_MODEL", "RESOLVE_MODEL", "VARIANT")
WAIT_LIMIT_KEY = "WAIT_LIMIT_S"
# Raised from 7200 (2h) on order ralph-model-roster: full gates legitimately
# exceed 2h on this box and every such wait was a false "never wrote its
# marker" halt.
DEFAULT_WAIT_LIMIT_S = 24 * 3600


def load_models(path):
    """Strict KEY=value data; unknown keys and comments are ignored. The
    model values stay raw (a roster is a comma-joined string until
    `parse_roster` splits it) so every consumer sees one format."""
    out = {}
    p = pathlib.Path(path)
    if not p.exists():
        return out
    for line in p.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        if key.strip() in MODEL_KEYS + (WAIT_LIMIT_KEY,):
            out[key.strip()] = value.strip()
    return out


def resolve_wait_limit(args, paths):
    """The one detached-wait decider: an explicit `--marker-timeout`, then
    WAIT_LIMIT_S in models.env, then DEFAULT_WAIT_LIMIT_S. A bad value is
    refused by name, never silently defaulted."""
    given = getattr(args, "marker_timeout", None)
    if given is not None:
        if given <= 0:
            raise ValueError(f"--marker-timeout={given} must be > 0")
        return given
    raw = load_models(paths.p(paths.models)).get(WAIT_LIMIT_KEY, "")
    if not raw:
        return DEFAULT_WAIT_LIMIT_S
    try:
        limit = int(raw)
    except ValueError:
        raise ValueError(f"{paths.models}: WAIT_LIMIT_S={raw!r} is not an integer") from None
    if limit <= 0:
        raise ValueError(f"{paths.models}: WAIT_LIMIT_S={limit} must be > 0")
    return limit


def parse_roster(value):
    """MODEL/REVIEW_MODEL roster semantics: `a,b,c` in declared order, blank
    segments dropped. A single value is a one-model roster — probed the same
    (the probe replaces the wave-failure discovery)."""
    return [m.strip() for m in (value or "").split(",") if m.strip()]


# The transcript line shapes a halt should carry: quota / permission path /
# provider error / crash tail (order ralph-model-roster). One shape list, used
# by both the probe's cause-keeping and the strikeout halt's tail.
ERROR_SHAPE_RE = re.compile(
    r"(?i)\b(error|failed|failure|fatal|panic|refused|denied|timeout|timed out"
    r"|quota|usage limit|rate.?limit|unauthori[sz]ed|forbidden|not found"
    r"|invalid api key|unexpected server)\b")

PROBE_TIMEOUT_S = 45
PROBE_PROMPT = "Reply with the single word OK."


def error_tail(text, limit=200):
    """The LAST error-shaped line of a transcript, truncated to one line a
    director reads; empty when none matches."""
    for line in reversed((text or "").splitlines()):
        if line.strip() and ERROR_SHAPE_RE.search(line):
            return line.strip()[:limit]
    return ""


def halt_tail_suffix(log_path):
    """` — last error: <line>` from a lane/review transcript, so the halt
    text alone is diagnostic; empty when the transcript holds no
    error-shaped line."""
    try:
        tail = error_tail(pathlib.Path(log_path).read_text(errors="replace"))
    except OSError:
        return ""
    return f" — last error: {tail}" if tail else ""


def _probe_configs(paths):
    """The opencode configs the probe consults for provider routing: the
    workdir's, then the user's. An unparseable file is skipped — the probe
    then falls back to the client itself."""
    out = []
    for file in (paths.workdir / ".opencode" / "opencode.json",
                 pathlib.Path.home() / ".config" / "opencode" / "opencode.json"):
        try:
            out.append(json.loads(file.read_text()))
        except (OSError, ValueError):
            continue
    return out


def probe_refusal(model, paths):
    """The probe never runs against the mesh daemon: an entry whose provider
    is declared with a loopback baseURL (localhost:9741 — the mesh daemon
    serves inference at /v1) is refused by name, and a bare id names no
    provider at all (order ralph-model-roster seam: probe the provider, not
    localhost). Returns "" to probe, else the cause."""
    provider, _, name = model.partition("/")
    if not provider or not name:
        return "names no provider/model pair — not probed"
    for cfg in _probe_configs(paths):
        declared = (cfg.get("provider") or {}).get(provider) or {}
        base = (declared.get("options") or {}).get("baseURL") or ""
        host = urllib.parse.urlparse(base).hostname or ""
        if host in ("localhost", "127.0.0.1", "::1"):
            return f"routes at {base} (the mesh daemon) — the probe never targets localhost"
    return ""


def probe_model(model, paths, timeout=PROBE_TIMEOUT_S):
    """One minimal chat call per model through the same client lanes use
    (`<worker_bin> run --model M`), provider-direct. Returns (True, "") when
    the model answers, else (False, cause kept from the error: quota reset
    text, provider error, timeout)."""
    refusal = probe_refusal(model, paths)
    if refusal:
        return False, f"{model}: {refusal}"
    client = worker_bin(paths)
    try:
        r = subprocess.run([client, "run", "--model", model, PROBE_PROMPT],
                           cwd=str(paths.workdir), capture_output=True, text=True,
                           timeout=timeout)
    except subprocess.TimeoutExpired:
        return False, f"timeout after {timeout}s"
    except OSError as e:
        return False, f"{client}: {e}"
    tail = error_tail((r.stdout or "") + (r.stderr or ""))
    if r.returncode == 0 and not tail:
        return True, ""
    return False, tail or f"exit {r.returncode}"


REVIEW_TAG_RE = re.compile(r"(?:^|—)\s*review\s*=\s*true\s*(?=—|$)")


def is_review(row):
    """A review row is the `REVIEW-` prefix or a `— review = true —` field on
    the row. It was the substring "review" anywhere in the id, which would send
    a row named for a review FEATURE to the stronger model."""
    if isinstance(row, str):
        return row.startswith("REVIEW-")
    return row.id.startswith("REVIEW-") or row.review


def select_model_args(row, model, review_model, variant):
    chosen = review_model if (review_model and is_review(row)) else model
    args = []
    if chosen:
        args += ["--model", chosen]
    if variant:
        args += ["--variant", variant]
    return args


# The checks ralph-check.sh implements itself. A manifest may not redeclare one:
# one name, one decider.
BUILTIN_CHECKS = ("clean", "lint", "test", "testfn", "layer", "env", "docs",
                  "testall", "prepush")
MANIFEST_MODEL_KEYS = {"worker": "MODEL", "review": "REVIEW_MODEL",
                       "resolve": "RESOLVE_MODEL", "variant": "VARIANT"}
MANIFEST_PATH_KEYS = {"state": "STATE.md", "prompt": "PROMPT.md", "charter": "CHARTER.md",
                      "conflicts": "conflicts.txt", "heavy": "heavy.txt", "control_dir": "ctl"}


@dataclasses.dataclass(frozen=True)
class QueueManifest:
    """`ralph/next/<name>/queue.toml`: everything one queue owns, as data.
    Named apart from `Queue`, which is the row grammar of its STATE.md."""
    name: str
    path: str
    label: str
    state: str
    prompt: str
    charter: str
    conflicts: str
    heavy: str
    control_dir: str
    models: dict
    checks: dict
    session_timeout: int | None = None
    worker_bin: str = ""
    settings: str = ""
    prompt_declared: bool = False
    audit_every: int | None = None        # absent: no cadence, the queue's own audit rows only


def manifest_rel(name):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", name or ""):
        raise ValueError(f"queue name {name!r} must be one path segment under {STAGED_DIR}/")
    return f"{STAGED_DIR}/{name}/queue.toml"


def load_manifest(workdir, name):
    """Strict: an unknown key, a mistyped value or a missing file is refused by
    name. A typo that silently fell back to a default is how a queue ends up
    running another queue's files."""
    import tomllib          # 3.11+; imported here so legacy launch lines need no manifest support
    rel = manifest_rel(name)
    file = pathlib.Path(workdir) / rel
    if not file.is_file():
        raise ValueError(f"no queue manifest at {rel} (workdir {workdir})")
    try:
        data = tomllib.loads(file.read_text())
    except tomllib.TOMLDecodeError as e:
        raise ValueError(f"{rel}: {e}") from e

    def bad(key, want):
        return ValueError(f"{rel}: `{key}` must be {want}")

    known = {"label", "session_timeout", "audit_every", "worker_bin", "settings", "models",
             "checks", *MANIFEST_PATH_KEYS}
    for key in data:
        if key not in known:
            raise ValueError(f"{rel}: unknown key `{key}` (known: {', '.join(sorted(known))})")
    strings = {}
    for key in ("label", "worker_bin", "settings", *MANIFEST_PATH_KEYS):
        if key in data and not isinstance(data[key], str):
            raise bad(key, "a string")
        strings[key] = data.get(key, "")
    timeout = data.get("session_timeout")
    if timeout is not None and (isinstance(timeout, bool) or not isinstance(timeout, int)
                                or timeout <= 0):
        raise bad("session_timeout", "a positive integer of seconds")
    every = data.get("audit_every")
    if every is not None and (isinstance(every, bool) or not isinstance(every, int) or every < 2):
        raise bad("audit_every", "an integer >= 2 (units between audits)")
    models = {}
    table = data.get("models", {})
    if not isinstance(table, dict):
        raise bad("models", "a table")
    for key, value in table.items():
        if key not in MANIFEST_MODEL_KEYS:
            raise ValueError(f"{rel}: unknown [models] key `{key}` "
                             f"(known: {', '.join(MANIFEST_MODEL_KEYS)})")
        if not isinstance(value, str):
            raise bad(f"models.{key}", "a string")
        models[MANIFEST_MODEL_KEYS[key]] = value
    checks = {}
    table = data.get("checks", {})
    if not isinstance(table, dict):
        raise bad("checks", "a table")
    for key, argv in table.items():
        if key in BUILTIN_CHECKS:
            raise ValueError(f"{rel}: [checks] `{key}` is a ralph-check.sh built-in "
                             "and cannot be redeclared")
        if (not isinstance(argv, list) or not argv
                or not all(isinstance(a, str) and a and "\n" not in a for a in argv)):
            raise bad(f"checks.{key}", "a non-empty argv list of one-line strings")
        checks[key] = tuple(argv)
    base = f"{STAGED_DIR}/{name}"
    return QueueManifest(
        name=name, path=rel, label=strings["label"] or name,
        models=models, checks=checks, session_timeout=timeout,
        worker_bin=strings["worker_bin"], settings=strings["settings"],
        prompt_declared="prompt" in data, audit_every=every,
        **{key: strings[key] or f"{base}/{default}"
           for key, default in MANIFEST_PATH_KEYS.items()})


MODEL_FLAGS = {"MODEL": "model", "REVIEW_MODEL": "review_model",
               "RESOLVE_MODEL": "resolve_model", "VARIANT": "variant"}


def resolve_models(args, paths):
    """The one model decider: the queue's `[models]`, then the flag, then the
    per-checkout models.env. A flag the manifest overrides is named, not dropped."""
    shared = load_models(paths.p(paths.models))
    declared = paths.manifest.models if paths.manifest else {}
    out = {}
    for key, flag in MODEL_FLAGS.items():
        given = getattr(args, flag, "") or ""
        if declared.get(key):
            if given and given != declared[key]:
                say(f"--queue {paths.queue} wins: ignoring --{flag.replace('_', '-')} {given} "
                    f"({paths.manifest.path} [models] names {declared[key]})")
            out[key] = declared[key]
        else:
            out[key] = given or shared.get(key, "")
    return out


def write_manifest_models(file, updates):
    """Set `[models]` keys in a queue.toml by line, leaving every other line as
    written (tomllib reads only; a re-serialise would drop the comments). The
    result must load, or the file is put back."""
    import json
    import tomllib
    original = file.read_text()
    lines = original.splitlines()
    header = next((i for i, l in enumerate(lines) if l.strip() == "[models]"), None)
    if header is None:
        if lines and lines[-1].strip():
            lines.append("")
        lines.append("[models]")
        header = len(lines) - 1
    end = next((i for i in range(header + 1, len(lines)) if lines[i].lstrip().startswith("[")),
               len(lines))
    while end > header + 1 and not lines[end - 1].strip():
        end -= 1                      # new keys go above the blank lines that close the table
    for key, value in updates.items():
        entry = f"{key} = {json.dumps(value, ensure_ascii=False)}"
        at = next((i for i in range(header + 1, end)
                   if re.match(rf"\s*{re.escape(key)}\s*=", lines[i])), None)
        if at is None:
            lines.insert(end, entry)
            end += 1
        else:
            lines[at] = entry
    file.write_text("\n".join(lines) + "\n")
    try:
        tomllib.loads(file.read_text())
    except tomllib.TOMLDecodeError as e:
        file.write_text(original)
        raise ValueError(f"{file}: the [models] rewrite did not parse ({e}); file restored") from e


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
    charter: str = "ralph/CHARTER.md"
    conflicts: str = "ralph/conflicts.txt"
    heavy: str = "ralph/heavy.txt"
    queue: str = ""                       # the --queue name; "" on a legacy launch line
    control_dir: str = "ralph"            # a queue's own: ralph/next/<name>/ctl
    director_commits: str = "ralph/.director-commits"
    log_dir: str = "target/ralph"
    prompt_addendum: str = ""             # set when the prompt is rendered, not read
    manifest: QueueManifest | None = None

    def p(self, rel):
        return self.workdir / rel

    @staticmethod
    def control_files(control_dir):
        """The per-loop files, as Paths fields. One loop per control_dir."""
        return {"control_dir": control_dir, "done": f"{control_dir}/DONE",
                "stop": f"{control_dir}/STOP", "needs_human": f"{control_dir}/NEEDS_HUMAN.md",
                "waiting": f"{control_dir}/waiting", "heartbeat": f"{control_dir}/.heartbeat",
                "director_commits": f"{control_dir}/.director-commits"}


PROMPT_BASE = "ralph/PROMPT.base.md"
SECTION_RE = re.compile(r"^<!-- section: (?P<name>[a-z0-9-]+)"
                        r"(?: (?P<mode>append|after=(?P<after>[a-z0-9-]+)))? -->\n", re.M)
VAR_RE = re.compile(r"\{\{([a-z_]+)\}\}")


def prompt_sections(text, source):
    """`<!-- section: name [append|after=<name>] -->` lines split a prompt file;
    the marker lines, and anything above the first one, are not rendered."""
    marks = list(SECTION_RE.finditer(text))
    out = []
    for n, m in enumerate(marks):
        body = text[m.end():marks[n + 1].start() if n + 1 < len(marks) else len(text)]
        if any(name == m["name"] for name, _, _, _ in out):
            raise ValueError(f"{source}: section `{m['name']}` appears twice")
        out.append((m["name"], m["mode"] or "", m["after"], body))
    return out


def section_vars(body):
    """The `key = value` lines of a `vars` section."""
    return dict([part.strip() for part in line.split("=", 1)] for line in body.splitlines()
                if "=" in line and not line.lstrip().startswith("#"))


def render_prompt(base, addendum, builtins):
    """The shared prompt once, a queue's differences beside it (until 2026-09-19
    every queue carried a sed-edited copy, and ei7-stage0's copy kept a
    ralph-mark.sh call that named another queue's file). An addendum section
    replaces the base section of its name, `append` adds to it, `after=<name>`
    places a new one, and any other new name goes last; `vars` is `key = value`
    lines for {{key}}. Whatever cannot be rendered is refused by name."""
    order, bodies = [], {}
    for name, mode, _, body in prompt_sections(base, "base"):
        if mode or name == "vars":
            raise ValueError(f"base: section `{name}` may not be `vars` or carry a mode")
        order.append(name)
        bodies[name] = body
    variables = dict(builtins)
    for name, mode, after, body in prompt_sections(addendum, "addendum"):
        if name == "vars":
            for key, value in section_vars(body).items():
                if key in builtins:
                    raise ValueError(f"addendum: var `{key}` is the loop's own, not the queue's")
                variables[key] = value
        elif mode == "append":
            if name not in bodies:
                raise ValueError(f"addendum: `{name} append` — the base has no section `{name}`")
            bodies[name] += body
        elif after:
            if after not in bodies or name in bodies:
                raise ValueError(f"addendum: `{name} after={after}` — `{after}` must exist "
                                 f"and `{name}` must be new")
            order.insert(order.index(after) + 1, name)
            bodies[name] = body
        else:
            if name not in bodies:
                order.append(name)
            bodies[name] = body

    def fill(m):
        if m[1] not in variables:
            raise ValueError(f"prompt: {{{{{m[1]}}}}} is not a var "
                             f"(known: {', '.join(sorted(variables))})")
        return variables[m[1]]

    return VAR_RE.sub(fill, "".join(bodies[name] for name in order))


def prompt_text(paths):
    """(text, source) for the worker prompt: rendered when the queue has an
    addendum and declares no `prompt` of its own, else the file as written."""
    if paths.prompt_addendum:
        text = render_prompt(paths.p(PROMPT_BASE).read_text(),
                             paths.p(paths.prompt_addendum).read_text(),
                             {"queue": paths.queue, "state": paths.state,
                              "control_dir": paths.control_dir, "log_dir": paths.log_dir})
        return text, f"{PROMPT_BASE} + {paths.prompt_addendum}"
    return paths.p(paths.prompt).read_text(), paths.prompt


# A driver heartbeat younger than this means a loop is live. One number for the
# watchdog's "stalled" and promote's "still running".
STALL_SECS = 300

# A pool lane may wait on a detached run's marker for this long from the moment
# its session first banked `ralph/waiting` (the file's mtime) before the pool
# escalates with a package. The serial flow's bound is `--marker-timeout`
# (7200s); a lane's detached run is expected to outlive many sessions
# (r9-boundary-sweep's full-density sweep ran ~23h), so its bound is its own.
LANE_MAX_WAIT_SECS = 48 * 3600


def job_name(paths, label):
    return f"dev.ralph.{paths.workdir.name}-{label}"


def job_running(paths, label):
    return host().job_running(job_name(paths, label))


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


def session_env(paths):
    """What every worker session is told about the loop that spawned it, so
    ralph-mark.sh and ralph-check.sh need no per-campaign default. RALPH_QUEUE
    is set even when empty: a legacy loop launched from inside a queue's
    session must not inherit that queue."""
    env = {"RALPH_QUEUE": paths.queue, "RALPH_STATE": paths.state,
           "RALPH_CONTROL_DIR": paths.control_dir}
    if paths.manifest and paths.manifest.settings:
        env["RALPH_CLAUDE_SETTINGS"] = str(paths.p(paths.manifest.settings))
    return env


def worker_bin(paths):
    """The manifest's worker_bin (a path is relative to the workdir), then
    RALPH_OPENCODE_BIN, then opencode."""
    declared = paths.manifest.worker_bin if paths.manifest else ""
    if declared:
        return str(paths.p(declared)) if "/" in declared else declared
    return os.environ.get("RALPH_OPENCODE_BIN", "opencode")


class Session:
    """One opencode session in its own process group, with a wall-clock
    timeout, a STOP check, a heartbeat, and permission-reject detection."""

    def __init__(self, paths, *, timeout=3600, opencode=None, poll=30,
                 notifier=notify, notify_enabled=True, cwd=None, env=None):
        self.paths = paths
        self.timeout = timeout
        self.opencode = opencode or worker_bin(paths)
        self.poll = poll
        self.notifier = notifier
        self.notify_enabled = notify_enabled
        self.cwd = pathlib.Path(cwd) if cwd else paths.workdir
        self.env = env or {}

    def heartbeat(self, context):
        try:
            self.paths.p(self.paths.control_dir).mkdir(parents=True, exist_ok=True)
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
                cwd=workdir, env={**os.environ, **session_env(self.paths), **self.env},
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
            self.notifier("auto — session timeout", f"killed at {self.timeout}s", self.notify_enabled)
            self._kill(proc)
        rc = proc.wait()
        self.heartbeat(f"session-end {self.paths.workdir.name}")
        try:
            rejects = log.read_text(errors="replace").count("auto-rejecting")
        except OSError:
            rejects = 0
        if rejects:
            say(f"WARNING: {rejects} permission auto-rejections — extend opencode.json")
            self.notifier("auto — permission rejects", f"{rejects} auto-rejections", self.notify_enabled)
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
                 sleep=time.sleep, max_stall=3, max_iter=200,
                 marker_timeout=DEFAULT_WAIT_LIMIT_S,
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
            queue = Queue(self.paths.p(self.paths.state))
            unit = queue.current()
            if unit is None:
                return self.halt(f"no ready unit in {self.paths.state}; check dependencies")
            if unit.id.startswith("HUMAN-"):
                return self.halt(f"operator approval required: {unit.id}")
            if self._audit_due(queue, unit):
                unit, refused = self._insert_audit(queue, unit)
                if refused:
                    return self.halt(f"audit row {unit.id} is in {self.paths.state} "
                                     f"but git refused the commit: {refused}")
            model_args = select_model_args(unit, self.model, self.review_model, self.variant)
            say(f"unit {unit.id} — {' '.join(model_args) or 'configured default'}")
            before = head_of(self.paths.workdir)
            note = (f"Your unit: {unit.id} — its row in {self.paths.state} is the [~] row, "
                    "or the first ready [ ] row. Open only that row; do not scan the "
                    "queue for another.\n\n")
            if self.paths.queue:
                # Another loop may own ralph/STOP and ralph/NEEDS_HUMAN.md in this checkout.
                note += (f"This queue's control files are {self.paths.needs_human}, "
                         f"{self.paths.done} and {self.paths.waiting} — never the files of "
                         "those names directly under ralph/, which belong to another loop.\n\n")
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

    def _audit_due(self, queue, unit):
        """`audit_every`, enforced where rows are dispatched: a 22-row queue ran
        ~60 commits on one closing audit because a cadence was its author's to
        remember. A [~] row is finished first — `current()` returns it until it
        closes, so a row inserted above it would be inserted again every pass."""
        every = self.paths.manifest.audit_every if self.paths.manifest else None
        return bool(every and unit.status is Status.PENDING
                    and not unit.id.startswith(AUDIT_PREFIX)
                    and queue.units_since_audit() >= every)

    def _insert_audit(self, queue, unit):
        """(the inserted row, git's refusal or ""). The id's <prefix> is the
        addendum's `prefix` var, else the queue's name."""
        prefix = self.paths.queue
        if self.paths.prompt_addendum:
            for name, _, _, body in prompt_sections(
                    self.paths.p(self.paths.prompt_addendum).read_text(), "addendum"):
                if name == "vars":
                    prefix = section_vars(body).get("prefix", prefix)
        if not re.fullmatch(r"[A-Za-z0-9._-]+", prefix):
            say(f"prefix var {prefix!r} cannot be part of a row id; using the queue name")
            prefix = self.paths.queue
        stem = f"{AUDIT_PREFIX}{prefix}-auto-"
        row_id = f"{stem}{1 + sum(r.id.startswith(stem) for r in queue.rows)}"
        n = queue.units_since_audit()
        last_done = [r.id for r in queue.rows if r.status is Status.DONE][-1]
        queue.insert_before(unit.id, audit_row(row_id, last_done))
        subject = f"ralph: audit due after {n} units — {row_id}"
        say(subject)
        r = commit_state(self.paths, subject)
        refused = "" if r.returncode == 0 else (r.stderr.strip() or f"exit {r.returncode}")
        return Queue(queue.path).by_id()[row_id], refused

    def _beat(self, context):
        try:
            self.paths.p(self.paths.control_dir).mkdir(parents=True, exist_ok=True)
            self.paths.p(self.paths.heartbeat).write_text(f"{int(time.time())} {context}\n")
        except OSError:
            pass

    def _prompt_text(self):
        """Re-read every iteration, as before; the hash is logged when it changes,
        so the log says which prompt each unit ran on."""
        text, source = prompt_text(self.paths)
        digest = hashlib.sha256(text.encode()).hexdigest()[:16]
        if digest != getattr(self, "_prompt_digest", None):
            self._prompt_digest = digest
            say(f"prompt: {source} sha256={digest}")
        return text

    def _log_path(self, iteration):
        return str(self.paths.p(self.paths.log_dir) / f"iter-{iteration}.out")


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
        log = self.paths.p(self.paths.director_commits)
        try:
            log.parent.mkdir(parents=True, exist_ok=True)
            with log.open("a") as fh:
                fh.write(f"{int(time.time())} attempt={attempt} {before}..{after} — {reason}\n")
        except OSError:
            pass

    def terminal_stop(self):
        if self.paths.p(self.paths.done).exists():
            say("supervisor: campaign DONE")
            self.notifier("DONE — campaign complete", "every row is [x]", self.notify_enabled)
            return 0
        stop = self.paths.p(self.paths.stop)
        pkg = self.paths.p(self.paths.needs_human)
        # An EMPTY STOP is the operator's and wins over any package beside it:
        # requiring the absence of a package made a stop unhonored, which kept
        # the supervisor dispatching resolutions (2026-09-16).
        if stop.exists() and not stop.stat().st_size:
            say("supervisor: operator STOP — leaving it stopped")
            self.notifier("stopped — operator stop preserved", self.paths.stop, self.notify_enabled)
            return 0
        if stop.exists() and stop.stat().st_size and not (pkg.exists() and pkg.stat().st_size):
            pkg.write_text(f"{stop.read_text()}\nresolve by hand, then remove "
                           f"{self.paths.stop} {self.paths.needs_human}\n")
            say(f"supervisor: halt package was missing — wrote one from {self.paths.stop}")
        queue = self._queue()
        unit = queue.current() if queue else None
        if unit is not None and unit.id.startswith("HUMAN-"):
            say(f"supervisor: operator approval required — {unit.id} (no resolution session)")
            self.notifier("OPERATOR — approval required", f"{unit.id} is a HUMAN row at the head; approve or mark it", self.notify_enabled)
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
                self.notifier("OPERATOR — unresolved after "
                              f"{self.resolve_max} resolutions", reason,
                              self.notify_enabled)
                return 2
            pkg_before = file_hash(pkg)
            head_before = head_of(self.paths.workdir)
            stop_file = self.paths.p(self.paths.stop)
            if pkg.exists() and pkg.stat().st_size:
                stop_file.unlink(missing_ok=True)
            say(f"supervisor: dispatching resolution session {attempt} — {reason}")
            self.notifier("auto — resolving", f"attempt {attempt}: {reason}", self.notify_enabled)
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
                    self.notifier("OPERATOR — resolution achieved nothing", reason,
                                  self.notify_enabled)
                    return 2
                say(f"supervisor: resolution {attempt} left NEEDS_HUMAN — retrying")
            else:
                say(f"supervisor: resolution {attempt} cleared the halt — resuming the campaign")


def conflict_pairs(text):
    """A line of N ids means all N-choose-2 pairs; `#` starts a comment. Reading
    only the first two dropped the third id of ring-doc's line without a word."""
    pairs = set()
    for line in text.splitlines():
        ids = line.split("#")[0].split()
        pairs.update(frozenset(pair) for pair in itertools.combinations(ids, 2))
    return pairs


class Pool:
    """The parallel driver: waves of ready units in git worktrees, serial
    merges, a conflict halts (never auto-resolved). REVIEW rows run serially
    in the main tree. Progress is the same file protocol as the serial flow."""

    def __init__(self, paths, *, session_for, notifier=notify, notify_enabled=True,
                 lanes=2, base_branch="", conflicts="ralph/conflicts.txt",
                 prompt="ralph/PROMPT.md", state="ralph/STATE.md",
                 marker_timeout=DEFAULT_WAIT_LIMIT_S, wait_poll=120, sleep=time.sleep,
                 model="", review_model="", variant="", max_review_attempts=3,
                 max_lane_failures=3, probe=None):
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
        self.probe = probe or (lambda model: probe_model(model, paths))
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
        p = self.paths.p(self.conflicts)
        return conflict_pairs(p.read_text()) if p.exists() else set()

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

    def _dispatch_model(self, roster_value):
        """The roster probe at dispatch (order ralph-model-roster): one
        minimal chat call per model through the lane client, in declared
        order; the first healthy model runs the wave/review. Returns (chosen
        or None, park-reason or None). An empty roster is today's behaviour —
        no probe, no stamp."""
        roster = parse_roster(roster_value)
        if not roster:
            return None, None
        causes = {}
        for model in roster:
            ok, cause = self.probe(model)
            if ok:
                say(f"pool: probe {model} — ok")
                return model, None
            causes[model] = cause
            say(f"pool: probe {model} — {cause}")
        return None, ("no healthy model in the roster — "
                      + "; ".join(f"{m}: {c}" for m, c in causes.items()))

    def _prompt_text(self):
        return self.paths.p(self.prompt).read_text()

    def poll_waiting_lanes(self):
        """Each tick, every lane worktree whose `ralph/waiting` names a marker:
        resume the lane whose marker has landed, hold the rest out of the
        waves, escalate past LANE_MAX_WAIT_SECS. Returns (halt reason or None,
        the units still waiting). The filesystem is the state — a restart or a
        previous pool generation loses nothing."""
        still = set()
        wt_root = self.paths.workdir / ".ralph" / "wt"
        if not wt_root.exists():
            return None, still
        for wt in sorted(wt_root.iterdir()):
            parsed = waiting_marker(wt, "ralph/waiting")
            if parsed is None:
                continue
            unit, waiting, marker = wt.name, parsed[0], parsed[1]
            named = marker.relative_to(wt)
            age = int(time.time() - waiting.stat().st_mtime)
            if age >= LANE_MAX_WAIT_SECS:
                return (f"lane {unit} waited {age // 3600}h on {named} "
                        f"(limit {LANE_MAX_WAIT_SECS // 3600}h) — the detached run "
                        "never wrote its marker"), still
            if marker.exists():
                say(f"pool: lane {unit} waiting on {named} — marker present, resuming")
                waiting.unlink()
                # End the waiting ON THE LANE BRANCH, not just on disk: a
                # committed waiting file that survives to the merge parks the
                # main tree's loop on a marker that only ever existed in this
                # worktree. The commit is a no-op when nothing is staged.
                self._git("add", "-A", "--", "ralph/waiting", cwd=wt)
                self._git("commit", "-q", "-m", f"{unit}: waiting ended — marker landed",
                          cwd=wt)
            else:
                say(f"pool: lane {unit} waiting on {named} ({age}s)")
                still.add(unit)
        return None, still

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
                self.notifier("DONE — pool complete", "every row is [x]", self.notify_enabled)
                return 0
            marker = wait_for_marker(self.paths, self.marker_timeout)
            if marker is not None and marker != "wait":
                return self._halt(marker)
            if marker == "wait":
                self.sleep(self.wait_poll)
                continue
            reason, waiting = self.poll_waiting_lanes()
            if reason is not None:
                return self._halt(reason)
            unit = queue.current()
            if unit is not None and unit.id.startswith("HUMAN-"):
                return self._halt(f"operator approval required: {unit.id}")
            review = queue.first_ready_review()
            if review is not None:
                # A review with no REVIEW_MODEL of its own runs on the worker
                # model (select_model_args routing) — probe that roster.
                review_model, park = self._dispatch_model(self.review_model or self.model)
                if park is not None:
                    return self._halt(park)
                result = self.run_review(review, review_model)
                if result is not None:
                    return result
                continue
            wave = queue.pick_wave(self.lanes, self._conflict_pairs(), self._heavy(), waiting)
            if not wave:
                say("pool: no ready unit and no ready review — waiting (dependencies unmet?)")
                self.sleep(60)
                continue
            model, park = self._dispatch_model(self.model)
            if park is not None:
                return self._halt(park)
            result = self.run_wave(wave, model)
            if result is not None:
                return result

    def run_review(self, review, model=None):
        session = self.session_for(self.paths.workdir)
        effective = model if model is not None else self.review_model
        model_args = select_model_args(review.id, self.model, effective, self.variant)
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
            say(f"pool: serial review {review.id} (main tree) attempt {attempt}"
                + (f" · model {effective}" if effective else ""))
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
                self.notifier("auto — halt package, director next", first_line(pkg), self.notify_enabled)
                return 3
            marker = wait_for_marker(self.paths, self.marker_timeout)
            if marker == "wait":
                return None          # handed off to a detached run; resume the loop
            if marker is not None:
                return self._halt(marker)
            say(f"pool: review {review.id} did not mark [x] (attempt {attempt}) — resuming")
        return self._halt(f"review {review.id} did not finish after "
                          f"{self.max_review_attempts} attempts"
                          + halt_tail_suffix(
                              self.paths.workdir / "target" / "ralph"
                              / f"review-{review.id}.out"))

    def _provision_host_pointers(self, wt):
        """Copy the per-host pointer dirs into a lane worktree.

        A row's `read: O8` names `.sovereign/features/<id>/order.md`, which is
        gitignored (`.gitignore:44`), so `git worktree add` never brings it and
        a lane cannot execute its row (dm-vocab-compile-fail-test, 2026-09-17,
        three waves). This is the copy `ralph/STATE.md` tells the operator to
        make for a peer checkout. A COPY, not a symlink: the ignore pattern is
        `.sovereign/features/` (directory-only), so a symlink is NOT ignored
        and the lane's `git add -A` would commit it.
        """
        src = self.paths.workdir / ".sovereign" / "features"
        dst = wt / ".sovereign" / "features"
        if not src.is_dir() or dst.exists():
            return
        try:
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(src, dst, symlinks=True)
        except OSError as e:
            say(f"pool: could not provision {dst} from {src}: {e}")

    def run_lane(self, unit, model=None):
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
            if own.returncode == 0:
                if own.stdout.strip() == "0":
                    ff = self._git("merge", "--ff-only", self.base_branch, cwd=wt)
                    if ff.returncode == 0:
                        say(f"pool: lane {unit} refreshed onto {self.base_branch}")
                    else:
                        say(f"pool: lane {unit} not refreshed: {ff.stderr.strip()}")
                else:
                    # A lane with its own commits was skipped entirely, so it ran
                    # on a stale base and the conflict landed at the POOL's merge
                    # as an escalation (2026-09-18: dm-rename-leaf-words,
                    # dm-next-edit-move). Merge the base in HERE, where the
                    # session can resolve it.
                    m = self._git("merge", "--no-edit", self.base_branch, cwd=wt)
                    if m.returncode == 0:
                        say(f"pool: lane {unit} merged {self.base_branch} in")
                    else:
                        say(f"pool: lane {unit} conflicts with {self.base_branch} — "
                            "the session resolves it")
        self._provision_host_pointers(wt)
        note = (f"POOL LANE: you are working unit {unit} in an isolated git worktree.\n"
                f"Commit your work here. When the unit passes its OWN tests, write "
                f"ralph/lanes/{unit}.done and commit it — the pool merges your branch then.\n"
                "Do NOT edit ralph/STATE.md except to correct your own row's premises "
                "(PROMPT §6); the pool marks the unit done after the merge.\n\n")
        if self._git("rev-parse", "-q", "--verify", "MERGE_HEAD", cwd=wt).returncode == 0:
            note = ("Your worktree has a MERGE IN PROGRESS: the pool merged the base "
                    "branch in and it conflicted. Resolve every conflict, `git add` the "
                    "files, `git commit --no-edit`, then do your unit.\n\n") + note
        model_args = select_model_args(unit, model if model is not None else self.model,
                                       self.review_model, self.variant)
        # One lock per lane: a lane builds in its own worktree/target, so the
        # shared /tmp lock would only serialize lanes against each other and
        # against other campaigns (2026-09-16 speed order).
        lock_dir = f"/tmp/svrn-cargo-lock.{os.getuid()}.lane-{unit}"
        session = self.session_for(wt, env={"SVRN_CARGO_LOCK_DIR": lock_dir})
        session.run(model_args, note + self._prompt_text(),
                    str(self.paths.workdir / "target" / "ralph" / f"lane-{unit}.out"))

    def run_wave(self, wave, model=None):
        # The chosen model is stamped on the wave line: one glance at
        # launchd.log says which provider served the wave (order
        # ralph-model-roster).
        say(f"pool: wave {', '.join(wave)}" + (f" · model {model}" if model else ""))
        with ThreadPoolExecutor(max_workers=len(wave)) as ex:
            list(ex.map(lambda unit: self.run_lane(unit, model), wave))
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
                self.notifier("auto — halt package, director next", first_line(lane_pkg), self.notify_enabled)
                return 3
            if not (wt / "ralph" / "lanes" / f"{unit}.done").exists():
                parsed = waiting_marker(wt, "ralph/waiting")
                if parsed is not None:
                    # A waiting end is the lane's own protocol for a detached
                    # run outliving the session (r9-boundary-sweep, struck out
                    # twice for it and halted ring 9, 2026-09-19): the tick
                    # polls the named marker and respawns the lane when it
                    # lands — the failure counter never sees this end.
                    say(f"pool: lane {unit} waiting on {parsed[1].relative_to(wt)} "
                        "— no failure count")
                    continue
                # A lane that keeps ending without its marker would otherwise be
                # re-run forever (2026-09-17: ~50 sessions over 2.5h on
                # dm-daemon-api-edge). Bound it and hand the row to the director.
                n = self._lane_failures.get(unit, 0) + 1
                self._lane_failures[unit] = n
                say(f"pool: lane {unit} ended without ralph/lanes/{unit}.done "
                    f"(failure {n}/{self.max_lane_failures}) — branch {branch} kept")
                if n >= self.max_lane_failures:
                    return self._halt(f"lane {unit} failed {n} waves — see "
                                      f"target/ralph/lane-{unit}.out and branch {branch}"
                                      + halt_tail_suffix(
                                          self.paths.workdir / "target" / "ralph"
                                          / f"lane-{unit}.out"))
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
            base = cond.split(":")[0]
            if base == "needs-human" and not self.running():
                title = "OPERATOR — halt package, loop down"
            else:
                title = {"needs-human": "auto — halt package (loop running)",
                         "down": "OPERATOR — loop down",
                         "stalled": "auto — no heartbeat",
                         "disk-low": "OPERATOR — disk low"}[base]
            if self.dry:
                print(f"notify: {title}: {body}")
            else:
                self.notifier(title, body, True)
            sf.write_text(f"{cond} {now}\n")
        return cond


def install_job(name, program_args, workdir, log_path, interval=None):
    """Exit-2 text instead of a traceback when the host has no job backend."""
    try:
        return host().install_job(name, program_args, workdir, log_path, interval)
    except HostError as e:
        print(f"install: {e}", file=sys.stderr)
        return None


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


def runtime_markers(paths):
    """What must not dirty the tree: a queue's whole control dir, else the legacy set."""
    return (f"{paths.control_dir}/",) if paths.queue else RUNTIME_MARKERS


def cmd_plan(args):
    paths = paths_for(args)
    queue = Queue(paths.p(paths.state))
    models = resolve_models(args, paths)
    unit = queue.current()
    print(f"prompt: {prompt_text(paths)[1]}  queue: {paths.state}")
    every = paths.manifest.audit_every if paths.manifest else None
    print(f"units since audit: {queue.units_since_audit()}"
          f"{f' (audit every {every})' if every else ''}  head: {head_of(paths.workdir)[:9]}")
    print(f"done: {queue.done_count()}/{len(queue.rows)}")
    if unit is None:
        print("no ready unit")
        return 0
    routed = select_model_args(unit, models["MODEL"], models["REVIEW_MODEL"],
                               models["VARIANT"])
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
    paths = paths_for(args)
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


def models_in_manifest(args, paths):
    """`models --queue`: the queue's own `[models]`, never the shared models.env."""
    toml_keys = {v: k for k, v in MANIFEST_MODEL_KEYS.items()}
    updates = {toml_keys[key]: getattr(args, flag) for key, flag in MODEL_FLAGS.items()
               if getattr(args, flag)}
    if updates:
        write_manifest_models(paths.p(paths.manifest.path), updates)
        say(f"models: wrote [models] {', '.join(updates)} in {paths.manifest.path} — "
            "a running loop reads it on its next start")
    current = load_manifest(paths.workdir, paths.queue).models
    print(f"models: {paths.manifest.path} [models]")
    for key in MODEL_KEYS:
        print(f"  {toml_keys[key]}={current.get(key) or '<unset>'}")
    return 0


def cmd_models(args):
    paths = paths_for(args)
    if paths.manifest:
        return models_in_manifest(args, paths)
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
            f"VARIANT={current.get('VARIANT', '')}\n"
            + (f"WAIT_LIMIT_S={current[WAIT_LIMIT_KEY]}\n" if current.get(WAIT_LIMIT_KEY) else ""))
        if not args.no_restart and args.label:
            job = job_name(paths, args.label)
            try:
                restarted = host().restart_job(job)
            except HostError as e:
                print(f"models: {e}; {job} was not restarted", file=sys.stderr)
            else:
                if restarted:
                    print(f"restarted {job} (any in-flight session was killed)")
                else:
                    print(f"{job} is not loaded — the change applies on the next start")
    print(f"models: {paths.models}")
    print(f"  MODEL={current.get('MODEL') or '<unset>'}")
    print(f"  REVIEW_MODEL={current.get('REVIEW_MODEL') or '<unset>'}")
    print(f"  RESOLVE_MODEL={current.get('RESOLVE_MODEL') or '<unset>'}")
    print(f"  VARIANT={current.get('VARIANT') or '<unset>'}")
    print(f"  WAIT_LIMIT_S={current.get(WAIT_LIMIT_KEY) or f'<default {DEFAULT_WAIT_LIMIT_S}>'}")
    return 0


def cmd_pool(args):
    paths = paths_for(args)
    models = resolve_models(args, paths)
    base = args.base_branch or subprocess.run(
        ["git", "-C", str(paths.workdir), "rev-parse", "--abbrev-ref", "HEAD"],
        capture_output=True, text=True).stdout.strip()

    def session_for(cwd, env=None):
        return Session(paths, timeout=args.session_timeout,
                       notify_enabled=args.notify, cwd=cwd, env=env)

    pool = Pool(paths, session_for=session_for, notify_enabled=args.notify,
                lanes=args.lanes, base_branch=base, conflicts=args.conflicts,
                prompt=args.prompt, state=args.state,
                marker_timeout=resolve_wait_limit(args, paths),
                model=models["MODEL"], review_model=models["REVIEW_MODEL"],
                variant=models["VARIANT"])
    if args.install_launchd:
        ensure_excludes(paths.workdir, RUNTIME_MARKERS + (".ralph/",))
        inner = [sys.executable, str(pathlib.Path(__file__).resolve()), "pool",
                 "--workdir", str(paths.workdir), "--label", args.label,
                 "--prompt", args.prompt, "--state", args.state,
                 "--lanes", str(args.lanes)]
        if args.notify:
            inner.append("--notify")
        plist = install_job(f"dev.ralph.{paths.workdir.name}-{args.label}", inner,
                                paths.workdir,
                                str(state_dir_for(paths, args.label) / "launchd.log"))
        print(f"wrote {plist}" if plist else "nothing installed")
        return 0 if plist else 2
    return guarded(pool.run, paths, notify_enabled=args.notify)


def cmd_report(args):
    paths = paths_for(args)
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
    markers = [pathlib.PurePath(m).name for m in (paths.done, paths.stop, paths.needs_human)
               if paths.p(m).exists()]
    print(f"markers: {', '.join(markers) if markers else 'none'}")
    decisions = paths.p("ralph/DECISIONS.md")
    if decisions.exists():
        lines = [l for l in decisions.read_text().splitlines() if l.strip()]
        after = sum(1 for l in lines if "REVIEW-AFTER:" in l)
        print(f"\n=== ralph/DECISIONS.md — {after} REVIEW-AFTER, last {args.lines} lines ===")
        print("\n".join(lines[-args.lines:]))
    else:
        print("\nralph/DECISIONS.md: not written yet")
    ranges = paths.p(paths.director_commits)
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
    paths = paths_for(args)
    state_dir = state_dir_for(paths, args.label)
    watch = Watch(paths, label=args.label, dry=os.environ.get("RALPH_WATCH_DRY") == "1")
    if args.install_launchd:
        plist = install_job(
            f"dev.ralphwatch.{paths.workdir.name}-{args.label}",
            [sys.executable, str(pathlib.Path(__file__).resolve()),
             "watch", "--workdir", str(paths.workdir), "--label", args.label,
             *(["--queue", paths.queue] if paths.queue else [])],
            paths.workdir, state_dir / "watch.log", interval=120)
        print(f"wrote {plist}" if plist else "nothing installed")
        return 0 if plist else 2
    watch.run(state_dir / "watch.state")
    return 0


DEFAULT_SESSION_TIMEOUT = 3600
LEGACY_PATH_FLAGS = ("prompt", "state", "charter", "conflicts")


def paths_for(args):
    """The one place a subcommand turns its flags into `Paths`: `--queue` wins,
    the legacy flags are next, the legacy defaults are last.

    `run` and `supervise` accept `--prompt` and `--state` and, until
    2026-09-17, built `Paths(workdir)` with the DEFAULTS - so a staged queue
    passed by flag was silently swapped for `ralph/STATE.md` (ARCH 6). Found
    the expensive way: a ring-doc launch spent six minutes on a domains row.
    `plan`, `stop`, `start`, `watch`, `models` and `report` still did that
    until 2026-09-19; they take `--queue` now and come through here too.
    """
    workdir = pathlib.Path(args.workdir).resolve()
    name = getattr(args, "queue", None)
    if not name:
        if getattr(args, "label", "") is None:
            args.label = "campaign"
        if getattr(args, "session_timeout", 0) is None:
            args.session_timeout = DEFAULT_SESSION_TIMEOUT
        return Paths(workdir,
                     prompt=getattr(args, "prompt", None) or "ralph/PROMPT.md",
                     state=getattr(args, "state", None) or "ralph/STATE.md",
                     charter=getattr(args, "charter", None) or "ralph/CHARTER.md",
                     conflicts=getattr(args, "conflicts", None) or "ralph/conflicts.txt")
    m = load_manifest(workdir, name)
    for flag in LEGACY_PATH_FLAGS:
        if getattr(args, flag, None):
            say(f"--queue {name} wins: ignoring --{flag} {getattr(args, flag)} "
                f"({m.path} names {getattr(m, flag)})")
    if getattr(args, "label", "") is None:
        args.label = m.label
    given = getattr(args, "session_timeout", 0)
    if m.session_timeout:
        if given and given != m.session_timeout:
            say(f"--queue {name} wins: ignoring --session-timeout {given} "
                f"({m.path} names {m.session_timeout})")
        args.session_timeout = m.session_timeout
    elif given is None:
        args.session_timeout = DEFAULT_SESSION_TIMEOUT
    addendum = f"{STAGED_DIR}/{name}/PROMPT.addendum.md"
    if m.prompt_declared or not (workdir / addendum).is_file():
        addendum = ""
    return Paths(workdir, prompt=m.prompt, state=m.state, charter=m.charter,
                 prompt_addendum=addendum,
                 conflicts=m.conflicts, heavy=m.heavy, queue=name, manifest=m,
                 log_dir=f"target/ralph/{name}", **Paths.control_files(m.control_dir))


def queue_flags(paths):
    """How a re-invocation (an installed job) names the same queue."""
    if paths.queue:
        return ["--queue", paths.queue]
    return ["--prompt", paths.prompt, "--state", paths.state]


def cmd_run(args):
    paths = paths_for(args)
    models = resolve_models(args, paths)
    session = Session(paths, timeout=args.session_timeout,
                      notify_enabled=args.notify)
    campaign = Campaign(
        paths,
        session_run=lambda model_args, prompt, log: session.run(model_args, prompt, log),
        notify_enabled=args.notify,
        max_stall=args.max_stall, max_iter=args.max_iter,
        marker_timeout=resolve_wait_limit(args, paths),
        model=models["MODEL"], review_model=models["REVIEW_MODEL"], variant=models["VARIANT"])
    if args.install_launchd:
        ensure_excludes(paths.workdir, runtime_markers(paths))
        plist = install_job(
            f"dev.ralph.{paths.workdir.name}-{args.label}",
            [sys.executable, str(pathlib.Path(__file__).resolve()), "run",
             "--workdir", str(paths.workdir), "--label", args.label,
             *queue_flags(paths)],
            paths.workdir, str(state_dir_for(paths, args.label) / "launchd.log"))
        print(f"wrote {plist}" if plist else "nothing installed")
        return 0 if plist else 2
    result = guarded(campaign.run, paths, notify_enabled=args.notify)
    if isinstance(result, int):
        return result
    print(f"campaign: {result.outcome.value} — {result.reason}")
    return {Outcome.DONE: 0, Outcome.OPERATOR_STOP: 0, Outcome.NEEDS_HUMAN: 2,
            Outcome.HALT: 3}[result.outcome]


def cmd_supervise(args):
    paths = paths_for(args)
    models = resolve_models(args, paths)
    session = Session(paths, timeout=args.session_timeout, notify_enabled=args.notify)
    campaign = list(args.campaign)
    if campaign and campaign[0] == "--":
        campaign = campaign[1:]
    if not campaign:
        print("ralph supervise: the campaign command is required after --", file=sys.stderr)
        return 2
    charter_path = paths.p(paths.charter)
    charter = charter_path.read_text() if charter_path.exists() else None
    if charter:
        say(f"supervisor: director charter loaded from {charter_path}")
    ensure_excludes(paths.workdir, runtime_markers(paths))

    def run_inner():
        subprocess.run(campaign, cwd=str(paths.workdir))

    def resolver_run(attempt, reason):
        resolve_model = models["RESOLVE_MODEL"] or models["REVIEW_MODEL"]
        resolve_variant = args.resolve_variant or models["VARIANT"]
        model_args = select_model_args("resolver", resolve_model, "", resolve_variant)
        prompt = resolver_prompt(paths, attempt, args.resolve_max, reason, charter)
        session.run(model_args, prompt,
                    str(paths.p(paths.log_dir) / f"supervise-{attempt}.out"))

    supervisor = Supervisor(paths, run_inner=run_inner, resolver_run=resolver_run,
                            notify_enabled=args.notify, resolve_max=args.resolve_max)
    if args.install_launchd:
        ensure_excludes(paths.workdir, runtime_markers(paths))
        plist = install_job(
            f"dev.ralph.{paths.workdir.name}-{args.label}",
            [sys.executable, str(pathlib.Path(__file__).resolve()), "supervise",
             "--workdir", str(paths.workdir), "--label", args.label,
             "--session-timeout", str(args.session_timeout),
             *(["--queue", paths.queue] if paths.queue else [])] + campaign,
            paths.workdir, str(state_dir_for(paths, args.label) / "launchd.log"))
        print(f"wrote {plist}" if plist else "nothing installed")
        return 0 if plist else 2
    return guarded(supervisor.run, paths, notify_enabled=args.notify)


def cmd_prompt(args):
    """What a worker of this queue is given, exactly."""
    sys.stdout.write(prompt_text(paths_for(args))[0])
    return 0


def cmd_check_argv(args):
    """ralph-check.sh's lookup: the argv a queue's `[checks]` declares for a
    name, one argument per line. 1 = not declared (the script falls through to
    its own verbs); a refused manifest is main()'s exit 2."""
    paths = paths_for(args)
    argv = paths.manifest.checks.get(args.name)
    if argv is None:
        return 1
    print("\n".join(argv))
    return 0


def cmd_stop(args):
    paths = paths_for(args)
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
    try:
        host().stop_job(job_name(paths, args.label))
    except HostError as e:
        say(f"stop: {e}")
    strays = sessions_under(paths.workdir)
    for pid in strays:
        try:
            os.killpg(os.getpgid(pid), signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            pass
    say(f"stop: booted out; {len(strays)} stray session(s) taken down")
    return 0


def cmd_start(args):
    paths = paths_for(args)
    job = job_name(paths, args.label)
    try:
        host().require_jobs()
        job_file = host().job_file(job)
        if not job_file.exists():
            print(f"start: no job installed at {job_file} — install it first:\n"
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
        host().start_job(job)
    except HostError as e:
        print(f"start: {e}", file=sys.stderr)
        return 2
    say(f"start: {job} started")
    return 0


def build_parser():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    def queue_flag(p, label_default=None):
        p.add_argument("--workdir", default=".")
        p.add_argument("--queue", default="",
                       help="a queue under ralph/next/<name>/ with a queue.toml: state, prompt, "
                            "charter, models, checks and control files all come from it")
        # None = not given: paths_for fills in the manifest's label, else "campaign".
        p.add_argument("--label", default=label_default)

    def common(p, notify_default=False, queue=True):
        if queue:
            queue_flag(p)
        else:
            p.add_argument("--workdir", default=".")
            p.add_argument("--label", default="campaign")
        p.add_argument("--session-timeout", type=int, default=None if queue else 3600,
                       help="default: 3600")
        p.add_argument("--marker-timeout", type=int, default=None,
                       help="default: WAIT_LIMIT_S from ralph/models.env, else 86400")
        p.add_argument("--notify", action="store_true", default=notify_default)
        p.add_argument("--model", default="")
        p.add_argument("--review-model", default="")
        p.add_argument("--variant", default="")

    p = sub.add_parser("run")
    common(p)
    p.add_argument("--prompt", default=None, help="default: ralph/PROMPT.md")
    p.add_argument("--state", default=None, help="default: ralph/STATE.md")
    p.add_argument("--max-stall", type=int, default=3)
    p.add_argument("--max-iter", type=int, default=200)
    p.add_argument("--install-launchd", "--install-job", dest="install_launchd",
                   action="store_true", help="launchd on macOS, systemd-run --user on Linux")
    p.set_defaults(fn=cmd_run)

    p = sub.add_parser("supervise")
    common(p, notify_default=True)
    p.add_argument("--prompt", default=None, help="default: ralph/PROMPT.md")
    p.add_argument("--state", default=None, help="default: ralph/STATE.md")
    p.add_argument("--resolve-model", default="")
    p.add_argument("--resolve-variant", default="")
    p.add_argument("--resolve-max", type=int, default=4)
    p.add_argument("--charter", default="", help="default: ralph/CHARTER.md when present")
    p.add_argument("--install-launchd", "--install-job", dest="install_launchd",
                   action="store_true", help="launchd on macOS, systemd-run --user on Linux")
    p.add_argument("campaign", nargs=argparse.REMAINDER)
    p.set_defaults(fn=cmd_supervise)

    p = sub.add_parser("pool")
    common(p, queue=False)          # another repo drives this verb; it stays on its flags
    p.add_argument("--prompt", default="ralph/PROMPT.md")
    p.add_argument("--state", default="ralph/STATE.md")
    p.add_argument("--lanes", type=int, default=2)
    p.add_argument("--conflicts", default="ralph/conflicts.txt")
    p.add_argument("--base-branch", default="")
    p.add_argument("--install-launchd", "--install-job", dest="install_launchd",
                   action="store_true", help="launchd on macOS, systemd-run --user on Linux")
    p.set_defaults(fn=cmd_pool)

    p = sub.add_parser("watch")
    queue_flag(p)
    p.add_argument("--install-launchd", "--install-job", dest="install_launchd",
                   action="store_true", help="launchd on macOS, systemd-run --user on Linux")
    p.set_defaults(fn=cmd_watch)

    p = sub.add_parser("stop")
    queue_flag(p)
    p.add_argument("--timeout", type=int, default=180,
                   help="seconds to wait for the loop to go down before suggesting --hard")
    p.add_argument("--hard", action="store_true",
                   help="boot the job out and take stray sessions down")
    p.set_defaults(fn=cmd_stop)

    p = sub.add_parser("start")
    queue_flag(p)
    p.set_defaults(fn=cmd_start)

    p = sub.add_parser("models")
    queue_flag(p, label_default="")
    p.add_argument("--model", default="")
    p.add_argument("--review-model", default="")
    p.add_argument("--resolve-model", default="")
    p.add_argument("--variant", default="")
    p.add_argument("--no-restart", action="store_true")
    p.set_defaults(fn=cmd_models)

    p = sub.add_parser("report")
    queue_flag(p)
    p.add_argument("--lines", type=int, default=40)
    p.set_defaults(fn=cmd_report)

    p = sub.add_parser("plan")
    common(p)
    p.set_defaults(fn=cmd_plan)

    p = sub.add_parser("prompt")
    queue_flag(p)
    p.add_argument("--prompt", default=None, help="default: ralph/PROMPT.md")
    p.set_defaults(fn=cmd_prompt)

    p = sub.add_parser("check-argv")
    p.add_argument("name")
    p.add_argument("--workdir", default=".")
    p.add_argument("--queue", required=True)
    p.set_defaults(fn=cmd_check_argv)

    p = sub.add_parser("promote")
    p.add_argument("name", help="the staged campaign under ralph/next/")
    p.add_argument("--workdir", default=".")
    p.add_argument("--label", default="campaign", help="the ACTIVE loop's label")
    p.add_argument("--dry-run", action="store_true",
                   help="parse the staged queue and print its head; change nothing")
    p.set_defaults(fn=cmd_promote)

    return ap


def main(argv=None):
    _install_signal_handlers()
    args = build_parser().parse_args(argv)
    try:
        return args.fn(args)
    except ValueError as e:
        if not getattr(args, "queue", None):
            raise
        print(f"ralph {args.cmd}: {e}", file=sys.stderr)      # a refused manifest, by name
        return 2


if __name__ == "__main__":
    sys.exit(main())
