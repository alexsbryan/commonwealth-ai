#!/usr/bin/env python3
"""ralph.py — a coding-agent campaign loop, as a closed state machine.

The queue is a STATE.md of rows; a unit is one row. The loop dispatches ready
units to agent sessions, one at a time in the main tree (`run`) or in lanes
beside it (`pool`), and lands what they finish. The machine is a table
(TRANSITIONS, with every other pair in IMPOSSIBLE and a reason), and every
event it consumes comes from something the loop owns: a process it started
ending, its own git operations, its clock against a budget it recorded, or an
operator act. A session ends by calling `ralph-result` exactly once (done,
continue, await <budget> -- <cmd>, needs-human); nothing else an agent writes
is read as loop state. Why: docs/RALPH_STATE_MACHINE.md.

`--queue <name>` runs `ralph/next/<name>/` from its `queue.toml`: its own
state, prompt, charter, models, checks and control files (`ctl/` beside the
manifest), so two loops share a checkout. Without it every flag and default
is the legacy one.

Subcommands:
  run        the serial loop: one unit at a time, in the main tree
  pool       the parallel loop: units in worktree lanes, reviews in the main tree
  result     how a session ends (on a session's PATH as `ralph-result`)
  status     what the loop holds, and what waits on the operator
  follow     status, then the live output of the units it holds (sessions and runs)
  stop       an operator stop (--drain: let running sessions finish)
  start      clear the operator stop and start the installed job
  unpark     release a row the loop parked for the operator
  watch      the watchdog: the loop is down, or its heartbeat is stale
  models     show or set the models a queue runs on
  plan       print the queue's head and the model it routes to
  report     the queue, the director's decisions and recent commits
  promote    make a staged campaign (ralph/next/<name>/) the active one
  prompt     print the worker prompt a queue runs on
  check-argv the argv a queue's [checks] declares (scripts/ralph-check.sh asks)
  supervise  retired: runs (or installs) the campaign command after --
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
import traceback
import urllib.parse

HASH_RE = re.compile(r"^[0-9a-f]{7,40}$")
ROW_RE = re.compile(
    r"^- \[(?P<mark>[x~ ])\] (?P<head>.*?) — depends \[(?P<deps>[^\]]*)\](?P<rest>.*)$"
)


def say(msg: str) -> None:
    # A log line that cannot be written (a full disk) is dropped: the loop that
    # was saying it must not die of it (2026-10-03).
    try:
        print(f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} {msg}", flush=True)
    except OSError:
        pass
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

    def install_job(self, name, argv, workdir, log_path, interval=None, keep_alive=False):
        """`keep_alive`: the host restarts the job on any exit but 0 — the
        loop exits 0 only when done or stopped, so a crash is the host's to
        restart and a finished loop stays down."""
        self.require_jobs()
        return self._install_job(name, [str(a) for a in argv], str(workdir), str(log_path),
                                 interval, keep_alive)

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
    """launchd, with the plist loaded by path from ralph's jobs dir. Never from
    ~/Library/LaunchAgents: launchd re-runs everything there at each login, so a
    job written there outlived its queue and every finished loop came back at
    boot (2026-10-05). Bootstrapped by path, a job runs until it exits or the
    login session ends, as LinuxHost's transient unit does until a reboot."""
    notifier = "/usr/bin/osascript"
    job_tools = ("launchctl",)

    def _notify_argv(self, title, body):
        return [self.notifier, "-e", f'display notification "{body}" with title "ralph: {title}"']

    def _domain(self, name=""):
        return f"gui/{os.getuid()}" + (f"/{name}" if name else "")

    def job_file(self, name):
        return self.home / ".config" / "ralph" / "jobs" / f"{name}.plist"

    def require_jobs(self):
        super().require_jobs()
        self._adopt_login_agents()

    def _adopt_login_agents(self):
        """Move the plists ralph wrote into ~/Library/LaunchAgents before
        2026-10-05 into the jobs dir. A loaded job keeps running and `start`
        still finds its file; only the next login stops running it."""
        agents = self.home / "Library" / "LaunchAgents"
        for legacy in sorted(agents.glob("dev.ralph*.plist")):
            dest = self.job_file(legacy.stem)
            try:
                dest.parent.mkdir(parents=True, exist_ok=True)
                if dest.exists():
                    legacy.unlink()
                    say(f"jobs: removed the login-agent copy of {legacy.stem}; {dest} is the job")
                else:
                    legacy.replace(dest)
                    say(f"jobs: moved {legacy.stem} out of ~/Library/LaunchAgents to {dest}; "
                        f"it no longer runs at login")
            except OSError as e:
                say(f"jobs: could not move {legacy} out of ~/Library/LaunchAgents ({e}); "
                    f"it will run again at the next login")

    def _job_running(self, name):
        r = self._run(["launchctl", "print", self._domain(name)], capture_output=True, text=True)
        return "state = running" in r.stdout

    def _install_job(self, name, argv, workdir, log_path, interval, keep_alive=False):
        plist = self.job_file(name)
        args = "\n".join(f"    <string>{a}</string>" for a in argv)
        schedule = (f"  <key>StartInterval</key><integer>{interval}</integer>\n"
                    if interval else "  <key>RunAtLoad</key><true/>\n")
        if keep_alive:
            schedule += ("  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n"
                         "  <key>ThrottleInterval</key><integer>60</integer>\n")
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

    def _install_job(self, name, argv, workdir, log_path, interval, keep_alive=False):
        spec = self.job_file(name)
        spec.parent.mkdir(parents=True, exist_ok=True)
        spec.write_text(json.dumps({"argv": argv, "workdir": workdir, "log": log_path,
                                    "interval": interval, "keep_alive": keep_alive},
                                   indent=2) + "\n")
        return spec

    def _start_job(self, name):
        job = json.loads(self.job_file(name).read_text())
        self._stop_job(name)
        timer = (["--on-active=1", f"--on-unit-active={job['interval']}"]
                 if job["interval"] else [])
        if job.get("keep_alive"):
            timer += ["-p", "Restart=on-failure", "-p", "RestartSec=60"]
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
class Status(enum.Enum):
    PENDING = " "
    ACTIVE = "~"
    DONE = "x"

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
        # A cycle among open rows is rows nothing can ever make ready: refused
        # here, by name, so the loop is never left with nothing ready and
        # nothing held. A done row blocks nothing (ersilia's REVIEW-16 cycle
        # closed with all three rows [x]), so only open-to-open edges count.
        state = {}

        def visit(row_id, trail):
            if state.get(row_id) == "open":
                raise ValueError(f"{self.path}: dependency cycle "
                                 f"{' -> '.join(trail[trail.index(row_id):] + [row_id])}")
            if state.get(row_id) == "closed":
                return
            state[row_id] = "open"
            for d in by[row_id].deps:
                if by[d].status is not Status.DONE:
                    visit(d, trail + [row_id])
            state[row_id] = "closed"

        for r in self.rows:
            if r.status is not Status.DONE:
                visit(r.id, [])

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

    def block(self, row_id):
        """The row's line and its indented continuation, up to the next row or
        heading: everything the worker reads as that row."""
        row = self.by_id()[row_id]
        lines = self.path.read_text().splitlines()
        out = [lines[row.lineno - 1]]
        for line in lines[row.lineno:]:
            if ROW_RE.match(line) or line.startswith("#"):
                break
            out.append(line)
        return "\n".join(out)

    def unmet_requirements(self, row, requires):
        """The `dispatch_requires` markers this row's block lacks. Reviews and
        HUMAN- rows are exempt: a review is where a census is written, and a
        HUMAN- row is the operator's act."""
        if not requires or is_review(row) or row.id.startswith("HUMAN-"):
            return []
        text = self.block(row.id)
        return [m for m in requires if m not in text]

    def done_count(self):
        return sum(1 for r in self.rows if r.status is Status.DONE)

    def deps_met(self, row):
        by = self.by_id()
        return all(by[d].status is Status.DONE for d in row.deps)

    def current(self, parked=frozenset()):
        """The unit the loop runs next: the `[~]` row, else the first ready `[ ]`
        row. A HUMAN- row is never a unit and neither is a parked row: both wait
        on the operator while every row that does not depend on them runs
        (phase-b-31, operator 2026-09-27: "rather than using every roadblock as a
        total stop"). Until then the first ready HUMAN- row halted the loop, and
        HUMAN-pb-lanes-dials-serve held 40 independent rows for 9 h."""
        def waits(r):
            return r.id.startswith("HUMAN-") or r.id in parked
        for r in self.rows:
            if r.status is Status.ACTIVE and not waits(r):
                return r
        for r in self.rows:
            if r.status is Status.PENDING and not waits(r) and self.deps_met(r):
                return r
        return None

    def awaiting_operator(self, parked=frozenset()):
        """What only the operator can move: ready HUMAN- rows, then parked rows."""
        human = [r.id for r in self.rows if r.id.startswith("HUMAN-")
                 and r.status is not Status.DONE and self.deps_met(r)]
        held = [r.id for r in self.rows if r.id in parked and r.status is not Status.DONE
                and not r.id.startswith("HUMAN-")]
        return human + held

    def mark_done(self, row_id, sha=""):
        """`[x]` the row and, when it names no commit yet, name `sha` after
        its id, as ralph-mark.sh writes it."""
        row = self.by_id()[row_id]
        lines = self.path.read_text().splitlines()
        old = lines[row.lineno - 1]
        if old != row.line:
            raise ValueError(f"{self.path}:{row.lineno}: row moved under mark_done")
        new = "- [x]" + old[5:]
        if sha and row.hash is None and HASH_RE.fullmatch(sha):
            new = re.sub(rf"^- \[x\] {re.escape(row_id)}(?= )", f"- [x] {row_id} {sha}", new)
        lines[row.lineno - 1] = new
        self.path.write_text("\n".join(lines) + "\n")
        self.rows = self._parse()

    def insert_before(self, row_id, line):
        """Add a row above another; the file is rewritten as mark_done does it."""
        row = self.by_id()[row_id]
        lines = self.path.read_text().splitlines()
        if lines[row.lineno - 1] != row.line:
            raise ValueError(f"{self.path}:{row.lineno}: row moved under insert_before")
        lines.insert(row.lineno - 1, line)
        self.path.write_text("\n".join(lines) + "\n")
        self.rows = self._parse()

    def append_row(self, line):
        """Add a row below the last one, as insert_before adds one above."""
        last = self.rows[-1]
        lines = self.path.read_text().splitlines()
        if lines[last.lineno - 1] != last.line:
            raise ValueError(f"{self.path}:{last.lineno}: row moved under append_row")
        lines.insert(last.lineno, line)
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

AUDIT_PREFIX = "REVIEW-audit-"
# The one text of a cadence audit (`audit_every` in queue.toml; Campaign inserts
# the row). A queue's hand-written audit rows stay its author's.
AUDIT_ROW_BODY = (
    "audit the commits since the previous `REVIEW-audit-` row's hash (since this queue "
    "started if there is none) against `docs/ARCH_PRINCIPLES.md`; REUSE AND SIZE, WITH "
    "DATA: (1) paste a per-unit net-line ledger for product code over that range — "
    "`git log --numstat --format=%s <hash>..HEAD`, summed by the subject's unit id, src "
    "apart from tests; (2) run `target/debug/sovereign-cli code dry-report --scope <dir>` "
    "for each crate dir the range touched and paste every exact or near clone with a side "
    "that is a symbol ADDED in the range; (3) for every new `struct`/`enum`/`trait` in the "
    "range, `target/debug/sovereign-cli code converge noun <Name>`, and paste any noun "
    "with more than one definition; fix here what is behaviour-preserving and small, "
    "record the rest in `ralph/REVIEW_FINDINGS.md` with both file:line sites "
    "— read: `docs/ARCH_PRINCIPLES.md` — check: LINT")


# The one reading of "units since the last audit": audit_every's trigger, the
# inserted row's subject and `plan` all ask it. Rows are not closed in file
# order (minted rows land mid-file, parked rows stay put, and an audit row is
# inserted above the unit in flight), so counting the `[x]` rows BELOW the last
# audit row also counts older rows that sit lower. On 2026-09-24 five-programs'
# audit-5 fired five units after audit-4 because eleven rows closed on
# 2026-09-22/23 sat below it. Time is in git: every close is a `ralph: <id> done`
# commit of the queue file (scripts/ralph-mark.sh), so this counts those after
# the newest audit's. A queue file with no such commit (a legacy launch line, a
# queue that never marked through the script) keeps Queue's positional count.
MARK_SUBJECT = re.compile(r"^ralph: (\S+) done$")


def units_since_audit(paths, queue):
    r = subprocess.run(["git", "-C", str(paths.workdir), "log", "--format=%s", "--",
                        paths.state], capture_output=True, text=True)
    marks = ([m.group(1) for s in r.stdout.splitlines() if (m := MARK_SUBJECT.match(s))]
             if r.returncode == 0 else [])
    if not marks:
        return queue.units_since_audit()
    n = 0
    for unit_id in marks:                    # newest first
        if unit_id.startswith(AUDIT_PREFIX):
            break
        if not unit_id.startswith("HUMAN-"):
            n += 1
    return n


def dispatch_refusal(paths, queue, row):
    """The one reading of `dispatch_requires` (phase-b-29: census before code).
    A row missing a marker is refused by name, never skipped: a skipped row
    runs later on a premise nobody trialled."""
    requires = paths.manifest.dispatch_requires if paths.manifest else ()
    missing = queue.unmet_requirements(row, requires)
    if not missing:
        return None
    return (f"{row.id} is not ready to dispatch: its row lacks {', '.join(repr(m) for m in missing)} "
            f"({paths.manifest.path} dispatch_requires) — census and trial it first")


def audit_row(row_id, after):
    return f"- [ ] {row_id} — depends [{after}] — {AUDIT_ROW_BODY}"


def audit_due(paths, queue, unit):
    """`audit_every`, enforced where rows are dispatched: a 22-row queue ran
    ~60 commits on one closing audit because a cadence was its author's to
    remember. A [~] row is finished first — `current()` returns it until it
    closes, so a row inserted above it would be inserted again every pass."""
    every = paths.manifest.audit_every if paths.manifest else None
    return bool(every and unit.status is Status.PENDING
                and not unit.id.startswith(AUDIT_PREFIX)
                and units_since_audit(paths, queue) >= every)


def closing_audit_due(paths, queue):
    """`audit_every` at a queue's end. audit_due runs where a unit is
    dispatched, so units that landed with no row after them closed unaudited
    (svrngs u3, 2026-10-05: four units, and the core changes landed between
    them, never reviewed). A finished queue owes one audit while any unit
    closed since the last."""
    every = paths.manifest.audit_every if paths.manifest else None
    return bool(every and queue.rows and queue.all_done()
                and units_since_audit(paths, queue) > 0)


def insert_audit(paths, queue, unit):
    """(the inserted row, git's refusal or ""): above `unit`, or below the
    last row as a closing audit when `unit` is None. The id's <prefix> is the
    addendum's `prefix` var, else the queue's name."""
    prefix = paths.queue
    if paths.prompt_addendum:
        for name, _, _, body in prompt_sections(
                paths.p(paths.prompt_addendum).read_text(), "addendum"):
            if name == "vars":
                prefix = section_vars(body).get("prefix", prefix)
    if not re.fullmatch(r"[A-Za-z0-9._-]+", prefix):
        say(f"prefix var {prefix!r} cannot be part of a row id; using the queue name")
        prefix = paths.queue
    stem = f"{AUDIT_PREFIX}{prefix}-auto-"
    row_id = f"{stem}{1 + sum(r.id.startswith(stem) for r in queue.rows)}"
    n = units_since_audit(paths, queue)
    last_done = [r.id for r in queue.rows if r.status is Status.DONE][-1]
    if unit is None:
        queue.append_row(audit_row(row_id, last_done))
        subject = f"ralph: closing audit after {n} unit{'' if n == 1 else 's'} — {row_id}"
    else:
        queue.insert_before(unit.id, audit_row(row_id, last_done))
        subject = f"ralph: audit due after {n} units — {row_id}"
    say(subject)
    r = commit_state(paths, subject)
    refused = "" if r.returncode == 0 else (r.stderr.strip() or f"exit {r.returncode}")
    return Queue(queue.path).by_id()[row_id], refused

def parked_ids(paths):
    """The rows parked for the operator: one `<row-id>.md` package each under
    the loop's parked dir. Deleting a package unparks its row."""
    root = paths.p(paths.parked)
    if not root.is_dir():
        return frozenset()
    return frozenset(f.stem for f in root.glob("*.md"))


