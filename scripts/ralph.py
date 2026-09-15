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
  models     show or set ralph/models.env and kickstart the loaded job
  plan       print the queue's head and the model it routes to
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


class Status(enum.Enum):
    PENDING = " "
    ACTIVE = "~"
    DONE = "x"


class Outcome(enum.Enum):
    DONE = "done"
    OPERATOR_STOP = "operator-stop"
    NEEDS_HUMAN = "needs-human"
    OPERATOR_REQUIRED = "operator-required"
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


MODEL_KEYS = ("MODEL", "REVIEW_MODEL", "VARIANT")


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


class Session:
    """One opencode session in its own process group, with a wall-clock
    timeout, a STOP check, a heartbeat, and permission-reject detection."""

    def __init__(self, paths, *, timeout=3600, opencode=None, poll=30,
                 notifier=notify, notify_enabled=True):
        self.paths = paths
        self.timeout = timeout
        self.opencode = opencode or os.environ.get("RALPH_OPENCODE_BIN", "opencode")
        self.poll = poll
        self.notifier = notifier
        self.notify_enabled = notify_enabled

    def heartbeat(self, context):
        try:
            self.paths.p(self.paths.heartbeat).write_text(f"{int(time.time())} {context}\n")
        except OSError:
            pass

    def run(self, model_args, prompt_text, log_path):
        workdir = str(self.paths.workdir)
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
                cwd=workdir, stdout=fh, stderr=subprocess.STDOUT, start_new_session=True)
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
                 wait_poll=120, models=None, model="", review_model="", variant=""):
        self.paths = paths
        self.session_run = session_run
        self.notifier = notifier
        self.notify_enabled = notify_enabled
        self.sleep = sleep
        self.max_stall = max_stall
        self.max_iter = max_iter
        self.marker_timeout = marker_timeout
        self.wait_poll = wait_poll
        self.models = models or {}
        self.model = model
        self.review_model = review_model
        self.variant = variant

    def halt(self, reason):
        pkg = self.paths.p(self.paths.needs_human)
        pkg.parent.mkdir(parents=True, exist_ok=True)
        pkg.write_text(f"# {reason}\n\nresolve by hand, then remove "
                       f"{self.paths.stop} {self.paths.needs_human}\n")
        if not pkg.stat().st_size:
            say(f"HALT could not write {pkg} (disk full?) — no decision package exists")
            self.notifier("halt-unwritable", reason, self.notify_enabled)
        self.paths.p(self.paths.stop).write_text(f"halt: {reason}\n")
        say(f"HALT: {reason}")
        self.notifier("HALT", reason, self.notify_enabled)
        return Result(Outcome.HALT, reason)

    def wait_for_marker(self):
        waiting = self.paths.p(self.paths.waiting)
        if not waiting.exists():
            return None
        m = re.search(r"[A-Za-z0-9._/-]+\.done", waiting.read_text())
        if not m:
            say("ralph/waiting names no *.done marker — ignoring it")
            waiting.unlink()
            return None
        marker = self.paths.p(m.group(0))
        if marker.exists():
            say(f"{marker} present — resuming")
            waiting.unlink()
            return None
        age = int(time.time() - waiting.stat().st_mtime)
        if age >= self.marker_timeout:
            return self.halt(f"waiting on {marker} for {age}s (limit {self.marker_timeout}s) "
                             "— the detached run never wrote its marker")
        say(f"waiting on {marker} (no session this tick, {age}s)")
        return "wait"

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
            marker = self.wait_for_marker()
            if isinstance(marker, Result):
                return marker
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
            say(f"unit {unit.id} — model {self.model or 'default'}"
                f"{'/' + self.review_model if self.review_model else ''}, "
                f"variant {self.variant or 'default'}")
            before = head_of(self.paths.workdir)
            self.session_run(model_args, self._prompt_text(), self._log_path(iteration))
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

    def terminal_stop(self):
        if self.paths.p(self.paths.done).exists():
            say("supervisor: campaign DONE")
            self.notifier("DONE", "campaign complete", self.notify_enabled)
            return 0
        stop = self.paths.p(self.paths.stop)
        pkg = self.paths.p(self.paths.needs_human)
        if stop.exists() and not stop.stat().st_size and not (pkg.exists() and pkg.stat().st_size):
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