def out_of_scope(paths, queue):
    """Open rows outside the queue's frozen scope (queue.toml `scope_file`,
    phase-b-32, operator 2026-09-27: "We can't keep adding scope"). They wait
    on the operator as a parked row does; a new row is the operator's act. An
    audit row and a split of an in-scope row (`<id>-<suffix>`) are in scope:
    a split re-chunks scope, it does not add it."""
    rel = paths.manifest.scope_file if paths.manifest else ""
    if not rel:
        return frozenset()
    try:
        text = paths.p(rel).read_text()
    except OSError as e:
        say(f"scope file {rel} unreadable ({e}) — no row is judged out of scope")
        return frozenset()
    allowed = {line.split("#")[0].strip() for line in text.splitlines()} - {""}
    return frozenset(
        r.id for r in queue.rows
        if r.status is not Status.DONE and r.id not in allowed
        and not r.id.startswith(AUDIT_PREFIX)
        and not any(r.id.startswith(f"{a}-") for a in allowed))


def write_parked(paths, row_id, package, reason):
    """The one spelling of a parked row's package: `<parked>/<row-id>.md`."""
    dest = paths.p(paths.parked) / f"{row_id}.md"
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(package + f"\n<!-- parked {row_id}: {reason}. Answer in the "
                    f"row, then delete this file to unpark it. -->\n")
    return dest


def held_ids(paths, queue):
    """Every row that waits on the operator besides HUMAN- rows: parked rows
    and rows outside the frozen scope. The one set the planners skip."""
    return parked_ids(paths) | out_of_scope(paths, queue)


def blocked_row(queue, title, parked=frozenset()):
    """The one open row a halt package is about, or None. The title must name
    exactly one open non-HUMAN row; an operator-only package may instead rest
    on the one `[~]` row. A halt that names no row (a stall, a full disk, an
    unready queue) is not a row's block, and parking on it would walk the loop
    through the whole queue."""
    open_rows = [r for r in queue.rows if r.status is not Status.DONE
                 and not r.id.startswith("HUMAN-") and r.id not in parked]
    named = [r for r in open_rows
             if re.search(rf"(?<![\w-]){re.escape(r.id)}(?![\w-])", title)]
    if len(named) == 1:
        return named[0]
    return None


def waiting_row(queue, parked=frozenset()):
    """The one `[~]` row, for an operator-only package whose title names none."""
    active = [r for r in queue.rows if r.status is Status.ACTIVE
              and not r.id.startswith("HUMAN-") and r.id not in parked]
    return active[0] if len(active) == 1 else None

MODEL_KEYS = ("MODEL", "REVIEW_MODEL", "RESOLVE_MODEL", "VARIANT")


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
        if key.strip() in MODEL_KEYS:
            out[key.strip()] = value.strip()
    return out



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
    localhost). Returns "" to probe, else the cause.

    The provider/model pair is opencode's grammar. A queue that declares its
    own worker_bin (the claude shim) names models the way that client does,
    bare (`claude-opus-5-5`), so a bare id is probed through it: phase-c's
    pool halted at its first dispatch on exactly that refusal."""
    provider, _, name = model.partition("/")
    if not provider or not name:
        if paths.manifest and paths.manifest.worker_bin:
            return ""
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
    dispatch_requires: tuple = ()         # markers every work row must carry before dispatch
    scope_file: str = ""                  # the frozen row ids; "" = no freeze (phase-b-32)


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

    known = {"label", "session_timeout", "audit_every", "dispatch_requires", "worker_bin",
             "settings", "scope_file", "models", "checks", *MANIFEST_PATH_KEYS}
    for key in data:
        if key not in known:
            raise ValueError(f"{rel}: unknown key `{key}` (known: {', '.join(sorted(known))})")
    strings = {}
    for key in ("label", "worker_bin", "settings", "scope_file", *MANIFEST_PATH_KEYS):
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
    requires = data.get("dispatch_requires", [])
    if (not isinstance(requires, list)
            or not all(isinstance(m, str) and m.strip() and "\n" not in m for m in requires)):
        raise bad("dispatch_requires", "a list of non-empty one-line strings")
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
        dispatch_requires=tuple(requires), scope_file=strings["scope_file"],
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
    parked: str = "ralph/parked"          # one <row-id>.md package per row waiting on the operator
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
                "waiting": f"{control_dir}/waiting", "parked": f"{control_dir}/parked",
                "heartbeat": f"{control_dir}/.heartbeat",
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
                              "control_dir": paths.control_dir, "log_dir": paths.log_dir,
                              "result": "ralph-result"})
        return text, f"{PROMPT_BASE} + {paths.prompt_addendum}"
    return paths.p(paths.prompt).read_text(), paths.prompt
# A driver heartbeat younger than this means a loop is live. One number for the
# watchdog's "stalled" and promote's "still running".
STALL_SECS = 300


def job_name(paths, label):
    return f"dev.ralph.{paths.workdir.name}-{label}"


def job_running(paths, label):
    return host().job_running(job_name(paths, label))


def session_env(paths):
    """What every worker session is told about the loop that spawned it, so
    ralph-mark.sh and ralph-check.sh need no per-campaign default. RALPH_QUEUE
    is set even when empty: a legacy loop launched from inside a queue's
    session must not inherit that queue. RALPH_CLAUDE_SETTINGS likewise (the
    shim reads empty as its default). RALPH_WORKDIR is the loop's checkout, not
    a pool lane's worktree: the permission bridge asks the operator there."""
    settings = paths.manifest.settings if paths.manifest else ""
    return {"RALPH_QUEUE": paths.queue, "RALPH_STATE": paths.state,
            "RALPH_CONTROL_DIR": paths.control_dir,
            "RALPH_WORKDIR": str(paths.workdir.resolve()),
            "RALPH_CLAUDE_SETTINGS": str(paths.p(settings)) if settings else ""}


def worker_bin(paths):
    """The manifest's worker_bin (a path is relative to the workdir), then
    RALPH_OPENCODE_BIN, then opencode."""
    declared = paths.manifest.worker_bin if paths.manifest else ""
    if declared:
        return str(paths.p(declared)) if "/" in declared else declared
    return os.environ.get("RALPH_OPENCODE_BIN", "opencode")

# A conflicts.txt line `<id> *` pairs the row with every other: it runs alone.
ALONE = "*"


def conflict_pairs(text):
    """A line of N ids means all N-choose-2 pairs; `#` starts a comment. Reading
    only the first two dropped the third id of ring-doc's line without a word.
    `*` among the ids is ALONE: each other id on the line runs in a wave of its
    own (`Loop._pick`)."""
    pairs = set()
    for line in text.splitlines():
        ids = line.split("#")[0].split()
        pairs.update(frozenset(pair) for pair in itertools.combinations(ids, 2))
    return pairs


# The one cargo budget (lib/cargo-jobs.sh), split across the lanes of a wave.
CARGO_JOBS_LIB = pathlib.Path(__file__).resolve().parent / "lib" / "cargo-jobs.sh"
# No wave starts with less free disk than this on the lane root: the red line,
# the one the watchdog's disk-low alert fires at (Watch.min_free_mb). Waves
# started into a full disk failed ENOSPC everywhere — every session, the model
# probes (opencode's own database), the supervisor's halt notice — and the loop
# died without a word (2026-10-03). Under it the pool first reclaims idle lanes'
# build output (Pool._reclaim_disk), and waits only when that is not enough. It
# was 40GB, a reserve for a lane's debug target outgrowing its clone (two ersilia
# r12 lanes reached 68G each); that reserve sat the pool idle for hours with
# 13-32GB free, room enough to run a lane (2026-10-04, 10-06).
DISK_FLOOR_GB = 5
# A lane session that ends without its done marker but with new commits ran out
# of time mid-work, not into a wall: it continues without a strike, at most this
# many times in a row before the strikes count again (12h at a 2h session cap).
MAX_LANE_CONTINUATIONS = 6
# This file, as the pool re-execs onto it between waves (Pool._maybe_reexec).
SELF = pathlib.Path(__file__).resolve()
# Where a lane reads its share: a file, because `toolbox run` forwards no env.
LANE_JOBS_FILE = "target/ralph/lane.env"
LANE_JOBS_VARS = ("SOVEREIGN_LINT_JOBS", "SOVEREIGN_TEST_JOBS")


def cargo_jobs_share(lanes):
    """(jobs per lane, reason) from lib/cargo-jobs.sh `cargo_jobs_share`; 0 jobs
    = free memory is under the floor for that many lanes. (None, error) when
    the decider could not be read."""
    r = subprocess.run(["bash", "-c", 'source "$0" && cargo_jobs_share "$1" && '
                        'printf "%s\\n%s\\n" "$CARGO_JOBS_SHARE" "$CARGO_JOBS_SHARE_REASON"',
                        str(CARGO_JOBS_LIB), str(lanes)], capture_output=True, text=True)
    lines = r.stdout.splitlines()
    if r.returncode != 0 or len(lines) < 2 or not lines[0].isdigit():
        return None, error_tail(r.stderr) or r.stderr.strip()[-200:] or f"exit {r.returncode}"
    return int(lines[0]), lines[1]


# ralph/DECISIONS.md is rendered from one file per decision (ralph-decisions.py).
DECISIONS_DIR = "ralph/decisions"
DECISIONS_RENDERED = "ralph/DECISIONS.md"
DECISIONS_SCRIPT = "scripts/ralph-decisions.py"


def remove_tree(path):
    """Remove `path` whole, clearing write-protection on the way, and return
    None or the error that left it in place. A read-only directory in the
    main tree's evidence (target/ralph/phase-b/ship/esc/seed, dr-xr-xr-x) is
    reflink-cloned into every lane; it stopped both an ignore_errors rmtree
    and `git worktree remove --force` from unlinking its entries, so a merged
    lane left its whole target behind (2026-10-02: four lanes, 115 GiB
    exclusive) and a cloned evidence tree was copied back as the lane's."""
    path = pathlib.Path(path)
    if not os.path.lexists(path):
        return None
    try:
        shutil.rmtree(path)
        return None
    except OSError:
        pass
    for d, dirs, _ in os.walk(path):
        for name in (d, *(os.path.join(d, x) for x in dirs)):
            if not os.path.islink(name):
                try:
                    os.chmod(name, os.stat(name).st_mode | 0o700)
                except OSError:
                    pass
    try:
        shutil.rmtree(path)
    except OSError as e:
        return e
    return None


def lane_root_for(workdir):
    """Where the pool keeps lane worktrees: beside the main tree, never inside
    it. Cargo reads every ancestor's .cargo/config.toml and concatenates their
    arrays, so a lane under the main tree ran with the main tree's
    target.rustflags twice; rustflags are part of every unit's identity, so
    each lane rebuilt every crates.io dependency its reflink-cloned target
    already held (2026-10-02, phase-c wave 2: proc-macro2 and 198 more on one
    lane's first lint, under .ralph/wt/)."""
    workdir = pathlib.Path(workdir).resolve()
    return workdir.parent / f"{workdir.name}-lanes"

# ---------------------------------------------------------------------------
# The machine (docs/RALPH_STATE_MACHINE.md), as data. A unit is one row of the
# queue. Its states and the events it consumes are closed sets, and every
# event comes from something the loop owns: a process it started ending, its
# own git operations, its clock, or an operator act (STOP, a parked file
# deleted, an edit of the queue). Nothing an agent writes is read as loop
# state; the one thing a session writes for the loop is its result
# (`ralph-result`, validated when it is called).


class U(enum.Enum):
    """A unit's state."""
    PENDING = "pending"        # a dependency is not done
    READY = "ready"
    RUNNING = "running"        # a worker session the loop holds
    DIRECTING = "directing"    # a director session the loop holds (a charter covers the unit)
    AWAITING = "awaiting"      # a background run the loop holds, with its budget
    MERGING = "merging"        # finished; the loop is landing it on the base
    HELD = "held"              # the operator's: parked, a HUMAN- row, or outside the frozen scope
    DONE = "done"


# The states that hold something the loop started. They live in the ledger;
# the other four are read from the queue and the parked dir each tick.
ACTIVE = frozenset({U.RUNNING, U.DIRECTING, U.AWAITING, U.MERGING})
SESSION = frozenset({U.RUNNING, U.DIRECTING})


class E(enum.Enum):
    """An event a unit consumes."""
    # the queue and the parked dir, re-read for a unit that holds no process
    DEPS_DONE = "deps-done"
    DEPS_UNMET = "deps-unmet"            # a dependency was reopened
    HOLD = "hold"                        # parked, made a HUMAN- row, or scoped out
    UNHOLD = "unhold"                    # the operator deleted the park, or rescoped
    ROW_CLOSED = "row-closed"            # the row reads [x]: an operator edit
    ROW_REOPENED = "row-reopened"
    # dispatch, and what it can refuse
    DISPATCH = "dispatch"
    DISPATCH_DIRECTOR = "dispatch-director"
    DISPATCH_REFUSED = "dispatch-refused"   # the row lacks a dispatch_requires marker
    SPAWN_FAILED = "spawn-failed"           # the loop could not start the session or its worktree
    STRIKE_LIMIT = "strike-limit"           # strikes reached the bound and no director is left
    # a session's end: its one result, or none
    RESULT_DONE = "result-done"
    RESULT_CONTINUE = "result-continue"
    RESULT_AWAIT = "result-await"
    AWAIT_FAILED = "await-failed"           # the loop could not start the run the result named
    RESULT_NEEDS_HUMAN = "result-needs-human"
    RESULT_NEEDS_DIRECTOR = "result-needs-director"
    NO_RESULT_COMMITS = "no-result-commits"
    NO_RESULT_NONE = "no-result-none"
    # a background run's end
    RUN_ENDED = "run-ended"                 # any end: success, failure, crash, OOM kill, reboot
    RUN_OVER_BUDGET = "run-over-budget"
    # the loop's own git operations
    MERGE_CLEAN = "merge-clean"
    MERGE_CONFLICT = "merge-conflict"
    MERGE_UNCOMMITTED = "merge-uncommitted"  # merged, but the loop's own done commit failed
    # the operator
    OPERATOR_STOP = "operator-stop"


# The events derived from the queue. They are generated only for a unit that
# holds no process: an active unit's session or run decides, and the queue is
# read for it again when it leaves.
STATIC_EVENTS = frozenset({E.DEPS_DONE, E.DEPS_UNMET, E.HOLD, E.UNHOLD, E.ROW_CLOSED,
                           E.ROW_REOPENED})

# (state, event) -> (next state, action). An action is a `Loop._do_<name>`
# method; None changes the state only.
TRANSITIONS = {
    (U.PENDING, E.DEPS_DONE): (U.READY, None),
    (U.PENDING, E.HOLD): (U.HELD, None),
    (U.PENDING, E.ROW_CLOSED): (U.DONE, None),
    (U.PENDING, E.OPERATOR_STOP): (U.PENDING, None),

    (U.READY, E.DEPS_UNMET): (U.PENDING, None),
    (U.READY, E.HOLD): (U.HELD, None),
    (U.READY, E.ROW_CLOSED): (U.DONE, None),
    (U.READY, E.DISPATCH): (U.RUNNING, "start_session"),
    (U.READY, E.DISPATCH_DIRECTOR): (U.DIRECTING, "start_session"),
    (U.READY, E.DISPATCH_REFUSED): (U.HELD, "park_refused"),
    (U.READY, E.SPAWN_FAILED): (U.READY, "strike"),
    (U.READY, E.STRIKE_LIMIT): (U.HELD, "park_struck"),
    (U.READY, E.OPERATOR_STOP): (U.READY, None),

    (U.RUNNING, E.RESULT_DONE): (U.MERGING, "accept_done"),
    (U.RUNNING, E.RESULT_CONTINUE): (U.READY, "continue_"),
    (U.RUNNING, E.RESULT_AWAIT): (U.AWAITING, "start_await"),
    (U.RUNNING, E.AWAIT_FAILED): (U.READY, "strike"),
    (U.RUNNING, E.RESULT_NEEDS_HUMAN): (U.HELD, "park_asked"),
    (U.RUNNING, E.RESULT_NEEDS_DIRECTOR): (U.READY, "escalate"),
    (U.RUNNING, E.NO_RESULT_COMMITS): (U.READY, "continue_"),
    (U.RUNNING, E.NO_RESULT_NONE): (U.READY, "strike"),
    (U.RUNNING, E.OPERATOR_STOP): (U.READY, "interrupted"),

    (U.DIRECTING, E.RESULT_DONE): (U.MERGING, "accept_done"),
    (U.DIRECTING, E.RESULT_CONTINUE): (U.READY, "director_cleared"),
    (U.DIRECTING, E.RESULT_AWAIT): (U.AWAITING, "start_await"),
    (U.DIRECTING, E.AWAIT_FAILED): (U.HELD, "park_director"),
    (U.DIRECTING, E.RESULT_NEEDS_HUMAN): (U.HELD, "park_asked"),
    (U.DIRECTING, E.NO_RESULT_COMMITS): (U.HELD, "park_director"),
    (U.DIRECTING, E.NO_RESULT_NONE): (U.HELD, "park_director"),
    (U.DIRECTING, E.OPERATOR_STOP): (U.READY, "interrupted"),

    (U.AWAITING, E.RUN_ENDED): (U.READY, "run_ended"),
    (U.AWAITING, E.RUN_OVER_BUDGET): (U.READY, "run_over_budget"),
    # The run is the unit's, not the session's: a stop leaves it running and
    # the next start watches it again (its pid, start time and exit file are
    # in the ledger).
    (U.AWAITING, E.OPERATOR_STOP): (U.AWAITING, None),

    (U.MERGING, E.MERGE_CLEAN): (U.DONE, None),
    (U.MERGING, E.MERGE_CONFLICT): (U.READY, "strike"),
    (U.MERGING, E.MERGE_UNCOMMITTED): (U.HELD, "park_unmarked"),
    # A merge runs inside one tick; a stop lets it finish.
    (U.MERGING, E.OPERATOR_STOP): (U.MERGING, None),

    (U.HELD, E.UNHOLD): (U.READY, "unhold"),
    (U.HELD, E.ROW_CLOSED): (U.DONE, None),
    (U.HELD, E.OPERATOR_STOP): (U.HELD, None),

    (U.DONE, E.ROW_REOPENED): (U.READY, None),
    (U.DONE, E.OPERATOR_STOP): (U.DONE, None),
}

# Every pair the table leaves out, with the reason it cannot occur. The
# totality test holds the two together: each (state, event) is in exactly one.
_NO_SESSION = "only a session the loop holds reports a result or ends without one"
_NO_RUN = "only an awaiting unit holds a background run"
_NO_MERGE = "only a merging unit is merged"
_NOT_READY = "the loop dispatches, refuses and escalates only ready units"
_ACTIVE_STATIC = ("the queue is re-read for a unit only when it holds no process; "
                  "its session or run decides, and the row is read again when it leaves")


def _impossible():
    out = {}
    result_events = {E.RESULT_DONE, E.RESULT_CONTINUE, E.RESULT_AWAIT, E.AWAIT_FAILED,
                     E.RESULT_NEEDS_HUMAN, E.RESULT_NEEDS_DIRECTOR, E.NO_RESULT_COMMITS,
                     E.NO_RESULT_NONE}
    dispatch_events = {E.DISPATCH, E.DISPATCH_DIRECTOR, E.DISPATCH_REFUSED, E.SPAWN_FAILED,
                       E.STRIKE_LIMIT}
    for state in U:
        for event in E:
            if (state, event) in TRANSITIONS:
                continue
            if event in result_events:
                out[(state, event)] = _NO_SESSION
            elif event in (E.RUN_ENDED, E.RUN_OVER_BUDGET):
                out[(state, event)] = _NO_RUN
            elif event in (E.MERGE_CLEAN, E.MERGE_CONFLICT, E.MERGE_UNCOMMITTED):
                out[(state, event)] = _NO_MERGE
            elif event in dispatch_events:
                out[(state, event)] = _NOT_READY
            elif state in ACTIVE:
                out[(state, event)] = _ACTIVE_STATIC
    out[(U.DIRECTING, E.RESULT_NEEDS_DIRECTOR)] = (
        "a director's needs-human goes to the operator: classify() never escalates a "
        "director to itself")
    out[(U.PENDING, E.DEPS_UNMET)] = "a pending unit's dependencies are already unmet"
    out[(U.READY, E.DEPS_DONE)] = "a ready unit's dependencies are already done"
    for state in (U.PENDING, U.READY):
        out[(state, E.UNHOLD)] = "only a held unit is released"
        out[(state, E.ROW_REOPENED)] = "only a done row is reopened"
    out[(U.HELD, E.HOLD)] = "a held unit is already held"
    for event in (E.DEPS_DONE, E.DEPS_UNMET):
        out[(U.HELD, event)] = ("held takes precedence over dependencies; they are read "
                                "again when the unit is released")
    out[(U.HELD, E.ROW_REOPENED)] = "only a done row is reopened"
    for event in (E.DEPS_DONE, E.DEPS_UNMET, E.HOLD, E.UNHOLD, E.ROW_CLOSED):
        out[(U.DONE, event)] = ("a done row is read only for being reopened; the rest "
                                "follows from READY")
    return out


IMPOSSIBLE = _impossible()

# How each state that waits is left, and what kind of signal that is. "owned":
# the loop produces it (a process it started ending, its clock against a budget
# it recorded, its own git). "operator": an operator act. No waiting state is
# left by a file an agent writes, and none by a clock alone.
WAITING_EXITS = {
    U.PENDING: ("owned", "a dependency's own transition to DONE"),
    U.READY: ("owned", "dispatch, once the one precondition guard passes"),
    U.RUNNING: ("owned", "the session's process ends; the loop kills it at its timeout"),
    U.DIRECTING: ("owned", "the session's process ends; the loop kills it at its timeout"),
    U.AWAITING: ("owned", "the run's process ends, or its budget passes and the loop kills it"),
    U.MERGING: ("owned", "the loop's merge, run when the main tree is free; the main tree's "
                         "holder is itself a RUNNING, DIRECTING or AWAITING unit"),
    U.HELD: ("operator", "unpark (delete the parked file), rescope, or mark the row [x]"),
}


def static_path(frm, to):
    """The STATIC_EVENTS that walk a unit holding no process from one state
    to another, shortest first; the table decides which steps exist."""
    if frm == to:
        return []
    frontier, seen = [(frm, [])], {frm}
    while frontier:
        state, path = frontier.pop(0)
        for event in E:
            if event not in STATIC_EVENTS or (state, event) not in TRANSITIONS:
                continue
            nxt = TRANSITIONS[(state, event)][0]
            if nxt == to:
                return path + [event]
            if nxt not in seen:
                seen.add(nxt)
                frontier.append((nxt, path + [event]))
    raise ValueError(f"no static path {frm.value} -> {to.value}")


class Pool(enum.Enum):
    """The loop's own state, read from its units each tick."""
    RUNNING = "running"        # a session runs, or a merge is landing
    IDLE = "idle"              # nothing is ready; background runs are bounded by their budgets
    BLOCKED = "blocked"        # a unit is ready and a precondition fails
    STUCK = "stuck"            # nothing ready, running or awaiting; units are held
    DRAINING = "draining"      # a drain stop: running sessions finish, nothing new starts
    DONE = "done"
    STOPPED = "stopped"
# ---------------------------------------------------------------------------
# Processes the loop starts. A session or a background run is recorded by its
# pid AND its start time, so a pid reused after a reboot is not mistaken for
# the run; both run in their own process group, so the loop can end them with
# everything they started; and both survive the loop's own exit or re-exec,
# because the ledger lets the next generation watch them again.


@dataclasses.dataclass(frozen=True)
class Proc:
    pid: int
    started: str               # `ps -o lstart=`; "" when ps could not say