class Watch:
    """The watchdog conditions, in order of precedence."""

    def __init__(self, paths, *, label, running=None, disk_free_mb=None,
                 stall_secs=300, min_free_mb=5120, notifier=notify, dry=False):
        self.paths = paths
        self.label = label
        self.running = running or self._running
        self.disk_free_mb = disk_free_mb or self._disk_free_mb
        self.stall_secs = stall_secs
        self.min_free_mb = min_free_mb
        self.notifier = notifier
        self.dry = dry

    def _running(self):
        job = f"dev.ralph.{self.paths.workdir.name}-{self.label}"
        r = subprocess.run(["launchctl", "print", f"gui/{os.getuid()}/{job}"],
                           capture_output=True, text=True)
        return "state = running" in r.stdout

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
                   "ralph/log.txt")


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


def cmd_models(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    file = paths.p(paths.models)
    current = load_models(file)
    if args.model or args.review_model or args.variant:
        current["MODEL"] = args.model or current.get("MODEL", "")
        current["REVIEW_MODEL"] = args.review_model or current.get("REVIEW_MODEL", "")
        current["VARIANT"] = args.variant or current.get("VARIANT", "")
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(
            "# ralph per-host model configuration (gitignored); written by scripts/ralph.py\n"
            f"MODEL={current.get('MODEL', '')}\n"
            f"REVIEW_MODEL={current.get('REVIEW_MODEL', '')}\n"
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
    print(f"  VARIANT={current.get('VARIANT') or '<unset>'}")
    return 0


def cmd_watch(args):
    paths = Paths(pathlib.Path(args.workdir).resolve())
    state_dir = state_dir_for(paths, args.label)
    watch = Watch(paths, label=args.label, dry=os.environ.get("RALPH_WATCH_DRY") == "1")
    if args.install_launchd:
        plist = install_launchd(
            f"dev.ralphwatch.{paths.workdir.name}-{args.label}",
            ["/usr/bin/python3", str(pathlib.Path(__file__).resolve()),
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
            ["/usr/bin/python3", str(pathlib.Path(__file__).resolve()), "run",
             "--workdir", str(paths.workdir), "--label", args.label,
             "--prompt", args.prompt, "--state", args.state],
            paths.workdir, str(state_dir_for(paths, args.label) / "launchd.log"))
        print(f"wrote {plist}")
        return 0
    result = campaign.run()
    print(f"campaign: {result.outcome.value} — {result.reason}")
    return {Outcome.DONE: 0, Outcome.OPERATOR_STOP: 0, Outcome.NEEDS_HUMAN: 2,
            Outcome.OPERATOR_REQUIRED: 2, Outcome.HALT: 3}[result.outcome]


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

    def run_inner():
        subprocess.run(campaign, cwd=str(paths.workdir))

    def resolver_run(attempt, reason):
        resolve_model = args.resolve_model or args.review_model or models.get("REVIEW_MODEL", "")
        resolve_variant = args.resolve_variant or args.variant or models.get("VARIANT", "")
        model_args = select_model_args("review", resolve_model, "", resolve_variant)
        prompt = (
            f"SUPERVISOR RESOLUTION (attempt {attempt} of {args.resolve_max}).\n\n"
            f"The campaign stopped short of DONE. Reason:\n  {reason}\n\n"
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
        session.run(model_args, prompt, str(paths.workdir / "target" / "ralph"
                                           / f"supervise-{attempt}.out"))

    supervisor = Supervisor(paths, run_inner=run_inner, resolver_run=resolver_run,
                            notify_enabled=args.notify, resolve_max=args.resolve_max)
    if args.install_launchd:
        ensure_excludes(paths.workdir, RUNTIME_MARKERS)
        plist = install_launchd(
            f"dev.ralph.{paths.workdir.name}-{args.label}",
            ["/usr/bin/python3", str(pathlib.Path(__file__).resolve()), "supervise",
             "--workdir", str(paths.workdir), "--label", args.label,
             "--session-timeout", str(args.session_timeout)] + campaign,
            paths.workdir, str(state_dir_for(paths, args.label) / "launchd.log"))
        print(f"wrote {plist}")
        return 0
    return supervisor.run()


def main(argv=None):
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
    p.add_argument("--install-launchd", action="store_true")
    p.add_argument("campaign", nargs=argparse.REMAINDER)
    p.set_defaults(fn=cmd_supervise)

    p = sub.add_parser("watch")
    p.add_argument("--workdir", default=".")
    p.add_argument("--label", default="campaign")
    p.add_argument("--install-launchd", action="store_true")
    p.set_defaults(fn=cmd_watch)

    p = sub.add_parser("models")
    p.add_argument("--workdir", default=".")
    p.add_argument("--label", default="")
    p.add_argument("--model", default="")
    p.add_argument("--review-model", default="")
    p.add_argument("--variant", default="")
    p.add_argument("--no-restart", action="store_true")
    p.set_defaults(fn=cmd_models)

    p = sub.add_parser("plan")
    common(p)
    p.set_defaults(fn=cmd_plan)

    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