def _ps(pids):
    """{pid: (lstart, stat)} for the pids that exist, or None when ps itself
    failed: a failed look is not evidence that a process is gone."""
    if not pids:
        return {}
    try:
        r = subprocess.run(["ps", "-o", "pid=,stat=,lstart=", "-p", ",".join(map(str, pids))],
                           capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return None
    if r.returncode not in (0, 1):     # 1: none of the pids exists
        return None
    out = {}
    for line in r.stdout.splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3 and parts[0].isdigit():
            out[int(parts[0])] = (" ".join(parts[2].split()), parts[1])
    return out


def proc_start(pid):
    found = _ps([pid]) or {}
    return found.get(pid, ("", ""))[0]


# The children this process started, so it reaps them itself; after a re-exec
# or a restart the pids are watched through ps instead.
_CHILDREN = {}


def proc_alive(proc):
    """True while the process runs, False once it has ended (or its pid now
    names another process), None when that cannot be told this tick."""
    child = _CHILDREN.get(proc.pid)
    if child is not None:
        if child.poll() is None:
            return True
        _CHILDREN.pop(proc.pid, None)
        return False
    try:
        pid, _ = os.waitpid(proc.pid, os.WNOHANG)       # a child from before a re-exec
        if pid:
            return False
    except ChildProcessError:
        pass
    except OSError:
        return None
    found = _ps([proc.pid])
    if found is None:
        return None
    if proc.pid not in found:
        return False
    lstart, stat = found[proc.pid]
    if stat.startswith("Z"):
        return False
    return not proc.started or lstart == proc.started


def proc_kill(proc, grace=5):
    """End the process group: SIGTERM, then SIGKILL after `grace` seconds. A
    process that is no longer the one recorded is left alone."""
    if proc_alive(proc) is not True:
        return
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(proc.pid, sig)
        except (ProcessLookupError, PermissionError):
            return
        deadline = time.time() + grace
        while time.time() < deadline:
            if proc_alive(proc) is not True:
                return
            time.sleep(0.2)


def proc_spawn(argv, *, cwd, env, log_path, append=False):
    """Start `argv` in a process group of its own, its output to `log_path`.
    Raises OSError when it cannot start."""
    log = pathlib.Path(log_path)
    log.parent.mkdir(parents=True, exist_ok=True)
    with open(log, "a" if append else "w") as fh:
        child = subprocess.Popen(argv, cwd=str(cwd), env=env, stdout=fh,
                                 stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                 start_new_session=True)
    _CHILDREN[child.pid] = child
    return Proc(child.pid, proc_start(child.pid))


# A background run's wrapper: the command runs in the foreground of a shell
# the loop started, and the shell records the exit status where only the loop
# reads it. A run that leaves no status ended without one (a reboot, a kill).
AWAIT_SH = ('"$@"; rc=$?; printf "%s\\n" "$rc" > "$RALPH_EXIT.tmp" '
            '&& mv "$RALPH_EXIT.tmp" "$RALPH_EXIT"; exit "$rc"')

DURATION_RE = re.compile(r"^(\d+)([smhd]?)$")
DURATION_UNITS = {"": 1, "s": 1, "m": 60, "h": 3600, "d": 86400}


def parse_duration(text):
    m = DURATION_RE.match((text or "").strip())
    return int(m[1]) * DURATION_UNITS[m[2]] if m else None


def fmt_secs(secs):
    secs = int(secs)
    if secs >= 3600:
        return f"{secs // 3600}h{(secs % 3600) // 60:02d}m"
    if secs >= 60:
        return f"{secs // 60}m{secs % 60:02d}s"
    return f"{secs}s"


# ---------------------------------------------------------------------------
# The ledger: the loop's own state, in one file only the loop writes, outside
# every worktree. The queue holds what the operator owns (rows, dependencies,
# [x]); the ledger holds what the loop started and its counters.


class Ledger:
    def __init__(self, path):
        self.path = pathlib.Path(path)
        self.units = {}
        self.notified = {}
        self.fresh = not self.path.exists()
        if not self.fresh:
            data = json.loads(self.path.read_text())
            self.units = data.get("units", {})
            self.notified = data.get("notified", {})

    def unit(self, unit_id):
        return self.units.setdefault(unit_id, {"strikes": 0, "continuations": 0,
                                               "directed": 0, "sessions": 0, "notes": []})

    def state(self, unit_id):
        raw = self.units.get(unit_id, {}).get("state")
        return U(raw) if raw else None

    def save(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        tmp.write_text(json.dumps({"units": self.units, "notified": self.notified},
                                  indent=1, sort_keys=True) + "\n")
        tmp.replace(self.path)
        self.fresh = False


# ---------------------------------------------------------------------------
# The session's one verb. Every session the loop starts is told how it ends
# (CONTRACT), with `ralph-result` on its PATH (scripts/ralph-bin/) and the file
# its result goes to in RALPH_RESULT. The command validates what it is given,
# so a session sees a refusal while it can still act on it.

RESULT_KINDS = ("done", "continue", "await", "needs-human")
AWAIT_MAX_S = 7 * 86400
RALPH_BIN = pathlib.Path(__file__).resolve().parent / "ralph-bin"

CONTRACT = """\
HOW THIS SESSION ENDS. End with exactly one `ralph-result` call; the loop reads nothing else
you write (no marker, waiting or package file), and a session that ends without one is
counted by its commits alone.
  ralph-result done                       the unit is finished and committed (a dirty tree is refused)
  ralph-result continue [note]            progress is committed and more remains; the note goes
                                          to the next session
  ralph-result await <budget> -- <cmd>    a long run (a field run, a battery, a release cut): the
                                          loop starts <cmd> in this worktree, logs it, and resumes
                                          this unit with its exit code and log when it ends, or
                                          kills it after <budget> (90m, 6h, 2d)
  ralph-result needs-human [--package FILE] <why>
                                          a decision the row and the design do not make; FILE holds
                                          the evidence (commands, their actual output, file:line)
Run <cmd> in the foreground of that call: never start a process yourself with nohup or `&`,
since it dies with this session or outlives it unseen.
"""
CONTRACT_OPERATOR = """\
  ralph-result needs-human --operator ... a fork the charter leaves to the operator: no director
                                          is sent
"""


def _git_out(cwd, *args):
    r = subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else None


def tree_changes(cwd, base_untracked=()):
    """What `done` refuses: tracked changes, and untracked files the session
    added (files untracked before it started are not its own)."""
    out = _git_out(cwd, "status", "--porcelain", "--untracked-files=all")
    if out is None:
        return ["git status failed"]
    before = set(base_untracked)
    dirty = []
    for line in out.splitlines():
        if line.startswith("?? ") and line[3:] in before:
            continue
        dirty.append(line)
    return dirty


def untracked_files(cwd):
    out = _git_out(cwd, "status", "--porcelain", "--untracked-files=all") or ""
    return [line[3:] for line in out.splitlines() if line.startswith("?? ")]


def result_refusal(kind, args, env, cwd):
    """None when the result stands, else what to fix."""
    if kind == "done":
        base = {}
        try:
            base = json.loads(pathlib.Path(env.get("RALPH_BASE", "")).read_text())
        except (OSError, ValueError):
            pass
        dirty = tree_changes(cwd, base.get("untracked", ()))
        if dirty:
            return ("the tree is not clean — commit your work (or remove what is not yours "
                    "to keep), then report done:\n  " + "\n  ".join(dirty[:20]))
    elif kind == "await":
        budget = parse_duration(args.budget)
        cap = int(env.get("RALPH_AWAIT_MAX") or AWAIT_MAX_S)
        if not budget:
            return f"budget {args.budget!r} is not a duration (90m, 6h, 2d, or seconds)"
        if budget > cap:
            return f"budget {fmt_secs(budget)} exceeds this loop's cap of {fmt_secs(cap)}"
        argv = list(args.argv or [])
        if argv and argv[0] == "--":
            argv = argv[1:]
        if not argv:
            return "no command: ralph-result await <budget> -- <command> [args...]"
        exe = argv[0]
        if not (shutil.which(exe) or (pathlib.Path(cwd) / exe).exists()
                or pathlib.Path(exe).is_absolute() and pathlib.Path(exe).exists()):
            return f"{exe!r} is neither on PATH nor a file in {cwd}"
    elif kind == "needs-human":
        if not " ".join(args.why or []).strip():
            return "say why: ralph-result needs-human [--package FILE] <why>"
        if args.package and not (pathlib.Path(cwd) / args.package).is_file():
            return f"--package {args.package}: no such file"
    return None


def cmd_result(args):
    env = os.environ
    target = env.get("RALPH_RESULT", "")
    if not target:
        print("ralph-result: not inside a ralph session (RALPH_RESULT is unset)", file=sys.stderr)
        return 2
    path = pathlib.Path(target)
    if path.exists():
        print(f"ralph-result: this session already reported "
              f"({json.loads(path.read_text()).get('kind')}); one result per session",
              file=sys.stderr)
        return 2
    cwd = pathlib.Path(env.get("RALPH_WORKSPACE") or os.getcwd())
    refusal = result_refusal(args.kind, args, env, cwd)
    if refusal:
        print(f"ralph-result {args.kind}: refused — {refusal}", file=sys.stderr)
        return 2
    rec = {"kind": args.kind, "at": int(time.time()),
           "head": (_git_out(cwd, "rev-parse", "HEAD") or "").strip()}
    if args.kind == "continue":
        rec["note"] = " ".join(args.note or []).strip()
    elif args.kind == "await":
        argv = list(args.argv)
        rec.update(budget_s=parse_duration(args.budget), argv=argv[1:] if argv[0] == "--" else argv)
    elif args.kind == "needs-human":
        rec.update(why=" ".join(args.why).strip(), operator=bool(args.operator),
                   package=(cwd / args.package).read_text(errors="replace")
                   if args.package else "")
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(rec, indent=1) + "\n")
    tmp.replace(path)
    print(f"ralph-result: {args.kind} recorded — end your turn now")
    return 0
# ---------------------------------------------------------------------------
# The loop.

TICK_S = 30
DEFAULT_SESSION_TIMEOUT = 3600
MAX_STRIKES = 3
DIRECTOR_MAX = 2                # director sessions per unit, reset when it is done or unparked
PROBE_OK_TTL_S = 600            # a model that answered is not probed again for this long
PROBE_RETRY_S = 300             # nor one that did not
# The one precondition guard: how long each may fail before the operator is
# told. Disk and the queue cannot heal without someone, so they say so at once.
PRECONDITION_NOTIFY_AFTER = {"queue": 0, "prompt": 0, "disk": 0, "memory": 1800, "model": 1800}
REBLOCK_LOG_S = 600
LANE_DONE_DIR = "ralph/lanes"   # the file protocol's lane marker; read only by adopt_legacy


class SpawnError(Exception):
    """The loop could not start a session or prepare its worktree."""


class Terminated(Exception):
    def __init__(self, signum):
        super().__init__(signum)
        self.signum = signum


class Loop:
    """The one driver. Serial (`run`) is one lane whose workspace is the main
    tree; parallel (`pool`) runs units in worktrees beside it and reviews in
    the main tree, alone. Each tick observes what the loop started, applies
    the queue's edits, lands finished units, and dispatches; every change of a
    unit's state goes through `fire`, which refuses a pair the table does not
    define."""

    def __init__(self, paths, *, label, lanes=1, lane_mode=False, base_branch="",
                 session_timeout=DEFAULT_SESSION_TIMEOUT, max_strikes=MAX_STRIKES,
                 max_continuations=MAX_LANE_CONTINUATIONS, director_max=DIRECTOR_MAX,
                 await_max_s=AWAIT_MAX_S, models=None, charter=None, notify_enabled=True,
                 notifier=notify, state_dir=None, lane_root=None, spawn=proc_spawn,
                 alive=proc_alive, kill=proc_kill, clock=time.time, sleep=time.sleep,
                 tick_s=TICK_S, probe=None, jobs_share=None, disk_free_gb=None,
                 disk_floor_gb=DISK_FLOOR_GB, code_digest=None, compiles=None, committed=None,
                 reexec=None, argv=None):
        self.paths = paths
        self.label = label
        self.lanes = lanes
        self.lane_mode = lane_mode
        self.session_timeout = session_timeout
        self.max_strikes = max_strikes
        self.max_continuations = max_continuations
        self.director_max = director_max
        self.await_max_s = await_max_s
        self.models = {k: "" for k in MODEL_KEYS} | dict(models or {})
        self.charter = charter
        self.notifier = notifier
        self.notify_enabled = notify_enabled
        self.state_dir = pathlib.Path(state_dir) if state_dir else state_dir_for(paths, label)
        self.ledger = Ledger(self.state_dir / "loop.json")
        self.spawn, self.alive, self.kill = spawn, alive, kill
        self.clock, self.sleep, self.tick_s = clock, sleep, tick_s
        self.probe = probe or (lambda model: probe_model(model, paths))
        self.jobs_share = jobs_share or cargo_jobs_share
        root = pathlib.Path(lane_root) if lane_root else lane_root_for(paths.workdir)
        self.disk_root = root if lane_mode else paths.workdir
        self.disk_free_gb = disk_free_gb or (
            lambda: shutil.disk_usage(self.disk_root if self.disk_root.exists()
                                      else paths.workdir).free // 2**30)
        self.disk_floor_gb = disk_floor_gb
        self.mech = Lanes(paths, lane_root=root, base_branch=base_branch,
                          disk_free_gb=lambda: self.disk_free_gb(), disk_floor_gb=disk_floor_gb)
        self.code_digest = code_digest or (lambda: hashlib.sha256(SELF.read_bytes()).hexdigest())
        self.compiles = compiles or (lambda: compile(SELF.read_text(), str(SELF), "exec"))
        # The launch line runs the working tree's file, so a half-made edit that
        # happens to compile would go live: only committed code is deployed.
        self.committed = committed or (lambda: subprocess.run(
            ["git", "-C", str(SELF.parent), "diff", "--quiet", "HEAD", "--", SELF.name],
            capture_output=True).returncode == 0)
        self.reexec = reexec or (lambda argv: os.execv(argv[0], argv))
        self.argv = argv or [sys.executable, str(SELF), *sys.argv[1:]]
        self.static = {}
        self._loaded = self.code_digest()
        self._refused = self._uncommitted = None
        self._probes = {}
        self._jobs = None
        self._blocked = None          # (precondition, why, since, last logged)
        self._prompt = None           # (text, source) read once per tick
        self._errors = set()

    # -- the table ----------------------------------------------------------

    def state(self, unit):
        return self.ledger.state(unit) or self.static.get(unit)

    def fire(self, unit, event, **ctx):
        frm = self.state(unit)
        if (frm, event) not in TRANSITIONS:
            reason = IMPOSSIBLE.get((frm, event), "not in the table")
            raise RuntimeError(f"unit {unit}: {event.value} in {frm.value if frm else None} "
                               f"— {reason}")
        to, action = TRANSITIONS[(frm, event)]
        entry = self.ledger.unit(unit)
        detail = getattr(self, f"_do_{action}")(unit, entry, **ctx) if action else None
        if to in ACTIVE:
            entry["state"] = to.value
            self.static.pop(unit, None)
        else:
            entry.pop("state", None)
            self.static[unit] = to
        say(f"unit {unit}: {frm.value} --{event.value}--> {to.value}"
            + (f" · {detail}" if detail else ""))
        self.ledger.save()

    def units_in(self, *states):
        return [u for u in self.ledger.units if self.ledger.state(u) in states]

    # -- the process --------------------------------------------------------

    def run(self):
        def term(signum, _frame):
            raise Terminated(signum)
        signal.signal(signal.SIGTERM, term)
        signal.signal(signal.SIGINT, term)
        try:
            self.boot()
            while True:
                try:
                    pool = self.tick()
                except Terminated:
                    raise
                except Exception as e:  # noqa: BLE001 — the tick's net; the loop stays up
                    self._tick_error(e)
                    pool = None
                if pool in (Pool.DONE, Pool.STOPPED):
                    return 0
                self.sleep(self.tick_s)
        except Terminated as t:
            say(f"loop: signal {t.signum} — ending the sessions it holds; background runs "
                "keep running and the next start watches them")
            self._end_sessions()
            return 128 + t.signum

    def boot(self):
        if self.lane_mode:
            self.mech.lane_root.mkdir(parents=True, exist_ok=True)
        self.paths.p(self.paths.control_dir).mkdir(parents=True, exist_ok=True)
        if self.ledger.fresh:
            for line in adopt_legacy(self, dry=False):
                say(f"adopt: {line}")
        self.ledger.save()
        say(f"loop: {'pool' if self.lane_mode else 'serial'} lanes={self.lanes} "
            f"queue={self.paths.queue or self.paths.state} ledger={self.ledger.path}"
            + (f" lane_root={self.mech.lane_root}" if self.lane_mode else ""))

    def tick(self):
        self._maybe_reexec()
        self._prompt = None
        queue, unreadable = self._read_queue()
        self._observe_sessions()
        self._observe_runs()
        if queue is None:
            return self._beat(self._block("queue", unreadable))
        self._reconcile(queue)
        if self.units_in(U.MERGING):
            self._merge()
            queue, unreadable = self._read_queue()
            if queue is None:
                return self._beat(self._block("queue", unreadable))
            self._reconcile(queue)          # what the merges closed readies its dependents
        mode = self._stop_mode()
        if mode == "now":
            return self._beat(self._stop_now())
        if mode == "drain":
            return self._beat(self._drain())
        if self._finish(queue):
            return self._beat(Pool.DONE)
        blocked = self._dispatch(queue)
        if blocked:
            return self._beat(self._block(*blocked))
        self._unblock()
        return self._beat(self._pool_state(queue))

    def _read_queue(self):
        try:
            return Queue(self.paths.p(self.paths.state)), ""
        except (OSError, ValueError) as e:
            return None, f"{self.paths.state} does not parse: {e}"

    def _beat(self, pool):
        sessions = len(self.units_in(*SESSION))
        awaiting = len(self.units_in(U.AWAITING))
        try:
            self.paths.p(self.paths.heartbeat).write_text(
                f"{int(time.time())} {pool.value} sessions={sessions} awaiting={awaiting}\n")
        except OSError:
            pass
        return pool

    def _notify_once(self, key, title, body):
        if self.ledger.notified.get(key):
            return
        self.ledger.notified[key] = int(self.clock())
        self.ledger.save()
        self.notifier(title, body, self.notify_enabled)

    def _tick_error(self, e):
        sig = f"{type(e).__name__}: {e}"
        say(f"loop: tick error — {sig}")
        for line in traceback.format_exc().rstrip().splitlines()[-6:]:
            say(f"  {line}")
        try:
            self.paths.p(self.paths.heartbeat).write_text(f"{int(time.time())} error {sig[:200]}\n")
        except OSError:
            pass
        if sig not in self._errors:
            self._errors.add(sig)
            self.notifier("OPERATOR — loop error, still running", sig[:200], self.notify_enabled)

    def _maybe_reexec(self):
        """Deploy committed fixes: hand the process to the file on disk when it
        changed, is committed and compiles. Sessions and runs carry across in
        the ledger, so this needs no quiet moment."""
        try:
            now = self.code_digest()
        except OSError:
            return
        if now in (self._loaded, self._refused):
            return
        if not self.committed():
            if now != self._uncommitted:
                self._uncommitted = now
                say(f"loop: {SELF.name} changed but holds uncommitted edits — staying on the "
                    "loaded code")
            return
        try:
            self.compiles()
        except SyntaxError as e:
            self._refused = now
            say(f"loop: {SELF.name} changed but does not compile ({e.msg}, line {e.lineno}) — "
                "staying on the loaded code")
            return
        self.ledger.save()
        say(f"loop: {SELF.name} changed ({self._loaded[:12]} → {now[:12]}) — re-exec; "
            f"{len(self.units_in(*SESSION))} session(s) and {len(self.units_in(U.AWAITING))} "
            "run(s) carry across")
        sys.stdout.flush()
        sys.stderr.flush()
        self.reexec(self.argv)

    # -- observe what the loop started --------------------------------------

    def _observe_sessions(self):
        now = self.clock()
        for unit in self.units_in(*SESSION):
            entry = self.ledger.units[unit]
            s = entry["session"]
            proc = Proc(s["pid"], s["started"])
            alive = self.alive(proc)
            if alive is None:
                continue
            if alive and now >= s["deadline"]:
                say(f"unit {unit}: session {s['n']} passed its {fmt_secs(self.session_timeout)} "
                    "timeout — ending its process group")
                self.notifier("auto — session timeout", f"{unit}: killed at "
                              f"{fmt_secs(self.session_timeout)}", self.notify_enabled)
                self.kill(proc)
                alive = self.alive(proc)
            if alive is False:
                self._session_ended(unit, entry)

    def _session_ended(self, unit, entry):
        s = entry["session"]
        try:
            rejects = pathlib.Path(s["log"]).read_text(errors="replace").count("auto-rejecting")
        except OSError:
            rejects = 0
        if rejects:
            # The client ends a session on a refused tool call (opencode, 10 of the
            # 29 sub-2-minute ersilia ends, 2026-09-17 → 10-04): a configuration
            # fault that repeats, so the strike it costs names it.
            manifest = self.paths.manifest
            perms = manifest.settings if manifest and manifest.settings else "opencode.json"
            s["rejects"] = f"{rejects} permission auto-rejection(s) in its log — extend {perms}"
            say(f"unit {unit}: {s['rejects']}")
            self.notifier("auto — permission rejects", f"{unit}: {rejects} auto-rejections",
                          self.notify_enabled)
        event, ctx = self.classify(unit, entry)
        if event is E.RESULT_AWAIT:
            try:
                ctx["run"] = self._start_run(unit, entry, ctx["result"])
            except (OSError, SpawnError) as e:
                event, ctx = E.AWAIT_FAILED, {"why": f"the loop could not start "
                                                     f"{ctx['result']['argv']}: {e}"}
        self.fire(unit, event, **ctx)

    def classify(self, unit, entry):
        """A session's end as one event: its result, else whether it left
        commits. Nothing else about the session is read."""
        s = entry["session"]
        try:
            res = json.loads(pathlib.Path(s["result"]).read_text())
        except (OSError, ValueError):
            res = None
        if res and res.get("kind") in RESULT_KINDS:
            kind = res["kind"]
            if kind == "done":
                return E.RESULT_DONE, {"result": res}
            if kind == "continue":
                return E.RESULT_CONTINUE, {"result": res}
            if kind == "await":
                return E.RESULT_AWAIT, {"result": res}
            if (s["role"] == "worker" and not res.get("operator")
                    and self._director_left(entry)):
                return E.RESULT_NEEDS_DIRECTOR, {"result": res}
            return E.RESULT_NEEDS_HUMAN, {"result": res}
        made = self._commits_since(s["workspace"], s["base"])
        return (E.NO_RESULT_COMMITS if made else E.NO_RESULT_NONE), {"commits": made}

    def _commits_since(self, workspace, base):
        if not base:
            return 0
        out = _git_out(workspace, "rev-list", "--count", f"{base}..HEAD")
        return int(out.strip()) if out and out.strip().isdigit() else 0

    def _observe_runs(self):
        now = self.clock()
        for unit in self.units_in(U.AWAITING):
            entry = self.ledger.units[unit]
            r = entry["run"]
            proc = Proc(r["pid"], r["started"])
            code = self._exit_code(r)
            if code is None:
                alive = self.alive(proc)
                if alive is None:
                    continue
                if alive and now < r["deadline"]:
                    continue
                if alive:
                    say(f"unit {unit}: background run passed its {fmt_secs(r['budget_s'])} "
                        "budget — ending its process group")
                    self.kill(proc)
                    self.fire(unit, E.RUN_OVER_BUDGET)
                    continue
                code = self._exit_code(r)       # it may have written its status as it ended
            else:
                self.alive(proc)                # reap it
            self.fire(unit, E.RUN_ENDED, code=code)

    @staticmethod
    def _exit_code(r):
        try:
            return int(pathlib.Path(r["exit_file"]).read_text().strip())
        except (OSError, ValueError):
            return None

    # -- the queue's edits --------------------------------------------------

    def _held(self, queue):
        return held_ids(self.paths, queue) | {r.id for r in queue.rows
                                              if r.id.startswith("HUMAN-")}

    def _target(self, row, queue, held):
        if row.status is Status.DONE:
            return U.DONE
        if row.id in held:
            return U.HELD
        return U.READY if queue.deps_met(row) else U.PENDING

    def _reconcile(self, queue):
        rows = queue.by_id()
        for unit in list(self.ledger.units):
            if unit not in rows and self.ledger.state(unit) not in ACTIVE:
                say(f"unit {unit}: no longer a row in {self.paths.state} — forgotten")
                self.ledger.units.pop(unit)
                self.static.pop(unit, None)
                self.ledger.save()
        held = self._held(queue)
        for row in queue.rows:
            if self.ledger.state(row.id) in ACTIVE:
                continue
            target = self._target(row, queue, held)
            if row.id not in self.static:
                parked = self.ledger.units.get(row.id, {}).get("parked")
                self.static[row.id] = U.HELD if parked else target
            for event in static_path(self.static[row.id], target):
                self.fire(row.id, event)
            if target is U.DONE and row.id in self.ledger.units:
                self.ledger.units.pop(row.id)
                self.ledger.save()

    # -- landing finished units ---------------------------------------------

    def _main_busy(self):
        """A main-tree unit holds the tree while its session or run lives:
        nothing else commits into it until that owned process ends."""
        return any(self.ledger.units[u].get("main")
                   for u in self.units_in(U.RUNNING, U.DIRECTING, U.AWAITING))

    def _merge(self):
        for unit in self.units_in(U.MERGING):
            if self._main_busy():
                return
            entry = self.ledger.units[unit]
            if entry.get("main"):
                ok, why = self._mark_done(unit, entry.get("done_head", ""), stage_only=False)
                self.fire(unit, E.MERGE_CLEAN if ok else E.MERGE_UNCOMMITTED, why=why)
                continue
            self._land_lane(unit, entry)

    def _land_lane(self, unit, entry):
        wt, branch = self.mech.lane_root / unit, f"ralph/{unit}"
        git = self.mech.git
        if git("merge-base", "--is-ancestor", branch, "HEAD").returncode != 0:
            refused = self.mech.renumber_decisions(unit, wt, branch)
            if refused is not None:
                self.fire(unit, E.MERGE_CONFLICT, why=refused)
                return
            say(f"unit {unit}: merging {branch}")
            r = git("merge", "--no-ff", "-m", f"merge {unit}", branch)
            if r.returncode != 0:
                git("merge", "--abort")
                self.fire(unit, E.MERGE_CONFLICT,
                          why=f"merging {branch}: {error_tail(r.stderr or r.stdout) or 'refused'}"
                              f" — the lane merges {self.mech.base_branch} in and resolves it")
                return
        tip = (git("rev-parse", "--short", branch).stdout or "").strip()
        rendered = self.mech.regenerate_decisions(unit)
        ok, why = self._mark_done(unit, tip, stage_only=True)
        if not ok:
            self.fire(unit, E.MERGE_UNCOMMITTED, why=why)
            return
        if rendered is not None:
            say(f"unit {unit}: {rendered}")
            self._notify_once(f"decisions:{unit}", "OPERATOR — decisions render failed", rendered)
        if self.mech.keep_evidence(unit, wt):
            self.mech.remove_lane(unit, wt)
            git("branch", "-D", branch)
        else:
            say(f"unit {unit}: worktree {wt} kept for its evidence")
        self.fire(unit, E.MERGE_CLEAN, why=f"merged {branch} at {tip}")

    def _mark_done(self, unit, sha, *, stage_only):
        """Mark the row [x] with its commit and commit the queue: the subject
        `ralph: <id> done` is what units_since_audit counts. (ok, why)."""
        try:
            queue = Queue(self.paths.p(self.paths.state))
            row = queue.by_id().get(unit)
            if row is None:
                return False, f"{unit} is no longer a row in {self.paths.state}"
            if row.status is not Status.DONE:
                queue.mark_done(unit, sha)
        except (OSError, ValueError) as e:
            return False, f"the [x] mark failed: {e}"
        git = self.mech.git
        subject = f"ralph: {unit} done"
        if stage_only:
            git("add", "--", self.paths.state)
            if git("diff", "--cached", "--quiet").returncode == 0:
                return True, "already marked"
            r = git("commit", "-q", "-m", subject)
        else:
            if git("diff", "--quiet", "HEAD", "--", self.paths.state).returncode == 0:
                return True, "already marked"
            r = commit_state(self.paths, subject)
        if r.returncode != 0:
            return False, ("the done commit failed — the index holds the mark; commit it, "
                           "then mark the row [x] or unpark it: "
                           + (error_tail(r.stderr or r.stdout) or f"exit {r.returncode}"))
        return True, f"marked [x] {sha}".rstrip()

    # -- stop, finish -------------------------------------------------------

    def _stop_mode(self):
        stop = self.paths.p(self.paths.stop)
        if not stop.exists():
            return None
        try:
            text = stop.read_text().strip()
        except OSError:
            return "now"
        return "drain" if text == "drain" else "now"

    def _end_sessions(self):
        for unit in self.units_in(*SESSION):
            entry = self.ledger.units[unit]
            s = entry["session"]
            self.kill(Proc(s["pid"], s["started"]))
            event, ctx = self.classify(unit, entry)
            if event in (E.NO_RESULT_COMMITS, E.NO_RESULT_NONE, E.RESULT_AWAIT):
                event, ctx = E.OPERATOR_STOP, {}
            self.fire(unit, event, **ctx)
        for unit in self.units_in(U.AWAITING):
            self.fire(unit, E.OPERATOR_STOP)

    def _stop_now(self):
        self._end_sessions()
        say(f"loop: operator stop — {self.paths.stop}")
        self.notifier("stopped — operator stop", self.paths.stop, self.notify_enabled)
        return Pool.STOPPED

    def _drain(self):
        if self.units_in(*SESSION) or self.units_in(U.MERGING):
            if not self.ledger.notified.get("draining"):
                self.ledger.notified["draining"] = int(self.clock())
                say(f"loop: draining — {len(self.units_in(*SESSION))} session(s) finish, "
                    "nothing new starts")
            return Pool.DRAINING
        self.ledger.notified.pop("draining", None)
        for unit in self.units_in(U.AWAITING):
            self.fire(unit, E.OPERATOR_STOP)
        # Drained: the stop stands as an operator stop.
        self.paths.p(self.paths.stop).write_text("")
        say("loop: drained — stopped")
        self.notifier("stopped — drained", self.paths.stop, self.notify_enabled)
        return Pool.STOPPED

    def _finish(self, queue):
        if not queue.rows or not queue.all_done() or self.units_in(*ACTIVE):
            return False
        if closing_audit_due(self.paths, queue):
            unit, refused = insert_audit(self.paths, queue, None)
            if refused:
                say(f"loop: audit row {unit.id} is in {self.paths.state} but git refused the "
                    f"commit: {refused}")
            return False
        self.paths.p(self.paths.control_dir).mkdir(parents=True, exist_ok=True)
        self.paths.p(self.paths.done).write_text("")
        say(f"loop: DONE — every row in {self.paths.state} is [x]")
        self._notify_once("done", "DONE — campaign complete", "every row is [x]")
        return True

    # -- dispatch -----------------------------------------------------------

    def _director_left(self, entry):
        return bool(self.charter) and entry.get("directed", 0) < self.director_max

    def _escalated(self, entry):
        return entry.get("strikes", 0) >= self.max_strikes or entry.get("escalate")

    def _ready(self, queue):
        rows = [r for r in queue.rows if self.static.get(r.id) is U.READY]
        # a [~] row is resumable: a session that was killed left one behind
        return ([r for r in rows if r.status is Status.ACTIVE]
                + [r for r in rows if r.status is not Status.ACTIVE])

    def is_main(self, row_id):
        return not self.lane_mode or row_id.startswith("REVIEW-")

    def _dispatch(self, queue):
        for row in self._ready(queue):
            entry = self.ledger.units.get(row.id)
            if entry and self._escalated(entry) and not self._director_left(entry):
                self.fire(row.id, E.STRIKE_LIMIT)
        picks = self._pick(self._ready(queue))
        if not picks:
            return None
        blocked = self._preconditions(picks)
        if blocked:
            return blocked
        if audit_due(self.paths, queue, picks[0]) and not self._main_busy():
            insert_audit(self.paths, queue, picks[0])
            return None                     # the next tick dispatches the audit row
        for row in picks:
            refusal = dispatch_refusal(self.paths, queue, row)
            if refusal is not None:
                self.fire(row.id, E.DISPATCH_REFUSED, why=refusal)
                continue
            entry = self.ledger.unit(row.id)
            role = "director" if self._escalated(entry) else "worker"
            model, why = self._model_for(row, role)
            if model is None:
                return "model", why
            try:
                session = self._start_session(row, role, model)
            except (OSError, SpawnError) as e:
                self.fire(row.id, E.SPAWN_FAILED, why=f"{type(e).__name__}: {e}")
                continue
            self.fire(row.id, E.DISPATCH_DIRECTOR if role == "director" else E.DISPATCH,
                      session=session)
        return None

    def _conflicts(self):
        p = self.paths.p(self.paths.conflicts)
        return conflict_pairs(p.read_text()) if p.exists() else set()

    def _heavy(self):
        p = self.paths.p(self.paths.heavy)
        if not p.exists():
            return set()
        return {line.split("#")[0].strip() for line in p.read_text().splitlines()} - {""}

    def _pick(self, ready):
        """What may start now. A main-tree unit runs alone: in the pool a ready
        review lets the lanes drain, then runs with nothing beside it. Lanes
        take the free slots, never two conflicting rows or two heavy ones at
        once, and a row marked `<id> *` in conflicts.txt runs by itself."""
        sessions = self.units_in(*SESSION)
        if not ready or len(sessions) >= self.lanes:
            return []
        main = [r for r in ready if self.is_main(r.id)]
        if main:
            if sessions or self.units_in(U.MERGING) or self._main_busy():
                return []
            return main[:1]
        conflicts, heavy = self._conflicts(), self._heavy()
        if any(frozenset((u, ALONE)) in conflicts for u in sessions):
            return []
        active = self.units_in(*ACTIVE)
        picked = []
        for row in ready:
            if len(sessions) + len(picked) >= self.lanes:
                break
            taken = active + [p.id for p in picked]
            if any(frozenset((row.id, other)) in conflicts for other in taken):
                continue
            if row.id in heavy and any(u in heavy for u in sessions + [p.id for p in picked]):
                continue
            alone = frozenset((row.id, ALONE)) in conflicts
            if alone and (sessions or picked):
                continue
            picked.append(row)
            if alone:
                break
        return picked

    def _prompt_text(self):
        if self._prompt is None:
            self._prompt = prompt_text(self.paths)
        return self._prompt

    def _preconditions(self, picks):
        """The one guard, before every dispatch: a queue whose prompt is on
        the result contract, disk, memory for the cargo budget, and (per
        unit, in _dispatch) a healthy model."""
        try:
            text, source = self._prompt_text()
        except (OSError, ValueError) as e:
            return "prompt", f"the worker prompt cannot be read: {e}"
        if "ralph-result" not in text:
            return "prompt", (f"{source} never names ralph-result — the queue is still on the "
                              "file protocol (waiting, NEEDS_HUMAN.md, *.done); move its end "
                              "instructions to ralph-result")
        free = self.disk_free_gb()
        if free < self.disk_floor_gb and self.lane_mode:
            busy = set(self.units_in(*ACTIVE)) | {r.id for r in picks}
            free = self.mech.reclaim_disk(busy, free)
        if free < self.disk_floor_gb:
            return "disk", (f"{free}GB free on {self.disk_root}, under the {self.disk_floor_gb}GB "
                            "floor")
        if self.lane_mode:
            jobs, why = self.jobs_share(self.lanes)
            if jobs is None:
                return "memory", f"the cargo budget could not be read ({CARGO_JOBS_LIB}): {why}"
            if jobs == 0:
                return "memory", why
            self._jobs = jobs
        return None

    def _model_for(self, row, role):
        """(model, None) for the first healthy model of the unit's roster, ("",
        None) when none is configured (the client's default), or (None, why)."""
        m = self.models
        if role == "director":
            roster = m["RESOLVE_MODEL"] or m["REVIEW_MODEL"] or m["MODEL"]
        elif is_review(row):
            roster = m["REVIEW_MODEL"] or m["MODEL"]
        else:
            roster = m["MODEL"]
        names = parse_roster(roster)
        if not names:
            return "", None
        causes = {}
        now = self.clock()
        for name in names:
            ok, cause, at = self._probes.get(name, (None, "", 0))
            if ok is None or now - at >= (PROBE_OK_TTL_S if ok else PROBE_RETRY_S):
                ok, cause = self.probe(name)
                self._probes[name] = (ok, cause, now)
                say(f"probe {name} — {'ok' if ok else cause}")
            if ok:
                return name, None
            causes[name] = cause
        return None, "no healthy model in the roster — " + "; ".join(
            f"{k}: {v}" for k, v in causes.items())

    def _block(self, name, why):
        now = self.clock()
        if self._blocked is None or self._blocked[0] != name:
            self._blocked = [name, why, now, now]
            say(f"loop: blocked ({name}) — {why}")
        elif now - self._blocked[3] >= REBLOCK_LOG_S:
            self._blocked[3] = now
            say(f"loop: still blocked ({name}, {fmt_secs(now - self._blocked[2])}) — {why}")
        if now - self._blocked[2] >= PRECONDITION_NOTIFY_AFTER.get(name, 0):
            self._notify_once(f"blocked:{name}", f"OPERATOR — blocked: {name}", why)
        return Pool.BLOCKED

    def _unblock(self):
        if self._blocked is not None:
            say(f"loop: {self._blocked[0]} holds again — dispatching")
            self.ledger.notified.pop(f"blocked:{self._blocked[0]}", None)
            self._blocked = None

    def _pool_state(self, queue):
        if self.units_in(*SESSION) or self.units_in(U.MERGING):
            pool = Pool.RUNNING
        elif self.units_in(U.AWAITING):
            pool = Pool.IDLE
        elif self._ready(queue):
            pool = Pool.RUNNING           # dispatch was refused or failed this tick; next tick retries
        else:
            held = sorted(r.id for r in queue.rows if self.static.get(r.id) is U.HELD)
            body = (f"held: {', '.join(held)}" if held else
                    f"no rows in {self.paths.state}" if not queue.rows else
                    "nothing ready and nothing held — check the dependencies")
            key = "stuck:" + ",".join(held)
            if not self.ledger.notified.get(key):
                say(f"loop: stuck — {body}")
                for old in [k for k in self.ledger.notified if k.startswith("stuck:")]:
                    self.ledger.notified.pop(old)
            self._notify_once(key, "OPERATOR — stuck", body)
            return Pool.STUCK
        for old in [k for k in self.ledger.notified if k.startswith("stuck:")]:
            self.ledger.notified.pop(old)
        return pool

    # -- sessions and runs --------------------------------------------------

    def _start_session(self, row, role, model):
        entry = self.ledger.unit(row.id)
        n = entry.get("sessions", 0) + 1
        main = self.is_main(row.id)
        notes, env = [], {}
        if main:
            cwd = self.paths.workdir
        else:
            cwd, notes, env = self.mech.prepare(row.id, self._jobs)
        base = (_git_out(cwd, "rev-parse", "HEAD") or "").strip()
        sessions = self.state_dir / "sessions"
        sessions.mkdir(parents=True, exist_ok=True)
        result, base_file = sessions / f"{row.id}-{n}.json", sessions / f"{row.id}-{n}.base.json"
        result.unlink(missing_ok=True)
        base_file.write_text(json.dumps({"head": base, "untracked": untracked_files(cwd)}))
        log = self.paths.p(self.paths.log_dir) / "sessions" / f"{row.id}-{n}.out"
        prompt = (self._director_prompt(row, entry, cwd, main) if role == "director"
                  else self._worker_prompt(row, entry, cwd, main, notes))
        model_args = (["--model", model] if model else []) + (
            ["--variant", self.models["VARIANT"]] if self.models["VARIANT"] else [])
        env.update({"RALPH_RESULT": str(result), "RALPH_BASE": str(base_file),
                    "RALPH_UNIT": row.id, "RALPH_WORKSPACE": str(cwd), "RALPH_PY": str(SELF),
                    "RALPH_AWAIT_MAX": str(self.await_max_s)})
        full_env = {**os.environ, **session_env(self.paths), **env,
                    "PATH": f"{RALPH_BIN}{os.pathsep}{os.environ.get('PATH', '')}"}
        proc = self.spawn([worker_bin(self.paths), "run", *model_args, prompt],
                          cwd=cwd, env=full_env, log_path=log)
        entry["sessions"] = n
        entry["notes"] = []
        now = self.clock()
        return {"role": role, "pid": proc.pid, "started": proc.started, "n": n,
                "begun": now, "deadline": now + self.session_timeout, "base": base,
                "workspace": str(cwd), "main": main, "result": str(result),
                "log": str(log), "model": model,
                "env": {k: v for k, v in env.items() if not k.startswith("RALPH_")}}

    def _worker_prompt(self, row, entry, cwd, main, lane_notes):
        status = _git_out(cwd, "status", "--porcelain") or ""
        text = ""
        if status:
            text += ("NOTE: the tree holds uncommitted work from a prior session:\n" + status
                     + "Inspect it and continue from it; do not discard work already done. "
                       "Commit it as you go.\n\n")
        text += (f"Your unit: {row.id} — its row in {self.paths.state}. Open only that row; do "
                 "not scan the queue for another.\n\n")
        if not main:
            text += (f"POOL LANE: you are working unit {row.id} in an isolated git worktree on "
                     f"branch ralph/{row.id}. Commit your work here; when you report done the "
                     "loop merges the branch and marks the row. Do not edit "
                     f"{self.paths.state} except to correct your own row's premises.\n\n")
        elif self.lane_mode:
            text += "You run in the main tree, alone: no lane runs beside you.\n\n"
        text += "".join(n + "\n\n" for n in lane_notes)
        parked = sorted(parked_ids(self.paths))
        if parked:
            text += f"Parked rows wait on the operator; do not open them: {', '.join(parked)}.\n\n"
        for note in entry.get("notes", []):
            text += f"FROM THE LOOP: {note}\n\n"
        text += CONTRACT + (CONTRACT_OPERATOR if self.charter else "") + "\n"
        return text + self._prompt_text()[0]

    def _director_prompt(self, row, entry, cwd, main):
        why = []
        if entry.get("asked"):
            why.append(f"Its worker asked for a decision:\n{entry['asked']}")
        if entry.get("strikes", 0) >= self.max_strikes:
            why.append(f"It struck out ({entry['strikes']} strikes): "
                       + "; ".join(entry.get("strike_why", [])[-self.max_strikes:]))
        logs = sorted((self.paths.p(self.paths.log_dir) / "sessions").glob(f"{row.id}-*.out"),
                      key=lambda p: p.stat().st_mtime)[-3:]
        decisions = ""
        if self.paths.p(DECISIONS_SCRIPT).exists():
            decisions = (f": `{DECISIONS_SCRIPT} new <campaign>` prints an entry file — fill it "
                         "(date, unit, fork, choice, evidence, what would falsify it; tag "
                         "`REVIEW-AFTER:` when the charter did not clearly cover it), run "
                         f"`{DECISIONS_SCRIPT} --write`, and land both with the change")
        where = "the main tree" if main else f"the lane worktree {cwd}, branch ralph/{row.id}"
        return (f"DIRECTOR for unit {row.id} (attempt {entry.get('directed', 0) + 1} of "
                f"{self.director_max}).\n\n" + "\n\n".join(why) + "\n\n"
                "You are the operator's delegate under the charter below: verify the evidence "
                "and DECIDE — do not defer a fork the charter covers. Reproduce every claim you "
                f"rely on. You work in {where}.\n\n"
                f"1. Read the row in {self.paths.state}, `git log` and `git status` here, and "
                "the last session logs:\n" + "".join(f"   {p}\n" for p in logs) +
                "2. Apply the smallest change that lets the unit flow: correct the row (and its "
                "source order, when a premise was false) or the code.\n"
                f"3. Record the decision in its own commit{decisions}.\n"
                "4. End with `ralph-result continue <what a worker does now>` when a worker can "
                "finish the unit, `ralph-result done` if you finished it, or `ralph-result "
                "needs-human --operator --package FILE <why>` when the charter leaves the fork "
                "to the operator — FILE gives the options, their costs and your "
                "recommendation.\n"
                "Never weaken a PASS BAR, never mark or approve a HUMAN- row, never push.\n\n"
                + CONTRACT + CONTRACT_OPERATOR + "\n=== CHARTER ===\n" + (self.charter or ""))

    def _start_run(self, unit, entry, res):
        s = entry["session"]
        stem = f"{unit}-{s['n']}"
        exit_file = self.state_dir / "sessions" / f"{stem}.exit"
        exit_file.unlink(missing_ok=True)
        log = self.paths.p(self.paths.log_dir) / "sessions" / f"{stem}.await.log"
        env = {**os.environ, **session_env(self.paths), **s.get("env", {}),
               "RALPH_EXIT": str(exit_file)}
        proc = self.spawn(["/bin/sh", "-c", AWAIT_SH, "ralph-await", *res["argv"]],
                          cwd=s["workspace"], env=env, log_path=log)
        now = self.clock()
        return {"pid": proc.pid, "started": proc.started, "begun": now,
                "budget_s": res["budget_s"], "deadline": now + res["budget_s"],
                "argv": res["argv"], "log": str(log), "exit_file": str(exit_file)}

    # -- actions ------------------------------------------------------------

    def _note(self, entry, text):
        entry.setdefault("notes", []).append(text)

    def _do_start_session(self, unit, entry, session):
        entry["session"] = session
        entry["main"] = session["main"]
        if session["role"] == "director":
            entry["directed"] = entry.get("directed", 0) + 1
        return (f"{session['role']} session {session['n']} · pid {session['pid']}"
                + (f" · model {session['model']}" if session["model"] else ""))

    def _do_strike(self, unit, entry, why="", **_):
        if not why:
            s = entry.get("session", {})
            why = (f"session {s.get('n')} ended with no result and no commit "
                   f"(log {s.get('log')})"
                   + (f"; {s['rejects']}" if s.get("rejects") else ""))
        entry["strikes"] = entry.get("strikes", 0) + 1
        entry.setdefault("strike_why", []).append(why)
        self._note(entry, f"strike {entry['strikes']} of {self.max_strikes}: {why}")
        return f"strike {entry['strikes']}/{self.max_strikes} — {why}"

    def _do_accept_done(self, unit, entry, result):
        entry.update(strikes=0, continuations=0, escalate=False, asked="", strike_why=[],
                     done_head=(result.get("head") or "")[:9])
        return "reported done"

    def _do_continue_(self, unit, entry, result=None, commits=0):
        k = entry.get("continuations", 0) + 1
        entry["continuations"] = k
        note = (result or {}).get("note", "")
        if note:
            self._note(entry, f"the previous session's note: {note}")
        how = "reported continue" if result else f"{commits} commit(s), no result"
        if k > self.max_continuations:
            return self._do_strike(unit, entry, why=f"{how} — {k} continuations in a row "
                                                    f"(bound {self.max_continuations})")
        return f"{how} — continuation {k}/{self.max_continuations}"

    def _do_start_await(self, unit, entry, result, run):
        entry["run"] = run
        return (f"run {run['argv']} · pid {run['pid']} · budget {fmt_secs(run['budget_s'])} · "
                f"log {run['log']}")

    def _package(self, unit, entry, head, body=""):
        s = entry.get("session", {})
        lines = [f"# {unit}: {head}", ""]
        if body:
            lines += [body.rstrip(), ""]
        if entry.get("strike_why"):
            lines += ["Strikes:", *[f"- {w}" for w in entry["strike_why"]], ""]
        if s.get("log"):
            tail = halt_tail_suffix(s["log"])
            lines += [f"Last session log: {s['log']}{tail}", ""]
        if not entry.get("main", True) and self.lane_mode:
            lines += [f"Lane worktree: {self.mech.lane_root / unit} (branch ralph/{unit})", ""]
        return "\n".join(lines)

    def _park(self, unit, entry, head, body=""):
        dest = write_parked(self.paths, unit, self._package(unit, entry, head, body), head)
        entry["parked"] = True
        self._notify_once(f"held:{unit}:{int(self.clock())}", "OPERATOR — held, loop continues",
                          f"{unit}: {head}")
        return f"parked at {dest.relative_to(self.paths.workdir)}"

    def _do_park_asked(self, unit, entry, result):
        who = "the director" if entry.get("session", {}).get("role") == "director" else "its session"
        return self._park(unit, entry, f"{who} needs the operator — {result['why']}",
                          result.get("package", ""))

    def _do_escalate(self, unit, entry, result):
        entry["escalate"] = True
        entry["asked"] = result["why"] + ("\n\n" + result["package"] if result.get("package") else "")
        return "the worker asked; a director goes next"

    def _do_park_struck(self, unit, entry):
        head = (f"struck out ({entry.get('strikes', 0)} strikes)" if not entry.get("escalate")
                else "its worker asked and no director is left")
        return self._park(unit, entry, head, entry.get("asked", ""))

    def _do_park_director(self, unit, entry, why="", **_):
        return self._park(unit, entry, why or "the director ended without a result")

    def _do_park_refused(self, unit, entry, why):
        return self._park(unit, entry, "refused at dispatch", why)

    def _do_park_unmarked(self, unit, entry, why):
        return self._park(unit, entry, "landed, but not marked done", why)

    def _do_interrupted(self, unit, entry):
        self._note(entry, "an operator stop ended the previous session; whatever it left "
                          "uncommitted is still in the tree")
        return "session ended by the operator — no strike"

    def _do_director_cleared(self, unit, entry, result):
        entry.update(strikes=0, continuations=0, escalate=False, asked="", strike_why=[])
        if result.get("note"):
            self._note(entry, f"the director: {result['note']}")
        return "the director cleared it — a worker goes next"

    def _do_run_ended(self, unit, entry, code):
        r = entry["run"]
        how = (f"exited {code}" if code is not None else
               "is gone without an exit status (a reboot, or something killed it)")
        self._note(entry, f"your background run {r['argv']} {how} after "
                          f"{fmt_secs(self.clock() - r['begun'])}; its log is {r['log']}. Read it "
                          "and carry on from what it shows.")
        return f"run {how}"

    def _do_run_over_budget(self, unit, entry):
        r = entry["run"]
        why = (f"background run {r['argv']} passed its {fmt_secs(r['budget_s'])} budget and the "
               f"loop ended it; its log is {r['log']}")
        return self._do_strike(unit, entry, why=why)

    def _do_unhold(self, unit, entry):
        if not entry.pop("parked", None):
            return None
        entry.update(strikes=0, continuations=0, directed=0, escalate=False, asked="",
                     strike_why=[])
        return "unparked by the operator — counters reset"
class Lanes:
    """The git and disk work of a lane: its worktree beside the main tree, its
    cloned target, the base merged into it, and, once it is done, the merge
    back, the decision renumbering, its evidence and its removal. No state:
    the loop's ledger holds that."""

    def __init__(self, paths, *, lane_root, base_branch, disk_free_gb, disk_floor_gb):
        self.paths = paths
        self.lane_root = pathlib.Path(lane_root)
        self.base_branch = base_branch
        self.disk_free_gb = disk_free_gb
        self.disk_floor_gb = disk_floor_gb

    def git(self, *args, cwd=None):
        return subprocess.run(["git", "-C", str(cwd or self.paths.workdir), *args],
                              capture_output=True, text=True)

    def prepare(self, unit, jobs):
        """The lane's worktree, made or brought up to the base: (path, notes
        for the session, env). Raises SpawnError when git cannot make it."""
        wt, branch = self.lane_root / unit, f"ralph/{unit}"
        notes = []
        if not wt.exists():
            self.lane_root.mkdir(parents=True, exist_ok=True)
            exists = self.git("rev-parse", "-q", "--verify", f"refs/heads/{branch}").returncode == 0
            args = ([str(wt), branch] if exists
                    else ["-b", branch, str(wt), self.base_branch])
            r = self.git("worktree", "add", "-q", *args)
            if r.returncode != 0:
                raise SpawnError(f"git worktree add for {unit}: "
                                 f"{error_tail(r.stderr) or r.stderr.strip()[-200:]}")
            say(f"lane {unit}: worktree {wt} on {branch}")
            self.provision_target(unit, wt)
        else:
            # A lane is kept across sessions, so a fix that lands on the base
            # never reaches it unless it is brought in (dm-daemon-api-edge burned
            # three waves on a stale opencode.json, 2026-09-17). A lane with no
            # commits of its own fast-forwards; one with work merges the base in
            # HERE, where its session can resolve a conflict, rather than at the
            # loop's merge (2026-09-18).
            own = self.git("rev-list", "--count", f"{self.base_branch}..HEAD", cwd=wt)
            if own.returncode == 0 and own.stdout.strip() == "0":
                ff = self.git("merge", "--ff-only", self.base_branch, cwd=wt)
                say(f"lane {unit}: " + ("refreshed onto " + self.base_branch if ff.returncode == 0
                                        else f"not refreshed: {ff.stderr.strip()}"))
            elif own.returncode == 0:
                m = self.git("merge", "--no-edit", self.base_branch, cwd=wt)
                say(f"lane {unit}: " + (f"merged {self.base_branch} in" if m.returncode == 0
                                        else f"conflicts with {self.base_branch} — the session "
                                             "resolves it"))
        self.provision_host_pointers(wt)
        if self.git("rev-parse", "-q", "--verify", "MERGE_HEAD", cwd=wt).returncode == 0:
            notes.append("Your worktree has a MERGE IN PROGRESS: the loop merged the base branch "
                         "in and it conflicted. Resolve every conflict, `git add` the files, "
                         "`git commit --no-edit`, then do your unit.")
        # One cargo lock per lane: a lane builds in its own worktree and target,
        # so the shared lock would only serialize lanes against each other
        # (2026-09-16 speed order).
        env = {"SVRN_CARGO_LOCK_DIR": f"/tmp/svrn-cargo-lock.{os.getuid()}.lane-{unit}"}
        if jobs:
            share = {var: str(jobs) for var in LANE_JOBS_VARS}
            env.update(share)
            jobs_file = wt / LANE_JOBS_FILE
            jobs_file.parent.mkdir(parents=True, exist_ok=True)
            jobs_file.write_text("".join(f"{k}={v}\n" for k, v in share.items()))
        return wt, notes, env

    def provision_host_pointers(self, wt):
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
            say(f"lane: could not provision {dst} from {src}: {e}")

    # A reflink clone shares the main tree's blocks until a lane rebuilds them
    # (btrfs; 7s for 136G, measured 2026-09-01). Never one target shared across
    # worktrees: cargo then ran another tree's build script (same date). macOS
    # cp has no --reflink ("illegal option", so every ersilia lane built from
    # an empty target, 2026-10-02); its -c is the APFS clonefile(2) clone.
    CLONE_TARGET = (("cp", "-a", "-c") if sys.platform == "darwin"
                    else ("cp", "-a", "--reflink=always"))
    # Cargo's output under a lane's target/: the next build regenerates it,
    # unlike the rest of target/ (the lane's evidence, battery and field scratch).
    RECLAIMABLE = ("debug", "release")

    def reclaim_disk(self, busy, free_gb):
        """Under the disk floor, free cargo output from lanes that cannot need
        it now, least recently built first, until the lane root is back over
        the floor; return the free GB after. A lane whose unit is active or
        about to start (its session or its background run may be executing
        those binaries) is never touched. The floor alone sat the ersilia pool idle
        for 66 ticks while two idle lanes held 15GB of it (2026-10-04)."""
        if not self.lane_root.is_dir():
            return free_gb
        idle = []
        for wt in self.lane_root.iterdir():
            if not wt.is_dir() or wt.name in busy:
                continue
            dirs = [wt / "target" / d for d in self.RECLAIMABLE if (wt / "target" / d).is_dir()]
            if dirs:
                idle.append((max(d.stat().st_mtime for d in dirs), wt.name, dirs))
        for _, unit, dirs in sorted(idle):
            if free_gb >= self.disk_floor_gb:
                break
            for d in dirs:
                err = remove_tree(d)
                if err is not None:
                    say(f"lane {unit} target/{d.name} not reclaimed: {err}")
            before, free_gb = free_gb, self.disk_free_gb()
            say(f"lane: reclaimed {free_gb - before}GB of build output from idle lane {unit} "
                f"({', '.join(f'target/{d.name}' for d in dirs)}) — it rebuilds when it next runs")
        return free_gb

    def provision_target(self, unit, wt):
        """A new lane's target/ is a clone of the main tree's, and every tracked
        file in the lane is touched after it, so the workspace crates rebuild
        once (~3-4 min) and external deps stay warm. Where the clone cannot be
        made the lane builds from an empty target, and the log says so."""
        src, dst = self.paths.workdir / "target", wt / "target"
        if not src.is_dir() or dst.exists():
            return
        r = subprocess.run([*self.CLONE_TARGET, str(src), str(dst)],
                           capture_output=True, text=True)
        if r.returncode != 0:
            err = remove_tree(dst)
            if err is not None:
                say(f"lane {unit} partial target clone not removed: {err}")
            say(f"lane {unit} target NOT cloned "
                f"({error_tail(r.stderr) or r.stderr.strip()[-200:]}) — the lane "
                "builds from an empty target")
            return
        # The lane's evidence directory starts empty: _keep_evidence copies it back.
        err = remove_tree(dst / "ralph")
        if err is not None:
            say(f"lane {unit} cloned evidence not cleared ({err}) — "
                "_keep_evidence will copy the main tree's back with the lane's")
        files = self.git("ls-files", "-z", cwd=wt).stdout.split("\0")
        touched = 0
        for rel in filter(None, files):
            try:
                os.utime(wt / rel)
                touched += 1
            except OSError:
                pass          # a tracked path the checkout does not hold (a submodule)
        say(f"lane {unit} target cloned from {src}; {touched} tracked files touched "
            "— the workspace crates rebuild once, external deps stay warm")

    def renumber_decisions(self, unit, wt, branch):
        """Lanes in one wave each mint `<campaign>-<max+1>` from the same tree,
        so the second merge is an add/add conflict. Before merging, give every
        decision the lane added whose path the main tree already holds the next
        free id (ralph-decisions.py renumber, the one minting rule), on the
        lane's branch. Returns a halt reason or None."""
        entries = self.paths.p(DECISIONS_DIR)
        if not entries.is_dir():
            return None
        added = self.git("diff", "--name-only", "--diff-filter=A", f"HEAD...{branch}",
                          "--", DECISIONS_DIR)
        clashes = [rel for rel in added.stdout.split() if self.paths.p(rel).exists()]
        renamed = {}
        for rel in clashes:
            r = subprocess.run([sys.executable, str(self.paths.p(DECISIONS_SCRIPT)), "renumber",
                                str(wt / rel), "--against", str(entries)],
                               capture_output=True, text=True)
            if r.returncode != 0:
                return (f"lane {unit}: could not renumber {rel}: "
                        f"{error_tail(r.stderr) or r.stderr.strip()[-200:]}")
            renamed[pathlib.Path(rel).stem] = pathlib.Path(r.stdout.strip()).stem
            say(f"lane {unit} decision {rel} is taken on the base — renumbered "
                f"{pathlib.Path(r.stdout.strip()).name}")
        if clashes:
            self.recite_decisions(unit, wt, branch, renamed)
            self.git("add", "-A", "--", DECISIONS_DIR, cwd=wt)
            mapping = ", ".join(f"{o} → {n}" for o, n in renamed.items())
            c = self.git("commit", "-q", "-m",
                          f"{unit}: decision ids renumbered at merge (pool): {mapping}", cwd=wt)
            if c.returncode != 0:
                return (f"lane {unit}: the renumber commit failed: "
                        f"{error_tail(c.stderr) or c.stderr.strip()[-200:]}")
        return None

    def recite_decisions(self, unit, wt, branch, renamed):
        """The lane's own files still cite the ids it minted, which on the base
        name other entries (2026-10-02: pc-removed-env-warn's .done cited
        phase-c-2, the seat's filing, after its entry became phase-c-4).
        Rewrite each old id to its new one in every file the lane changed and
        stage them; the renumbered entry, which records the old id on purpose,
        is not among them (its old path is gone, its new one is untracked)."""
        pattern = re.compile(r"(?<![\w-])(" + "|".join(map(re.escape, renamed)) + r")(?!\d)")
        changed = self.git("diff", "--name-only", "--diff-filter=AM", f"HEAD...{branch}")
        rewritten = []
        for rel in changed.stdout.split():
            path = wt / rel
            if not path.is_file():
                continue
            try:
                text = path.read_text()
            except UnicodeDecodeError:
                continue
            new = pattern.sub(lambda m: renamed[m.group(1)], text)
            if new != text:
                path.write_text(new)
                rewritten.append(rel)
        if rewritten:
            self.git("add", "--", *rewritten, cwd=wt)
        say(f"lane {unit} citations of {', '.join(renamed)} rewritten in "
            f"{len(rewritten)} file(s){': ' + ', '.join(rewritten) if rewritten else ''}")

    def regenerate_decisions(self, unit):
        """Lanes write ralph/decisions/<id>.md only; the rendered ledger is
        regenerated here, once per merge, and lands in the mark commit. Returns
        a halt reason or None."""
        if not self.paths.p(DECISIONS_DIR).is_dir():
            return None
        r = subprocess.run([sys.executable, str(self.paths.p(DECISIONS_SCRIPT)), "--write"],
                           cwd=str(self.paths.workdir), capture_output=True, text=True)
        if r.returncode != 0:
            return (f"merged {unit}, but {DECISIONS_SCRIPT} --write failed: "
                    f"{error_tail(r.stderr) or (r.stderr or r.stdout).strip()[-200:]}")
        say(f"lane: {r.stdout.strip()}")
        self.git("add", "--", DECISIONS_RENDERED)
        return None

    def remove_lane(self, unit, wt):
        """A merged lane's worktree goes, its target included; whatever `git
        worktree remove` leaves is removed here and said, never left silent."""
        r = self.git("worktree", "remove", "--force", str(wt))
        if not wt.exists():
            return
        err = remove_tree(wt)
        self.git("worktree", "prune")
        why = error_tail(r.stderr) or r.stderr.strip()[-200:] or f"exit {r.returncode}"
        if err is None:
            say(f"lane {unit} worktree remove left {wt} ({why}) — removed it")
        else:
            say(f"lane {unit} worktree {wt} NOT removed ({why}; then {err}) — "
                "its target holds disk until it is")

    def keep_evidence(self, unit, wt):
        """A lane's raw evidence (its target/ralph/: check logs, readings) is
        copied to <log_dir>/<unit>/ in the main tree before the worktree, its
        target included, is removed. False when the copy failed: the caller
        keeps the worktree rather than lose what a commit may cite."""
        src = wt / "target" / "ralph"
        if not src.is_dir():
            return True
        dest = self.paths.p(self.paths.log_dir) / unit
        try:
            shutil.copytree(src, dest, symlinks=True, dirs_exist_ok=True)
        except (OSError, shutil.Error) as e:
            say(f"lane {unit} evidence copy to {dest} failed: {e}")
            return False
        say(f"lane {unit} evidence kept at {dest}")
        return True
# ---------------------------------------------------------------------------
# The file protocol, read once. A loop that starts with no ledger may be
# taking over from the file-protocol loop this one replaced: its control
# files and lane markers are read here, once, turned into ledger entries,
# parks and notes for the next session, and removed. After that no file an
# agent writes is read again.

LEGACY_WAITING_NOTE = (
    "before this loop took over, a session left `{rel}` for a background run it started "
    "itself:\n{text}\n{how} The loop watches only runs it starts: from now on, end with "
    "`ralph-result await` instead.")
# A legacy wait whose first line names a marker becomes a loop-owned wait: a
# watcher the loop starts, which ends when the marker appears, bounded so a
# run that died without writing it costs this long, not the old 48h.
LEGACY_WAIT_BUDGET_S = 12 * 3600
LEGACY_WAIT_SH = 'until [ -e "$1" ]; do sleep 30; done'


def legacy_marker(waiting):
    """The marker a file-protocol waiting file's FIRST line names, or None."""
    first = waiting.read_text(errors="replace").partition("\n")[0]
    m = re.search(r"[A-Za-z0-9._/-]+\.done", first)
    if not m or m.group(0).startswith(f"{LANE_DONE_DIR}/"):
        return None
    return m.group(0)


def adopt_wait(loop, unit, root, waiting, main, dry, lines):
    """One legacy wait: a watcher on its marker when its first line names one,
    else a note for the unit's next session."""
    text = waiting.read_text(errors="replace").strip()
    rel = loop.paths.waiting
    marker = legacy_marker(waiting)
    entry = loop.ledger.unit(unit)
    if marker is None:
        lines.append(f"{unit}: {rel} names no marker — a note for its next session")
        if not dry:
            loop._note(entry, LEGACY_WAITING_NOTE.format(
                rel=rel, text=text, how="Check whether that run is still going (ps, its "
                "log, its output files) and carry on from what you find."))
        return
    lines.append(f"{unit}: waits on {marker} — a loop-owned watcher, budget "
                 f"{fmt_secs(LEGACY_WAIT_BUDGET_S)}")
    if dry:
        return
    stem = f"{unit}-adopted"
    exit_file = loop.state_dir / "sessions" / f"{stem}.exit"
    exit_file.parent.mkdir(parents=True, exist_ok=True)
    exit_file.unlink(missing_ok=True)
    log = loop.paths.p(loop.paths.log_dir) / "sessions" / f"{stem}.await.log"
    argv = ["/bin/sh", "-c", LEGACY_WAIT_SH, "ralph-legacy-wait", str(root / marker)]
    proc = loop.spawn(["/bin/sh", "-c", AWAIT_SH, "ralph-await", *argv], cwd=root,
                      env={**os.environ, "RALPH_EXIT": str(exit_file)}, log_path=log)
    now = loop.clock()
    loop._note(entry, LEGACY_WAITING_NOTE.format(
        rel=rel, text=text, how=f"The loop waited for {marker} to appear; if it did not, the "
        "run may have died without writing it — check its log before relaunching."))
    entry.update(state=U.AWAITING.value, main=main, run={
        "pid": proc.pid, "started": proc.started, "begun": now,
        "budget_s": LEGACY_WAIT_BUDGET_S, "deadline": now + LEGACY_WAIT_BUDGET_S,
        "argv": argv, "log": str(log), "exit_file": str(exit_file)})


def adopt_legacy(loop, dry=False):
    """What the file protocol left, as lines saying what was done with each;
    with `dry`, what would be. Called by Loop.boot on a fresh ledger."""
    paths, ledger, lines = loop.paths, loop.ledger, []
    try:
        queue = Queue(paths.p(paths.state))
    except (OSError, ValueError) as e:
        return [f"{paths.state} does not parse ({e}) — nothing adopted; the loop blocks on it"]
    rows = queue.by_id()
    held = held_ids(paths, queue)

    def unlink(path):
        if not dry:
            pathlib.Path(path).unlink(missing_ok=True)

    def park(unit, package, why):
        lines.append(f"{unit}: parked — {why}")
        if not dry:
            write_parked(paths, unit, package, why)
            ledger.unit(unit)["parked"] = True

    counters = paths.workdir / "target" / "ralph" / "pool-state.json"
    if counters.exists():
        try:
            saved = json.loads(counters.read_text())
        except (OSError, ValueError):
            saved = {}
        for unit, n in saved.get("failures", {}).items():
            n = max(n, saved.get("merge_failures", {}).get(unit, 0))
            if unit in rows and n:
                lines.append(f"{unit}: {n} strike(s) carried from the pool's counters")
                if not dry:
                    ledger.unit(unit)["strikes"] = n
        for unit, k in saved.get("continuations", {}).items():
            if unit in rows and k and not dry:
                ledger.unit(unit)["continuations"] = k
        unlink(counters)

    active = waiting_row(queue, held)
    waiting = paths.p(paths.waiting)
    if waiting.exists():
        if active is not None:
            adopt_wait(loop, active.id, paths.workdir, waiting, True, dry, lines)
        else:
            lines.append(f"{paths.waiting} names no [~] row — dropped: {first_line(waiting)[:120]}")
        unlink(waiting)

    pkg = paths.p(paths.needs_human)
    if pkg.exists() and pkg.stat().st_size:
        text = pkg.read_text(errors="replace")
        row = blocked_row(queue, first_line(pkg), held) or active
        if row is not None:
            park(row.id, text, "the file protocol's halt package")
        else:
            dest = paths.workdir / "target" / "ralph" / "halts" / f"{int(time.time())}-adopted.md"
            lines.append(f"{paths.needs_human} names no row — archived at {dest}: "
                         f"{first_line(pkg)[:120]}")
            if not dry:
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_text(text)
        unlink(pkg)
    stop = paths.p(paths.stop)
    if stop.exists() and stop.stat().st_size and stop.read_text().strip() != "drain":
        lines.append(f"{paths.stop} held a halt reason — removed: {first_line(stop)[:120]}")
        unlink(stop)
    if paths.p(paths.done).exists() and not (queue.rows and queue.all_done()):
        lines.append(f"{paths.done} removed — rows remain open")
        unlink(paths.p(paths.done))

    root = loop.mech.lane_root
    if loop.lane_mode and root.is_dir():
        for wt in sorted(d for d in root.iterdir() if d.is_dir()):
            unit = wt.name
            if unit not in rows or rows[unit].status is Status.DONE:
                continue
            lane_pkg = wt / paths.needs_human
            if lane_pkg.exists() and lane_pkg.stat().st_size:
                park(unit, lane_pkg.read_text(errors="replace"), "its lane's halt package")
                unlink(lane_pkg)
                continue
            marker = f"{LANE_DONE_DIR}/{unit}.done"
            own = loop.mech.git("diff", "--name-only", f"HEAD...ralph/{unit}", "--", marker)
            if (wt / marker).exists() and marker in own.stdout.split():
                lines.append(f"{unit}: its lane wrote {marker} — it merges next")
                if not dry:
                    tip = (loop.mech.git("rev-parse", "--short", f"ralph/{unit}").stdout or "").strip()
                    ledger.unit(unit).update(state=U.MERGING.value, main=False, done_head=tip)
                continue
            lane_waiting = wt / paths.waiting
            if lane_waiting.exists():
                adopt_wait(loop, unit, wt, lane_waiting, False, dry, lines)
                if not dry:
                    lane_waiting.unlink()
                    loop.mech.git("add", "-A", "--", paths.waiting, cwd=wt)
                    loop.mech.git("commit", "-q", "-m",
                                  f"{unit}: waiting ended — the loop now owns background runs",
                                  cwd=wt)
    return lines or ["nothing of the file protocol left to adopt"]
def install_job(name, program_args, workdir, log_path, interval=None, keep_alive=False):
    """Exit-2 text instead of a traceback when the host has no job backend."""
    try:
        return host().install_job(name, program_args, workdir, log_path, interval, keep_alive)
    except HostError as e:
        print(f"install: {e}", file=sys.stderr)
        return None


def ensure_excludes(workdir, rel_paths):
    """Runtime markers must not dirty the tree the campaign commits into.
    Git names the exclude file: in a linked worktree `.git` is a file and the
    excludes live in the common dir, so `<workdir>/.git/info` does not exist."""
    r = subprocess.run(["git", "-C", str(workdir), "rev-parse", "--git-path", "info/exclude"],
                       capture_output=True, text=True)
    if r.returncode != 0:
        return
    exclude = pathlib.Path(workdir) / r.stdout.strip()
    exclude.parent.mkdir(parents=True, exist_ok=True)
    existing = set(exclude.read_text().splitlines()) if exclude.exists() else set()
    with exclude.open("a") as fh:
        for rel in rel_paths:
            if rel not in existing:
                fh.write(f"{rel}\n")


def state_dir_for(paths, label):
    d = pathlib.Path.home() / ".svrnmesh" / "ralph" / f"{paths.workdir.name}-{label}"
    d.mkdir(parents=True, exist_ok=True)
    return d


RUNTIME_MARKERS = ("ralph/DONE", "ralph/STOP", "ralph/NEEDS_HUMAN.md", "ralph/parked/",
                   "ralph/.heartbeat", "ralph/waiting", "ralph/models.env",
                   "ralph/log.txt", "ralph/.director-commits")


def runtime_markers(paths):
    """What must not dirty the tree: a queue's whole control dir, else the
    legacy set, and the loop's own logs either way. A session log created in
    the main tree after the dispatch snapshot would otherwise read as the
    session's untracked file, and `done` would be refused."""
    own = (f"{paths.control_dir}/",) if paths.queue else RUNTIME_MARKERS
    return own + (f"{paths.log_dir}/",)

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
            f"VARIANT={current.get('VARIANT', '')}\n")
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
    return 0

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

class Watch:
    """The watchdog's one job: notice the loop is down, or its heartbeat is
    stale, which is the one thing the loop cannot report about itself. The
    loop writes its heartbeat every tick in every state, so stale means dead.
    Everything else (held units, a blocked precondition, done) the loop says."""

    def __init__(self, paths, *, label, running=None, stall_secs=STALL_SECS, notifier=notify,
                 dry=False):
        self.paths = paths
        self.label = label
        self.running = running or (lambda: job_running(paths, label))
        self.stall_secs = stall_secs
        self.notifier = notifier
        self.dry = dry

    def condition(self):
        if self.paths.p(self.paths.done).exists():
            return None
        stop = self.paths.p(self.paths.stop)
        if stop.exists() and not stop.read_text().strip():
            return None
        name = f"{self.paths.workdir.name}-{self.label}"
        if not self.running():
            return ("down", f"{name} is not running and has no DONE or operator STOP")
        beat = self.paths.p(self.paths.heartbeat)
        if beat.exists():
            age = int(time.time() - beat.stat().st_mtime)
            if age > self.stall_secs:
                return ("stalled", f"{name}: no heartbeat for {age}s — the loop is hung")
        return None

    def run(self, state_file, nag_secs=1800):
        condition = self.condition()
        sf = pathlib.Path(state_file)
        last_cond, last_ts = "", 0
        if sf.exists():
            parts = sf.read_text().split()
            if len(parts) >= 2:
                last_cond, last_ts = parts[0], int(parts[1])
        if condition is None:
            sf.write_text("")
            return None
        cond, body = condition
        now = int(time.time())
        if cond != last_cond or now - last_ts >= nag_secs:
            title = {"down": "OPERATOR — loop down", "stalled": "OPERATOR — loop hung"}[cond]
            if self.dry:
                print(f"notify: {title}: {body}")
            else:
                self.notifier(title, body, True)
            sf.write_text(f"{cond} {now}\n")
        return cond


def ledger_for(paths, label):
    return state_dir_for(paths, label) / "loop.json"


def current_branch(workdir):
    return subprocess.run(["git", "-C", str(workdir), "rev-parse", "--abbrev-ref", "HEAD"],
                          capture_output=True, text=True).stdout.strip()


def loop_for(args, *, lane_mode):
    paths = paths_for(args)
    models = resolve_models(args, paths)
    charter_path = paths.p(paths.charter)
    charter = charter_path.read_text() if charter_path.exists() else None
    strikes = args.max_lane_failures if lane_mode else args.max_stall
    if getattr(args, "max_iter", None):
        say(f"--max-iter {args.max_iter} is ignored: the loop is bounded per unit by its "
            "strikes, not by a session count")
    return paths, Loop(paths, label=args.label, lanes=args.lanes if lane_mode else 1,
                       lane_mode=lane_mode,
                       base_branch=(getattr(args, "base_branch", "") or
                                    current_branch(paths.workdir)),
                       session_timeout=args.session_timeout, max_strikes=strikes,
                       await_max_s=args.marker_timeout or AWAIT_MAX_S, models=models,
                       charter=charter, notify_enabled=not args.no_notify)


def install_loop_job(args, paths, verb):
    """The loop as a host job the host restarts on any exit but 0 (done or
    stopped): launchd KeepAlive, systemd Restart=on-failure."""
    ensure_excludes(paths.workdir, runtime_markers(paths))
    argv = [sys.executable, str(SELF), verb, "--workdir", str(paths.workdir),
            "--label", args.label, *queue_flags(paths),
            "--session-timeout", str(args.session_timeout)]
    if verb == "pool":
        argv += ["--lanes", str(args.lanes)]
        if not paths.queue:
            argv += ["--conflicts", paths.conflicts]
    else:
        argv += ["--max-stall", str(args.max_stall)]
    job = install_job(job_name(paths, args.label), argv, paths.workdir,
                      str(state_dir_for(paths, args.label) / "launchd.log"), keep_alive=True)
    print(f"wrote {job}" if job else "nothing installed")
    return 0 if job else 2


def cmd_run(args):
    paths, loop = loop_for(args, lane_mode=False)
    if args.install_launchd:
        return install_loop_job(args, paths, "run")
    ensure_excludes(paths.workdir, runtime_markers(paths))
    return loop.run()


def cmd_pool(args):
    paths, loop = loop_for(args, lane_mode=True)
    if args.install_launchd:
        return install_loop_job(args, paths, "pool")
    ensure_excludes(paths.workdir, runtime_markers(paths) + (".ralph/",))
    return loop.run()


def cmd_supervise(args):
    """The supervisor is gone: the host restarts the loop and the loop holds
    its own escalations. A supervise line (an installed job, a launch line in
    a queue's comments) runs, or installs, the campaign command it wraps."""
    campaign = list(args.campaign)
    if campaign and campaign[0] == "--":
        campaign = campaign[1:]
    if not campaign:
        print("ralph supervise: the campaign command is required after --", file=sys.stderr)
        return 2
    say("supervise: the supervisor is retired (docs/RALPH_STATE_MACHINE.md) — running the "
        "campaign command directly")
    if args.install_launchd:
        paths = paths_for(args)
        job = install_job(job_name(paths, args.label), campaign, paths.workdir,
                          str(state_dir_for(paths, args.label) / "launchd.log"), keep_alive=True)
        print(f"wrote {job}" if job else "nothing installed")
        return 0 if job else 2
    sys.stdout.flush()
    os.execvp(campaign[0], campaign)


def cmd_watch(args):
    paths = paths_for(args)
    state_dir = state_dir_for(paths, args.label)
    if args.install_launchd:
        job = install_job(
            f"dev.ralphwatch.{paths.workdir.name}-{args.label}",
            [sys.executable, str(SELF), "watch", "--workdir", str(paths.workdir),
             "--label", args.label, *(["--queue", paths.queue] if paths.queue else [])],
            paths.workdir, state_dir / "watch.log", interval=120)
        print(f"wrote {job}" if job else "nothing installed")
        return 0 if job else 2
    Watch(paths, label=args.label, dry=os.environ.get("RALPH_WATCH_DRY") == "1").run(
        state_dir / "watch.state")
    return 0


def cmd_stop(args):
    """An operator stop. Plain: running sessions end now (their worktrees
    resume later). --drain: running sessions finish, nothing new starts. Either
    way a background run keeps running and the next start watches it again."""
    paths = paths_for(args)
    stop = paths.p(paths.stop)
    stop.parent.mkdir(parents=True, exist_ok=True)
    stop.write_text("drain\n" if args.drain else "")
    say(f"stop: wrote {paths.stop}{' (drain)' if args.drain else ''} — waiting up to "
        f"{args.timeout}s for the loop to exit")
    deadline = time.time() + args.timeout
    while time.time() < deadline:
        if not job_running(paths, args.label):
            say("stop: the loop is down")
            return 0
        time.sleep(3)
    if not args.hard:
        say("stop: still running" + (" (draining: its sessions are finishing)" if args.drain
                                     else "") + " — --hard boots the job out")
        return 1
    try:
        host().stop_job(job_name(paths, args.label))
    except HostError as e:
        say(f"stop: {e}")
    path = ledger_for(paths, args.label)
    killed = 0
    if path.exists():
        for unit, entry in Ledger(path).units.items():
            for key in ("session", "run"):
                rec = entry.get(key)
                if rec and entry.get("state") in (U.RUNNING.value, U.DIRECTING.value,
                                                  U.AWAITING.value):
                    proc_kill(Proc(rec["pid"], rec["started"]))
                    killed += 1
    say(f"stop: booted out; {killed} session(s) and run(s) taken down")
    return 0


def cmd_start(args):
    paths = paths_for(args)
    job = job_name(paths, args.label)
    try:
        host().require_jobs()
        job_file = host().job_file(job)
        if not job_file.exists():
            print(f"start: no job installed at {job_file} — install it first:\n"
                  f"  python3 {SELF} run|pool --workdir {paths.workdir} --label {args.label} "
                  f"{' '.join(queue_flags(paths))} --install-launchd", file=sys.stderr)
            return 2
        stop = paths.p(paths.stop)
        if stop.exists():
            stop.unlink()
            say("start: cleared the operator STOP")
        if host().job_running(job):
            say(f"start: {job} is already running — left alone")
            return 0
        host().start_job(job)
    except HostError as e:
        print(f"start: {e}", file=sys.stderr)
        return 2
    say(f"start: {job} started")
    return 0


def cmd_unpark(args):
    paths = paths_for(args)
    pkg = paths.p(paths.parked) / f"{args.row}.md"
    if not pkg.exists():
        print(f"unpark: {args.row} is not parked ({pkg} does not exist)", file=sys.stderr)
        return 2
    pkg.unlink()
    say(f"unpark: {args.row} released — the loop resets its strikes and dispatches it")
    return 0


def heartbeat_line(paths):
    beat = paths.p(paths.heartbeat)
    try:
        return f"heartbeat {int(time.time() - beat.stat().st_mtime)}s ago: {beat.read_text().strip()}"
    except OSError:
        return "no heartbeat"


def active_logs(entries):
    """{unit: (state, what, log)} for every unit holding a process: a session's
    transcript, or an awaiting unit's run output."""
    out = {}
    for unit, entry in entries.items():
        state = entry.get("state")
        if state in (U.RUNNING.value, U.DIRECTING.value):
            s = entry["session"]
            out[unit] = (state, f"{s['role']} session {s['n']}, pid {s['pid']}", s["log"])
        elif state == U.AWAITING.value:
            r = entry["run"]
            out[unit] = (state, f"run {r['argv']}, pid {r['pid']}, budget "
                                f"{fmt_secs(r['budget_s'])}", r["log"])
    return out


def status_lines(paths, label, pool=False):
    """The loop's state: its heartbeat, what it holds, what waits on the
    operator. With no ledger yet, what a start would adopt."""
    queue = Queue(paths.p(paths.state))
    yield heartbeat_line(paths)
    yield f"queue {paths.state}: {queue.done_count()}/{len(queue.rows)} done"
    path = ledger_for(paths, label)
    if not path.exists():
        loop = Loop(paths, label=label, lane_mode=pool, lanes=2 if pool else 1,
                    state_dir=path.parent)
        yield f"no ledger at {path} — a start would adopt:"
        yield from (f"  {line}" for line in adopt_legacy(loop, dry=True))
        return
    ledger = Ledger(path)
    now = time.time()
    for unit, entry in ledger.units.items():
        state = entry.get("state")
        counters = ", ".join(f"{k} {entry[k]}" for k in ("strikes", "continuations", "directed")
                             if entry.get(k))
        if state in (U.RUNNING.value, U.DIRECTING.value):
            s = entry["session"]
            yield (f"  {unit}: {state} — {s['role']} session {s['n']}, pid {s['pid']}, "
                   f"{fmt_secs(now - s['begun'])} in, log {s['log']}"
                   + (f" ({counters})" if counters else ""))
        elif state == U.AWAITING.value:
            r = entry["run"]
            yield (f"  {unit}: awaiting {r['argv']} — pid {r['pid']}, "
                   f"{fmt_secs(now - r['begun'])} of {fmt_secs(r['budget_s'])}, log {r['log']}")
        elif state:
            yield f"  {unit}: {state}"
        elif counters or entry.get("parked"):
            yield f"  {unit}: {'parked, ' if entry.get('parked') else ''}{counters}"
    for unit in sorted(parked_ids(paths)):
        yield f"  parked {unit}: {first_line(paths.p(paths.parked) / f'{unit}.md')[:120]}"


def cmd_status(args):
    for line in status_lines(paths_for(args), args.label, args.pool):
        print(line)
    return 0


def follow(paths, label, *, unit=None, lines=40, out=sys.stdout, poll=1.0, sleep=time.sleep,
           stop=lambda: False):
    """Status, then the pipe: the log of every unit the loop holds, followed
    as it grows. When a unit's session ends and its next session or its run
    starts, the pipe switches to that log, with a `==` line saying so; a line
    from the log is prefixed `[unit]` unless one unit was asked for. The
    ledger is the loop's own record, read, never written."""
    for line in status_lines(paths, label):
        out.write(line + "\n")
    path = ledger_for(paths, label)
    tracked, pending, first, beat = {}, {}, True, None
    while True:
        try:
            entries = Ledger(path).units if path.exists() else {}
        except (OSError, ValueError):
            entries = None              # mid-write on a full disk; read it next poll
        if entries is not None:
            try:
                now_beat = paths.p(paths.heartbeat).read_text().split()[1:2]
            except OSError:
                now_beat = []
            if now_beat != beat:
                beat = now_beat
                out.write(f"== loop: {heartbeat_line(paths)} ==\n")
            logs = active_logs(entries)
            if unit:
                logs = {u: v for u, v in logs.items() if u == unit}
            for gone in [u for u in tracked if u not in logs]:
                out.write(f"== {gone}: left {tracked.pop(gone)[0]} ==\n")
                pending.pop(gone, None)
            for u, (state, what, log) in logs.items():
                if tracked.get(u, (None, None, None))[2] == log:
                    continue
                out.write(f"== {u}: {state} — {what} · {log} ==\n")
                offset = 0
                if first:               # attaching: show the tail, not the whole transcript
                    try:
                        text = pathlib.Path(log).read_bytes()
                        offset = len(text) - len(b"".join(text.splitlines(True)[-lines:]))
                    except OSError:
                        pass
                tracked[u] = (state, offset, log)
                pending[u] = b""
            first = False
            for u, (state, offset, log) in list(tracked.items()):
                try:
                    with open(log, "rb") as fh:
                        fh.seek(offset)
                        data = fh.read()
                except OSError:
                    continue
                tracked[u] = (state, offset + len(data), log)
                *done, pending[u] = (pending[u] + data).split(b"\n")
                prefix = "" if unit else f"[{u}] "
                for raw in done:
                    out.write(prefix + raw.decode(errors="replace") + "\n")
            out.flush()
        if stop():
            return 0
        sleep(poll)


def cmd_follow(args):
    paths = paths_for(args)
    try:
        return follow(paths, args.label, unit=args.unit, lines=args.lines,
                      stop=(lambda: True) if args.no_follow else (lambda: False))
    except KeyboardInterrupt:
        return 0


def cmd_plan(args):
    paths = paths_for(args)
    queue = Queue(paths.p(paths.state))
    models = resolve_models(args, paths)
    held = held_ids(paths, queue)
    unit = queue.current(held)
    print(f"prompt: {prompt_text(paths)[1]}  queue: {paths.state}")
    every = paths.manifest.audit_every if paths.manifest else None
    print(f"units since audit: {units_since_audit(paths, queue)}"
          f"{f' (audit every {every})' if every else ''}  head: {head_of(paths.workdir)[:9]}")
    print(f"done: {queue.done_count()}/{len(queue.rows)}")
    waiting = queue.awaiting_operator(held)
    if waiting:
        print(f"waiting on the operator (the loop runs past them): {', '.join(waiting)}")
    beyond = sorted(out_of_scope(paths, queue))
    if beyond:
        print(f"outside the frozen scope ({paths.manifest.scope_file}): {', '.join(beyond)}")
    if unit is None:
        print("no ready unit")
        return 0
    routed = select_model_args(unit, models["MODEL"], models["REVIEW_MODEL"], models["VARIANT"])
    print(f"unit {unit.id} — {' '.join(routed) or 'configured default'}")
    refusal = dispatch_refusal(paths, queue, unit)
    if refusal is not None:
        print(f"refused at dispatch: {refusal}")
    requires = paths.manifest.dispatch_requires if paths.manifest else ()
    if requires:
        unmet = [r.id for r in queue.rows if r.status is not Status.DONE
                 and queue.unmet_requirements(r, requires)]
        print(f"open rows lacking {', '.join(requires)}: {len(unmet)}"
              + (f" ({', '.join(unmet)})" if unmet else ""))
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

    def common(p):
        queue_flag(p)
        p.add_argument("--prompt", default=None, help="default: ralph/PROMPT.md")
        p.add_argument("--state", default=None, help="default: ralph/STATE.md")
        p.add_argument("--session-timeout", type=int, default=None, help="default: 3600")
        p.add_argument("--marker-timeout", type=int, default=None,
                       help="the longest budget a session may give `ralph-result await` "
                            f"(default {AWAIT_MAX_S}s)")
        p.add_argument("--notify", action="store_true",
                       help="accepted for old launch lines; the loop notifies unless --no-notify")
        p.add_argument("--no-notify", action="store_true")
        p.add_argument("--model", default="")
        p.add_argument("--review-model", default="")
        p.add_argument("--resolve-model", default="")
        p.add_argument("--variant", default="")
        p.add_argument("--install-launchd", "--install-job", dest="install_launchd",
                       action="store_true", help="launchd on macOS, systemd-run --user on Linux")

    p = sub.add_parser("run", help="the serial loop: one unit at a time, in the main tree")
    common(p)
    p.add_argument("--max-stall", type=int, default=MAX_STRIKES,
                   help="strikes before a unit escalates")
    p.add_argument("--max-iter", type=int, default=None, help="ignored (old launch lines)")
    p.set_defaults(fn=cmd_run)

    p = sub.add_parser("pool", help="the parallel loop: lanes in worktrees, reviews in the "
                                    "main tree")
    common(p)
    p.add_argument("--lanes", type=int, default=2)
    p.add_argument("--max-lane-failures", type=int, default=MAX_STRIKES,
                   help="strikes before a unit escalates")
    p.add_argument("--conflicts", default=None, help="default: ralph/conflicts.txt")
    p.add_argument("--base-branch", default="")
    p.set_defaults(fn=cmd_pool)

    p = sub.add_parser("supervise", help="retired: runs (or installs) the command after --")
    queue_flag(p)
    for flag in ("--prompt", "--state", "--charter", "--resolve-model", "--resolve-variant",
                 "--model", "--review-model", "--variant"):
        p.add_argument(flag, default=None)
    p.add_argument("--session-timeout", type=int, default=None)
    p.add_argument("--marker-timeout", type=int, default=None)
    p.add_argument("--resolve-max", type=int, default=None)
    p.add_argument("--notify", action="store_true")
    p.add_argument("--install-launchd", "--install-job", dest="install_launchd",
                   action="store_true")
    p.add_argument("campaign", nargs=argparse.REMAINDER)
    p.set_defaults(fn=cmd_supervise)

    p = sub.add_parser("result", help="how a session ends (run as `ralph-result`)")
    kinds = p.add_subparsers(dest="kind", required=True)
    kinds.add_parser("done")
    k = kinds.add_parser("continue")
    k.add_argument("note", nargs="*")
    k = kinds.add_parser("await")
    k.add_argument("budget")
    k.add_argument("argv", nargs=argparse.REMAINDER)
    k = kinds.add_parser("needs-human")
    k.add_argument("--operator", action="store_true",
                   help="a fork the charter leaves to the operator: no director is sent")
    k.add_argument("--package", default="", help="a file holding the evidence")
    k.add_argument("why", nargs="*")
    p.set_defaults(fn=cmd_result)

    p = sub.add_parser("status")
    queue_flag(p)
    p.add_argument("--pool", action="store_true", help="preview a pool's adoption")
    p.set_defaults(fn=cmd_status)

    p = sub.add_parser("follow", help="status, then the live output of the units the loop "
                                      "holds")
    queue_flag(p)
    p.add_argument("--unit", default=None, help="follow one row only")
    p.add_argument("--lines", type=int, default=40, help="lines of each log shown on attach")
    p.add_argument("--no-follow", action="store_true", help="print once and exit")
    p.set_defaults(fn=cmd_follow)

    p = sub.add_parser("watch")
    queue_flag(p)
    p.add_argument("--install-launchd", "--install-job", dest="install_launchd",
                   action="store_true", help="launchd on macOS, systemd-run --user on Linux")
    p.set_defaults(fn=cmd_watch)

    p = sub.add_parser("stop")
    queue_flag(p)
    p.add_argument("--drain", action="store_true",
                   help="let running sessions finish; start nothing new")
    p.add_argument("--timeout", type=int, default=180,
                   help="seconds to wait for the loop to go down before suggesting --hard")
    p.add_argument("--hard", action="store_true",
                   help="boot the job out and end its sessions and runs")
    p.set_defaults(fn=cmd_stop)

    p = sub.add_parser("start")
    queue_flag(p)
    p.set_defaults(fn=cmd_start)

    p = sub.add_parser("unpark")
    queue_flag(p)
    p.add_argument("row")
    p.set_defaults(fn=cmd_unpark)

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
    queue_flag(p)
    p.add_argument("--model", default="")
    p.add_argument("--review-model", default="")
    p.add_argument("--resolve-model", default="")
    p.add_argument("--variant", default="")
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
