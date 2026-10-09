#!/usr/bin/env python3
"""The loop's machine: its table, its one result verb, and every past
incident as a scenario row (docs/RALPH_STATE_MACHINE.md).

A Loop is driven tick by tick against real git repos in temp dirs. Sessions
and background runs are fakes: a session's scenario runs in its worktree when
the loop spawns it (it commits, and reports through the real `result` verb),
and the fake then reports the process alive for as many ticks as the scenario
asked. The process primitives themselves are tested against real processes.
"""
import contextlib
import io
import json
import os
import pathlib
import plistlib
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
import ralph  # noqa: E402

U, E = ralph.U, ralph.E


def write(root, rel, text):
    p = pathlib.Path(root) / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)
    return p


def git(cwd, *args):
    r = subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise AssertionError(f"git {' '.join(args)}: {r.stderr}")
    return r.stdout.strip()


PROMPT = "Do your unit. End with ralph-result as the note above says.\n"


def make_repo(tmp, state, prompt=PROMPT, extra=None):
    wd = pathlib.Path(tmp) / "repo"
    wd.mkdir()
    git(wd, "init", "-q", "-b", "main")
    git(wd, "config", "user.email", "t@example.invalid")
    git(wd, "config", "user.name", "t")
    write(wd, ".gitignore", "target/\n")
    write(wd, "ralph/STATE.md", state)
    write(wd, "ralph/PROMPT.md", prompt)
    for rel, text in (extra or {}).items():
        write(wd, rel, text)
    git(wd, "add", "-A")
    git(wd, "commit", "-q", "-m", "init")
    ralph.ensure_excludes(wd, ralph.runtime_markers(ralph.Paths(wd)))   # as cmd_run does
    return wd


class Clock:
    def __init__(self):
        self.t = 1_000_000.0

    def __call__(self):
        return self.t

    def advance(self, secs):
        self.t += secs


class Session:
    """What a fake session sees, and the two things it does."""

    def __init__(self, argv, cwd, env):
        self.argv, self.cwd, self.env = argv, pathlib.Path(cwd), env
        self.prompt = argv[-1]

    def commit(self, rel="work.txt", text=None, msg="work"):
        p = self.cwd / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text if text is not None else (p.read_text() if p.exists() else "") + "x\n")
        git(self.cwd, "add", "--", rel)
        git(self.cwd, "commit", "-q", "-m", msg)

    def result(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with mock.patch.dict(os.environ, self.env), contextlib.redirect_stdout(out), \
                contextlib.redirect_stderr(err):
            rc = ralph.main(["result", *args])
        self.last = (rc, out.getvalue(), err.getvalue())
        return rc


FOREVER = "forever"      # a scenario's lifetime: alive until the loop kills it


class Procs:
    """spawn / alive / kill for the Loop. A session spawn runs the next
    scenario at once; a run spawn the next run scenario. Each reports alive
    for the ticks it returned (FOREVER: until killed; nothing: it has ended)."""

    def __init__(self):
        self.pid = 70000
        self.life = {}
        self.sessions, self.runs = [], []
        self.spawned, self.killed, self.finals = [], [], {}

    def spawn(self, argv, *, cwd, env, log_path, append=False):
        self.pid += 1
        pathlib.Path(log_path).parent.mkdir(parents=True, exist_ok=True)
        pathlib.Path(log_path).write_text("")
        self.spawned.append((argv, pathlib.Path(cwd), env, pathlib.Path(log_path)))
        if argv[0] == "/bin/sh":
            scenario = self.runs.pop(0) if self.runs else (lambda *a: (FOREVER, None))
            ticks, code = scenario(argv[4:], pathlib.Path(cwd), env)
            if code is not None:
                self.finals[self.pid] = (env["RALPH_EXIT"], code)
        else:
            scenario = self.sessions.pop(0) if self.sessions else (lambda s: 0)
            ticks = scenario(Session(argv, cwd, env))
        self.life[self.pid] = None if ticks == FOREVER else int(ticks or 0)
        return ralph.Proc(self.pid, "fake")

    def alive(self, proc):
        n = self.life.get(proc.pid, 0)
        if n is None:
            return True
        if n > 0:
            self.life[proc.pid] = n - 1
            return True
        final = self.finals.pop(proc.pid, None)
        if final:
            pathlib.Path(final[0]).write_text(f"{final[1]}\n")
        return False

    def kill(self, proc):
        self.killed.append(proc.pid)
        self.finals.pop(proc.pid, None)
        self.life[proc.pid] = 0

    def session_argvs(self):
        return [a for a, *_ in self.spawned if a[0] != "/bin/sh"]


def manifest_stub(worker_bin):
    """The queue manifest the loop reads, shaped as the loop touches it: a
    declared worker_bin, no dispatch census, no audit cadence, no freeze."""
    return mock.Mock(worker_bin=worker_bin, settings="", dispatch_requires=(),
                     audit_every=None, scope_file="", path="ralph/next/t/queue.toml")


class Rig:
    """A Loop over a temp repo, with every seam faked."""

    def __init__(self, tmp, state, *, lane_mode=False, lanes=1, charter=None, prompt=PROMPT,
                 models=None, probe=None, disk=None, extra=None, manifest=None, **kw):
        self.tmp = pathlib.Path(tmp)
        self.wd = make_repo(tmp, state, prompt, extra)
        self.procs, self.clock, self.notes = Procs(), Clock(), []
        self.disk = disk or (lambda: 100)
        self.probe = probe or (lambda m: (True, ""))
        self.manifest = manifest
        self.kw = dict(label="t", lanes=lanes, lane_mode=lane_mode, base_branch="main",
                       charter=charter, models=models or {"MODEL": "prov/m"},
                       state_dir=self.tmp / "state", lane_root=self.tmp / "lanes", **kw)
        self.loop = self.new_loop()

    def new_loop(self):
        return ralph.Loop(
            ralph.Paths(self.wd, manifest=self.manifest), spawn=self.procs.spawn, alive=self.procs.alive,
            kill=self.procs.kill, clock=self.clock, sleep=lambda s: FOREVER,
            probe=lambda m: self.probe(m), jobs_share=lambda n: (2, "test budget"),
            disk_free_gb=lambda: self.disk(), code_digest=lambda: "same",
            notifier=lambda title, body, enabled: self.notes.append((title, body)),
            **self.kw)

    def tick(self, n=1):
        out = None
        for _ in range(n):
            out = self.loop.tick()
            self.clock.advance(30)
        return out

    def state(self, unit):
        return self.loop.state(unit)

    def row(self, unit):
        return ralph.Queue(self.wd / "ralph/STATE.md").by_id()[unit]

    def entry(self, unit):
        return self.loop.ledger.units.get(unit, {})

    def titles(self):
        return [t for t, _ in self.notes]


def done(s):
    s.commit()
    assert s.result("done") == 0, s.last
    return 0


# ---------------------------------------------------------------------------
# The table.


class TableTests(unittest.TestCase):
    def test_every_pair_is_defined_or_impossible_with_a_reason(self):
        for state in U:
            for event in E:
                defined = (state, event) in ralph.TRANSITIONS
                impossible = (state, event) in ralph.IMPOSSIBLE
                self.assertTrue(defined != impossible,
                                f"({state.value}, {event.value}) must be in exactly one")
                if impossible:
                    self.assertTrue(ralph.IMPOSSIBLE[(state, event)].strip())

    def test_every_action_is_a_loop_method(self):
        for (state, event), (to, action) in ralph.TRANSITIONS.items():
            self.assertIsInstance(to, U)
            if action:
                self.assertTrue(hasattr(ralph.Loop, f"_do_{action}"), action)

    def test_every_waiting_state_has_an_owned_or_operator_exit(self):
        # No waiting state is left by a file an agent writes, or by a clock alone.
        for state in U:
            if state is U.DONE:
                continue
            kind, how = ralph.WAITING_EXITS[state]
            self.assertIn(kind, ("owned", "operator"), state)
            leaves = {to for (s, e), (to, _) in ralph.TRANSITIONS.items() if s is state and to is not s}
            self.assertTrue(leaves, f"{state.value} has no transition out")

    def test_every_active_state_is_left_by_an_event_of_its_own_process(self):
        own = {U.RUNNING: {E.RESULT_DONE, E.NO_RESULT_NONE}, U.DIRECTING: {E.NO_RESULT_NONE},
               U.AWAITING: {E.RUN_ENDED, E.RUN_OVER_BUDGET}, U.MERGING: {E.MERGE_CLEAN}}
        for state, events in own.items():
            for event in events:
                to, _ = ralph.TRANSITIONS[(state, event)]
                self.assertIsNot(to, state)

    def test_a_static_path_joins_every_pair_of_static_states(self):
        static = (U.PENDING, U.READY, U.HELD, U.DONE)
        for a in static:
            for b in static:
                path = ralph.static_path(a, b)
                state = a
                for event in path:
                    state = ralph.TRANSITIONS[(state, event)][0]
                self.assertIs(state, b)

    def test_the_loop_refuses_a_pair_outside_the_table(self):
        with tempfile.TemporaryDirectory() as tmp:
            rig = Rig(tmp, "- [ ] a — depends []\n- [ ] b — depends [a]\n")
            rig.procs.sessions.append(lambda s: FOREVER)
            rig.tick()
            self.assertIs(rig.state("b"), U.PENDING)
            with self.assertRaisesRegex(RuntimeError, "only a session the loop holds"):
                rig.loop.fire("b", E.RESULT_DONE, result={})


# ---------------------------------------------------------------------------
# The one result verb.


class ResultVerbTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.wd = make_repo(self.tmp.name, "- [ ] a — depends []\n")
        base = pathlib.Path(self.tmp.name) / "base.json"
        base.write_text(json.dumps({"head": "", "untracked": []}))
        self.out = pathlib.Path(self.tmp.name) / "result.json"
        self.s = Session(["x"], self.wd, {"RALPH_RESULT": str(self.out), "RALPH_BASE": str(base),
                                          "RALPH_WORKSPACE": str(self.wd),
                                          "RALPH_AWAIT_MAX": "86400"})

    def tearDown(self):
        self.tmp.cleanup()

    def recorded(self):
        return json.loads(self.out.read_text())

    def test_outside_a_session_it_refuses(self):
        s = Session(["x"], self.wd, {"RALPH_RESULT": ""})
        self.assertEqual(s.result("done"), 2)
        self.assertIn("not inside a ralph session", s.last[2])

    def test_done_refuses_a_dirty_tree_and_names_what_is_dirty(self):
        write(self.wd, "stray.txt", "x")
        self.assertEqual(self.s.result("done"), 2)
        self.assertIn("stray.txt", self.s.last[2])
        self.assertFalse(self.out.exists())
        git(self.wd, "add", "stray.txt")
        git(self.wd, "commit", "-q", "-m", "keep it")
        self.assertEqual(self.s.result("done"), 0)
        self.assertEqual(self.recorded()["kind"], "done")

    def test_untracked_files_from_before_the_session_are_not_its_own(self):
        write(self.wd, "old.log", "x")
        base = pathlib.Path(self.s.env["RALPH_BASE"])
        base.write_text(json.dumps({"head": "", "untracked": ["old.log"]}))
        self.assertEqual(self.s.result("done"), 0, self.s.last)

    def test_one_result_per_session(self):
        self.assertEqual(self.s.result("continue", "half"), 0)
        self.assertEqual(self.s.result("done"), 2)
        self.assertIn("already reported (continue)", self.s.last[2])
        self.assertEqual(self.recorded()["note"], "half")

    def test_await_validates_budget_and_command(self):
        self.assertEqual(self.s.result("await", "soon", "--", "true"), 2)
        self.assertIn("not a duration", self.s.last[2])
        self.assertEqual(self.s.result("await", "3d", "--", "true"), 2)
        self.assertIn("exceeds this loop's cap", self.s.last[2])
        self.assertEqual(self.s.result("await", "1h", "--"), 2)
        self.assertIn("no command", self.s.last[2])
        self.assertEqual(self.s.result("await", "1h", "--", "no-such-tool-xyz"), 2)
        self.assertIn("neither on PATH", self.s.last[2])
        self.assertEqual(self.s.result("await", "90m", "--", "sh", "-c", "exit 3"), 0)
        self.assertEqual(self.recorded()["budget_s"], 5400)
        self.assertEqual(self.recorded()["argv"], ["sh", "-c", "exit 3"])

    def test_needs_human_wants_a_reason_and_reads_its_package(self):
        self.assertEqual(self.s.result("needs-human"), 2)
        write(self.wd, "pkg.md", "# options\n")
        self.assertEqual(self.s.result("needs-human", "--package", "pkg.md", "--operator",
                                       "pick", "one"), 0)
        rec = self.recorded()
        self.assertEqual((rec["why"], rec["operator"], rec["package"]),
                         ("pick one", True, "# options\n"))

    def test_the_wrapper_runs_the_loops_own_file(self):
        wrapper = ralph.RALPH_BIN / "ralph-result"
        env = {**os.environ, **self.s.env, "RALPH_PY": str(pathlib.Path(ralph.__file__))}
        r = subprocess.run([str(wrapper), "continue", "via", "path"], env=env,
                           capture_output=True, text=True, cwd=self.wd)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.recorded()["note"], "via path")


# ---------------------------------------------------------------------------
# The process primitives, against real processes.


class ProcTests(unittest.TestCase):
    def test_a_spawned_group_is_alive_until_killed(self):
        with tempfile.TemporaryDirectory() as tmp:
            proc = ralph.proc_spawn(["sleep", "30"], cwd=tmp, env=dict(os.environ),
                                    log_path=pathlib.Path(tmp) / "log")
            self.assertTrue(proc.started)
            self.assertIs(ralph.proc_alive(proc), True)
            ralph.proc_kill(proc, grace=2)
            self.assertIs(ralph.proc_alive(proc), False)

    def test_a_reused_pid_is_not_the_recorded_process(self):
        # The start time names the process: a pid alone is reused after a reboot.
        stranger = ralph.Proc(os.getpid(), "Thu Jan  1 00:00:00 1970")
        self.assertIs(ralph.proc_alive(stranger), False)
        me = ralph.Proc(os.getpid(), ralph.proc_start(os.getpid()))
        self.assertIs(ralph.proc_alive(me), True)

    def test_the_await_wrapper_records_any_exit_code(self):
        with tempfile.TemporaryDirectory() as tmp:
            exit_file = pathlib.Path(tmp) / "x.exit"
            proc = ralph.proc_spawn(["/bin/sh", "-c", ralph.AWAIT_SH, "ralph-await", "sh", "-c",
                                     "echo out; exit 3"], cwd=tmp,
                                    env={**os.environ, "RALPH_EXIT": str(exit_file)},
                                    log_path=pathlib.Path(tmp) / "log")
            deadline = time.time() + 10
            while ralph.proc_alive(proc) and time.time() < deadline:
                time.sleep(0.05)
            self.assertEqual(exit_file.read_text().strip(), "3")
            self.assertIn("out", (pathlib.Path(tmp) / "log").read_text())

    def test_a_killed_run_leaves_no_exit_code(self):
        with tempfile.TemporaryDirectory() as tmp:
            exit_file = pathlib.Path(tmp) / "x.exit"
            proc = ralph.proc_spawn(["/bin/sh", "-c", ralph.AWAIT_SH, "ralph-await", "sleep", "30"],
                                    cwd=tmp, env={**os.environ, "RALPH_EXIT": str(exit_file)},
                                    log_path=pathlib.Path(tmp) / "log")
            ralph.proc_kill(proc, grace=2)
            self.assertFalse(exit_file.exists())

    def test_durations(self):
        self.assertEqual([ralph.parse_duration(t) for t in ("90m", "6h", "2d", "45", "x", "")],
                         [5400, 21600, 172800, 45, None, None])


# ---------------------------------------------------------------------------
# The serial loop.


class SerialTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.tmp.cleanup()

    def rig(self, state="- [ ] a — depends []\n- [ ] b — depends [a]\n", **kw):
        return Rig(self.tmp.name, state, **kw)

    def test_a_unit_reported_done_is_marked_with_its_commit_and_the_queue_ends_done(self):
        rig = self.rig()
        rig.procs.sessions += [done, done]
        rig.tick()
        self.assertIs(rig.state("a"), U.RUNNING)
        self.assertIs(rig.state("b"), U.PENDING)
        rig.tick()
        row = rig.row("a")
        self.assertIs(row.status, ralph.Status.DONE)
        mark = git(rig.wd, "log", "--format=%H", "--grep=^ralph: a done$")
        self.assertTrue(git(rig.wd, "rev-parse", f"{mark}^").startswith(row.hash))
        self.assertIs(rig.state("b"), U.RUNNING)
        self.assertIs(rig.tick(), ralph.Pool.DONE)
        self.assertTrue((rig.wd / "ralph/DONE").exists())
        self.assertIn("DONE — campaign complete", rig.titles())

    def test_the_session_is_told_its_unit_and_the_contract(self):
        rig = self.rig()
        rig.procs.sessions.append(done)
        rig.tick()
        prompt = rig.procs.session_argvs()[0][-1]
        self.assertIn("Your unit: a", prompt)
        self.assertIn("HOW THIS SESSION ENDS", prompt)
        self.assertTrue(prompt.endswith(PROMPT))
        env = rig.procs.spawned[0][2]
        self.assertTrue(env["PATH"].startswith(str(ralph.RALPH_BIN)))
        self.assertEqual(env["RALPH_UNIT"], "a")

    def test_the_one_shot_contract_rides_in_the_prompt_every_client_gets(self):
        # FAILING INPUT: the one-shot note lived only in the claude shim's
        # --append-system-prompt, so a battery worker (opencode) was never
        # told the process dies with its background tasks and an uncommitted
        # turn is lost. The note is the CONTRACT's now, in every prompt.
        rig = self.rig()
        rig.procs.sessions.append(done)
        rig.tick()
        prompt = rig.procs.session_argvs()[0][-1]
        self.assertIn("one-shot", prompt)
        self.assertIn("Commit before you end your turn", prompt)

    def test_a_continue_note_reaches_the_next_session(self):
        rig = self.rig()

        def cont(s):
            s.commit()
            s.result("continue", "the", "parser", "is", "half", "done")

        rig.procs.sessions += [cont, done]
        rig.tick(2)
        self.assertIn("the parser is half done", rig.procs.session_argvs()[1][-1])
        self.assertEqual(rig.entry("a").get("strikes"), 0)

    def test_strikes_escalate_to_held_without_a_charter_and_the_rest_runs(self):
        rig = self.rig("- [ ] a — depends []\n- [ ] c — depends []\n")
        nothing = lambda s: 0          # noqa: E731 — ends with no result and no commit
        rig.procs.sessions += [nothing] * 3 + [done]
        rig.tick(3)                 # an end is seen and the unit re-dispatched in one tick
        self.assertEqual(rig.entry("a")["strikes"], 2)
        rig.tick()
        self.assertEqual(rig.entry("a")["strikes"], 3)
        self.assertIs(rig.state("a"), U.HELD)
        self.assertTrue((rig.wd / "ralph/parked/a.md").exists())
        self.assertIn("struck out", (rig.wd / "ralph/parked/a.md").read_text())
        self.assertIs(rig.state("c"), U.RUNNING)
        self.assertIn("OPERATOR — held, loop continues", rig.titles())

    def test_unparking_resets_the_strikes(self):
        rig = self.rig("- [ ] a — depends []\n")
        rig.procs.sessions += [lambda s: 0] * 3
        rig.tick(5)
        self.assertIs(rig.state("a"), U.HELD)
        self.assertIs(rig.loop.tick(), ralph.Pool.STUCK)
        (rig.wd / "ralph/parked/a.md").unlink()
        rig.procs.sessions.append(done)
        rig.tick()
        self.assertEqual(rig.entry("a")["strikes"], 0)
        self.assertIs(rig.state("a"), U.RUNNING)

    def test_needs_human_holds_the_row_with_its_package(self):
        rig = self.rig("- [ ] a — depends []\n- [ ] c — depends []\n")

        def ask(s):
            write(s.cwd, "pkg.md", "A or B: A costs 2h, B costs a test.\n")
            s.result("needs-human", "--package", "pkg.md", "pick", "A", "or", "B")

        rig.procs.sessions += [ask, done]
        rig.tick(2)
        self.assertIs(rig.state("a"), U.HELD)
        package = (rig.wd / "ralph/parked/a.md").read_text()
        self.assertIn("pick A or B", package)
        self.assertIn("A costs 2h", package)
        self.assertIs(rig.state("c"), U.RUNNING)

    def test_with_a_charter_a_worker_question_goes_to_the_director_first(self):
        rig = self.rig("- [ ] a — depends []\n", charter="decide row order\n",
                       models={"MODEL": "prov/w", "RESOLVE_MODEL": "prov/r"})

        def ask(s):
            s.result("needs-human", "which", "option")

        def direct(s):
            s.commit("row-fix.txt")
            s.result("continue", "take", "option", "A")

        rig.procs.sessions += [ask, direct, done]
        rig.tick(2)
        self.assertIs(rig.state("a"), U.DIRECTING)
        director = rig.procs.session_argvs()[1]
        self.assertIn("DIRECTOR for unit a", director[-1])
        self.assertIn("which option", director[-1])
        self.assertIn("decide row order", director[-1])
        self.assertEqual(director[director.index("--model") + 1], "prov/r")
        rig.tick()
        self.assertIs(rig.state("a"), U.RUNNING)
        self.assertIn("the director: take option A", rig.procs.session_argvs()[2][-1])
        rig.tick()
        self.assertIs(rig.row("a").status, ralph.Status.DONE)

    def test_a_director_that_leaves_it_to_the_operator_holds_it(self):
        rig = self.rig("- [ ] a — depends []\n", charter="c\n")
        rig.procs.sessions += [lambda s: 0] * 3 + [
            lambda s: s.result("needs-human", "--operator", "the", "charter", "reserves", "it")]
        rig.tick(5)
        self.assertIs(rig.state("a"), U.HELD)
        self.assertIn("the charter reserves it", (rig.wd / "ralph/parked/a.md").read_text())
        self.assertEqual(rig.entry("a")["directed"], 1)

    def test_a_session_past_its_timeout_is_ended_and_judged_by_its_end(self):
        rig = self.rig(session_timeout=60)

        def slow(s):
            s.commit()
            return FOREVER

        rig.procs.sessions += [slow, done]
        rig.tick()
        rig.tick()                  # 30s: still inside the timeout
        self.assertIs(rig.state("a"), U.RUNNING)
        rig.tick()                  # 60s: killed; it committed, so it continues
        self.assertEqual(len(rig.procs.killed), 1)
        self.assertEqual(rig.entry("a")["continuations"], 1)
        self.assertIn("auto — session timeout", rig.titles())

    def test_operator_stop_ends_sessions_without_a_strike_and_leaves_runs(self):
        rig = self.rig("- [ ] a — depends []\n")
        rig.procs.sessions.append(lambda s: FOREVER)
        rig.tick()
        write(rig.wd, "ralph/STOP", "")
        self.assertIs(rig.tick(), ralph.Pool.STOPPED)
        self.assertIs(rig.state("a"), U.READY)
        self.assertEqual(rig.entry("a")["strikes"], 0)
        self.assertIn("operator stop ended the previous session", rig.entry("a")["notes"][0])

    def test_a_drain_lets_the_session_finish_and_starts_nothing(self):
        rig = self.rig()
        rig.procs.sessions += [lambda s: (done(s), 2)[1]]
        rig.tick()
        write(rig.wd, "ralph/STOP", "drain\n")
        self.assertIs(rig.tick(), ralph.Pool.DRAINING)
        self.assertIs(rig.tick(), ralph.Pool.DRAINING)
        self.assertIs(rig.tick(), ralph.Pool.STOPPED)
        self.assertIs(rig.row("a").status, ralph.Status.DONE)
        self.assertEqual(len(rig.procs.session_argvs()), 1)
        self.assertEqual((rig.wd / "ralph/STOP").read_text(), "")

    def test_a_queue_whose_prompt_is_on_the_file_protocol_blocks_dispatch(self):
        rig = self.rig(prompt="Write ralph/waiting naming your marker.\n")
        self.assertIs(rig.tick(), ralph.Pool.BLOCKED)
        self.assertEqual(rig.procs.spawned, [])
        self.assertIn("OPERATOR — blocked: prompt", rig.titles())
        rig.tick(3)
        self.assertEqual(rig.titles().count("OPERATOR — blocked: prompt"), 1)

    def test_a_human_row_is_held_and_stuck_is_said_once(self):
        rig = self.rig("- [ ] HUMAN-approve — depends []\n- [ ] a — depends [HUMAN-approve]\n")
        self.assertIs(rig.tick(), ralph.Pool.STUCK)
        rig.tick(3)
        self.assertEqual(rig.titles().count("OPERATOR — stuck"), 1)
        self.assertIs(rig.state("HUMAN-approve"), U.HELD)


# ---------------------------------------------------------------------------
# The pool.


class PoolTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.tmp.cleanup()

    def rig(self, state, **kw):
        return Rig(self.tmp.name, state, lane_mode=True, lanes=kw.pop("lanes", 2), **kw)

    def test_two_lanes_run_at_once_and_each_lands_on_the_base(self):
        rig = self.rig("- [ ] a — depends []\n- [ ] b — depends []\n")
        rig.procs.sessions += [lambda s: (s.commit("a.txt"), s.result("done"))[1],
                               lambda s: (s.commit("b.txt"), s.result("done"))[1]]
        rig.tick()
        self.assertEqual({rig.state("a"), rig.state("b")}, {U.RUNNING})
        cwds = {c.name for _, c, *_ in rig.procs.spawned}
        self.assertEqual(cwds, {"a", "b"})
        self.assertIs(rig.tick(), ralph.Pool.DONE)
        log = git(rig.wd, "log", "--format=%s")
        for unit in ("a", "b"):
            self.assertIn(f"merge {unit}", log)
            self.assertIn(f"ralph: {unit} done", log)
            self.assertTrue((rig.wd / f"{unit}.txt").exists())
            self.assertFalse((rig.tmp / "lanes" / unit).exists())

    def test_conflicting_and_heavy_rows_never_run_together(self):
        rig = self.rig("- [ ] a — depends []\n- [ ] b — depends []\n- [ ] c — depends []\n"
                       "- [ ] d — depends []\n",
                       extra={"ralph/conflicts.txt": "a b\n", "ralph/heavy.txt": "c\nd\n"},
                       lanes=3)
        rig.procs.sessions += [lambda s: FOREVER] * 4
        rig.tick()
        running = {u for u in "abcd" if rig.state(u) is U.RUNNING}
        self.assertEqual(running, {"a", "c"})

    def test_a_row_marked_alone_runs_by_itself(self):
        rig = self.rig("- [ ] a — depends []\n- [ ] b — depends []\n",
                       extra={"ralph/conflicts.txt": "b *\n"})
        rig.procs.sessions += [lambda s: (done(s), 1)[1], lambda s: FOREVER]
        rig.tick()
        self.assertEqual((rig.state("a"), rig.state("b")), (U.RUNNING, U.READY))
        rig.tick(3)
        self.assertIs(rig.state("b"), U.RUNNING)

    def test_a_review_lets_the_lanes_drain_then_runs_alone_in_the_main_tree(self):
        rig = self.rig("- [ ] a — depends []\n- [ ] b — depends []\n"
                       "- [ ] REVIEW-1 — depends [a]\n- [ ] c — depends []\n")
        rig.procs.sessions += [lambda s: (done(s), 1)[1], lambda s: (done(s), 4)[1],
                               done, done]
        rig.tick()
        self.assertEqual({u for u in "abc" if rig.state(u) is U.RUNNING}, {"a", "b"})
        rig.tick(2)                 # a lands; the review is ready, so c does not take its slot
        self.assertIs(rig.row("a").status, ralph.Status.DONE)
        self.assertEqual((rig.state("REVIEW-1"), rig.state("b"), rig.state("c")),
                         (U.READY, U.RUNNING, U.READY))
        rig.tick(3)                 # b lands; the review runs, alone, in the main tree
        self.assertIs(rig.state("REVIEW-1"), U.RUNNING)
        self.assertEqual(rig.procs.spawned[2][1], rig.wd)
        self.assertIs(rig.state("c"), U.READY)


# ---------------------------------------------------------------------------
# Every past incident, as a row. Each names the commit that fixed it in the
# file-protocol loop and the test that holds it here; a new incident is a new
# row, its fix a changed cell or a changed number.

INCIDENTS = [
    ("c6f375538", "a SIGKILL orphaned a lane session beside its successor",
     "test_a_signal_ends_the_sessions_and_leaves_the_runs"),
    ("07bf3ebe1", "an unparseable STATE.md killed the supervisor (~18h down)",
     "test_an_unparseable_queue_blocks_and_keeps_watching_what_runs"),
    ("adb515604", "the supervisor died of its own errors and of ENOSPC",
     "test_an_error_in_a_tick_is_said_once_and_the_loop_stays_up"),
    ("6bc367088", "a stop naming no row ended the night",
     "test_nothing_but_done_or_an_operator_stop_ends_the_loop"),
    ("a0df57d1f", "fixes never reached the running pool",
     "test_a_committed_change_redeploys_with_sessions_in_flight"),
    ("24355ab77", "finished jobs re-ran at login",
     "test_the_job_restarts_on_failure_and_stays_down_when_done"),
    ("0eec06b5e", "start killed a running job",
     "test_start_leaves_a_running_job_alone"),
    ("8e98859b3", "no cp --reflink on macOS",
     "test_the_lane_target_clone_is_the_platforms_own"),
    ("0a4289609", "waves started into a full disk",
     "test_the_disk_floor_blocks_dispatch"),
    ("db84fe84a", "the floor waited while idle lanes held the space",
     "test_the_disk_floor_reclaims_idle_lanes_first"),
    ("d7ab91fc2", "a 40GB floor sat the pool idle with room to run",
     "test_the_disk_floor_is_the_watchdogs_red_line"),
    ("a617796fe", "the serial driver halted on an all-done queue",
     "test_an_all_done_queue_ends_done"),
    ("b1d992295", "a finished queue closed unaudited",
     "test_a_closing_audit_runs_before_done"),
    ("83f56804b", "a productive lane was struck at the session cap",
     "test_a_session_that_commits_without_a_result_continues"),
    ("1a6b5cef0", "sessions that died in 30-90s struck lanes out",
     "test_a_dead_model_blocks_dispatch_instead_of_striking"),
    ("e899ef1f4", "a run killed by a reboot held its lane for days",
     "test_a_run_that_vanishes_resumes_its_unit_at_once"),
    ("f0b70b6c4", "prose on waiting's first line; a lane's own marker as the run's",
     "test_files_an_agent_writes_are_not_read"),
    ("16223550c", "a merge conflict halted the pool, 8 times",
     "test_a_merge_conflict_goes_back_to_the_lane"),
    ("2026-10-06", "r12-release-preview: the cut exited 1, its success-only marker never came",
     "test_a_run_that_fails_resumes_its_lane_with_the_code_and_the_log"),
    ("2026-10-08", "zoracite's target/ was purged overnight with an awaiting unit's run log in it",
     "test_the_loops_logs_are_never_under_target"),
    ("2026-10-08", "a 29-minute run a stopped loop saw 14h late was reported as 14h09m long",
     "test_a_run_seen_late_reports_when_it_ended"),
]


class IncidentTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.tmp.cleanup()

    def test_every_incident_has_its_row(self):
        names = {name for _, _, name in INCIDENTS}
        self.assertEqual(len(names), len(INCIDENTS))
        for name in names:
            self.assertTrue(hasattr(self, name), name)

    def test_a_signal_ends_the_sessions_and_leaves_the_runs(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n- [ ] b — depends []\n",
                  lane_mode=True, lanes=2)
        rig.procs.sessions += [lambda s: FOREVER,
                               lambda s: (s.result("await", "1h", "--", "true"), 0)[1]]
        rig.procs.runs.append(lambda argv, cwd, env: (FOREVER, None))
        rig.tick(2)
        self.assertEqual((rig.state("a"), rig.state("b")), (U.RUNNING, U.AWAITING))

        def term(_secs):
            raise ralph.Terminated(signal.SIGTERM)

        rig.loop.sleep = term
        with mock.patch.object(ralph.signal, "signal"):
            self.assertEqual(rig.loop.run(), 128 + signal.SIGTERM)
        self.assertIs(rig.state("a"), U.READY)
        self.assertEqual(rig.entry("a")["strikes"], 0)
        self.assertIs(rig.state("b"), U.AWAITING)
        session_pid = rig.entry("a")["session"]["pid"]
        self.assertIn(session_pid, rig.procs.killed)
        self.assertNotIn(rig.entry("b")["run"]["pid"], rig.procs.killed)

    def test_an_unparseable_queue_blocks_and_keeps_watching_what_runs(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")
        rig.procs.sessions.append(lambda s: (done(s), 1)[1])
        rig.tick()
        write(rig.wd, "ralph/STATE.md", "- [ ] a — depends [ghost]\n")
        self.assertIs(rig.tick(), ralph.Pool.BLOCKED)
        self.assertIs(rig.tick(), ralph.Pool.BLOCKED)
        self.assertIs(rig.state("a"), U.MERGING)      # its session's end was still observed
        self.assertIn("OPERATOR — blocked: queue", rig.titles())

    def test_an_error_in_a_tick_is_said_once_and_the_loop_stays_up(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")
        calls = {"n": 0}

        def flaky(_model):
            calls["n"] += 1
            if calls["n"] <= 3:
                raise OSError(28, "No space left on device")
            return True, ""

        rig.probe = flaky
        sleeps = []

        def sleep(_secs):
            sleeps.append(1)
            if len(sleeps) >= 5:
                raise ralph.Terminated(signal.SIGTERM)

        rig.loop.sleep = sleep
        rig.procs.sessions.append(lambda s: FOREVER)
        with mock.patch.object(ralph.signal, "signal"):
            rig.loop.run()
        self.assertEqual(rig.titles().count("OPERATOR — loop error, still running"), 1)
        self.assertEqual(len(rig.procs.session_argvs()), 1)

    def test_nothing_but_done_or_an_operator_stop_ends_the_loop(self):
        # A struck-out row, a held row and a blocked precondition leave the loop
        # up and ticking: only DONE and STOPPED return from run().
        terminal = {ralph.Pool.DONE, ralph.Pool.STOPPED}
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n- [ ] HUMAN-x — depends []\n")
        rig.procs.sessions += [lambda s: 0] * 3
        seen = {rig.tick() for _ in range(8)}
        self.assertFalse(seen & terminal, seen)
        self.assertIn(ralph.Pool.STUCK, seen)

    def test_a_committed_change_redeploys_with_sessions_in_flight(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")
        rig.procs.sessions.append(lambda s: (done(s), 2)[1])
        rig.tick()
        execs = []
        rig.loop.code_digest = lambda: "changed"
        rig.loop.committed = lambda: True
        rig.loop.compiles = lambda: None
        rig.loop.reexec = execs.append
        rig.tick()
        self.assertEqual(len(execs), 1)
        # The next generation reads the same ledger and watches the same session.
        rig.loop = rig.new_loop()
        self.assertIs(rig.state("a"), U.RUNNING)
        rig.tick(2)
        self.assertIs(rig.row("a").status, ralph.Status.DONE)
        self.assertEqual(len(rig.procs.session_argvs()), 1)

    def test_the_job_restarts_on_failure_and_stays_down_when_done(self):
        with tempfile.TemporaryDirectory() as home:
            mac = ralph.MacHost(home=home, which=lambda t: t, run=mock.Mock())
            plist = mac.install_job("dev.ralph.x-y", ["python3", "ralph.py", "run"], home,
                                    f"{home}/log", keep_alive=True)
            data = plistlib.loads(pathlib.Path(plist).read_bytes())
            self.assertEqual(data["KeepAlive"], {"SuccessfulExit": False})
            self.assertTrue(str(plist).startswith(f"{home}/.config/ralph/jobs/"))

    def test_a_bare_program_is_installed_as_the_shells_own(self):
        # launchd ran a bare `python3` as /usr/bin/python3, 3.9, not the
        # installing shell's 3.13 (svrngs u6, 2026-10-06).
        shell = {"launchctl": "/bin/launchctl", "python3": "/opt/py313/bin/python3"}
        with tempfile.TemporaryDirectory() as home:
            mac = ralph.MacHost(home=home, which=shell.get, run=mock.Mock())
            plist = mac.install_job("dev.ralph.x-y", ["python3", "ralph.py", "run"], home,
                                    f"{home}/log", keep_alive=True)
            data = plistlib.loads(pathlib.Path(plist).read_bytes())
            self.assertEqual(data["ProgramArguments"], ["/opt/py313/bin/python3", "ralph.py", "run"])
            with self.assertRaises(ralph.HostError):
                mac.install_job("dev.ralph.x-z", ["nope", "run"], home, f"{home}/log")

    def test_start_leaves_a_running_job_alone(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")
        fake = mock.Mock()
        fake.job_file.return_value = rig.tmp / "job.plist"
        (rig.tmp / "job.plist").write_text("")
        fake.job_running.return_value = True
        with mock.patch.object(ralph, "host", lambda: fake):
            rc = ralph.main(["start", "--workdir", str(rig.wd), "--label", "t"])
        self.assertEqual(rc, 0)
        fake.start_job.assert_not_called()

    def test_the_lane_target_clone_is_the_platforms_own(self):
        flag = "-c" if sys.platform == "darwin" else "--reflink=always"
        self.assertIn(flag, ralph.Lanes.CLONE_TARGET)

    def test_the_disk_floor_blocks_dispatch(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n", disk=lambda: 2)
        self.assertIs(rig.tick(), ralph.Pool.BLOCKED)
        self.assertEqual(rig.procs.spawned, [])
        self.assertIn("OPERATOR — blocked: disk", rig.titles())

    def test_the_disk_floor_reclaims_idle_lanes_first(self):
        free = {"gb": 2}
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n- [ ] b — depends []\n",
                  lane_mode=True, lanes=1, disk=lambda: free["gb"])
        idle = rig.tmp / "lanes" / "old" / "target" / "debug"
        idle.mkdir(parents=True)

        def reclaim(busy, gb):
            self.assertNotIn("old", busy)
            free["gb"] = 50
            return 50

        rig.loop.mech.reclaim_disk = reclaim
        rig.procs.sessions.append(lambda s: FOREVER)
        rig.tick()
        self.assertIs(rig.state("a"), U.RUNNING)

    def test_the_disk_floor_is_the_watchdogs_red_line(self):
        self.assertEqual(ralph.DISK_FLOOR_GB, 5)

    def test_an_all_done_queue_ends_done(self):
        rig = Rig(self.tmp.name, "- [x] a abc1234 — depends []\n")
        self.assertIs(rig.tick(), ralph.Pool.DONE)
        self.assertEqual(rig.procs.spawned, [])

    def test_a_closing_audit_runs_before_done(self):
        manifest = ('label = "q"\naudit_every = 2\n')
        rig = Rig(self.tmp.name, "", extra={
            "ralph/next/q/queue.toml": manifest,
            "ralph/next/q/STATE.md": "- [ ] a — depends []\n",
            "ralph/next/q/PROMPT.md": PROMPT})
        paths = ralph.paths_for(mock.Mock(workdir=str(rig.wd), queue="q", label=None,
                                          session_timeout=None, prompt=None, state=None,
                                          charter=None, conflicts=None))
        ralph.ensure_excludes(rig.wd, ralph.runtime_markers(paths))
        rig.loop = ralph.Loop(paths, spawn=rig.procs.spawn, alive=rig.procs.alive,
                              kill=rig.procs.kill, clock=rig.clock, sleep=lambda s: FOREVER,
                              probe=lambda m: (True, ""), jobs_share=lambda n: (2, ""),
                              disk_free_gb=lambda: 100, code_digest=lambda: "same",
                              notifier=lambda *a: rig.notes.append(a[:2]),
                              label="q", state_dir=rig.tmp / "state", lane_root=rig.tmp / "lanes")
        rig.procs.sessions += [done, done]
        rig.tick(2)
        queue = ralph.Queue(rig.wd / "ralph/next/q/STATE.md")
        audits = [r for r in queue.rows if r.id.startswith(ralph.AUDIT_PREFIX)]
        self.assertEqual(len(audits), 1)
        self.assertIsNot(rig.tick(), ralph.Pool.DONE)
        self.assertIs(rig.loop.state(audits[0].id), U.RUNNING)
        self.assertIs(rig.tick(), ralph.Pool.DONE)

    def test_a_session_that_commits_without_a_result_continues(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n", max_continuations=2)
        rig.procs.sessions += [lambda s: s.commit() or 0] * 3
        rig.tick(3)
        self.assertEqual(rig.entry("a")["continuations"], 2)
        self.assertEqual(rig.entry("a")["strikes"], 0)
        rig.tick()
        self.assertEqual(rig.entry("a")["strikes"], 1)      # past the bound, it counts

    def test_a_dead_model_blocks_dispatch_instead_of_striking(self):
        alive = {"ok": False}
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n",
                  models={"MODEL": "prov/a,prov/b"},
                  probe=lambda m: (alive["ok"], "429 usage limit"))
        self.assertIs(rig.tick(), ralph.Pool.BLOCKED)
        self.assertEqual(rig.entry("a").get("strikes", 0), 0)
        self.assertEqual(rig.procs.spawned, [])
        alive["ok"] = True
        rig.clock.advance(ralph.PROBE_RETRY_S)
        rig.procs.sessions.append(lambda s: FOREVER)
        rig.tick()
        argv = rig.procs.session_argvs()[0]
        # One probed model, never the roster as one --model (9 resolver deaths, 2026-10).
        self.assertEqual(argv[argv.index("--model") + 1], "prov/a")

    def test_a_roster_spawns_each_model_on_its_own_client(self):
        # A mixed roster: the bare id runs on the queue's worker_bin (the
        # claude shim), the provider/model id on opencode. FAILING INPUT:
        # _start_session spawned worker_bin(paths) for every roster name, so
        # the battery id was executed by the claude shim — argv[0] constant.
        shim, opencode = "/nonexistent/claude-shim", "/nonexistent/opencode"
        alive = {"claude-opus-5-5": True}

        def cont(s):
            s.commit()
            assert s.result("continue", "more") == 0, s.last
            return 0

        rig = Rig(self.tmp.name, "- [ ] a — depends []\n",
                  models={"MODEL": "claude-opus-5-5,prov/x"},
                  manifest=manifest_stub(shim),
                  probe=lambda m: (alive.get(m, m == "prov/x"), ""))
        rig.procs.sessions += [cont, cont]
        with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": opencode}):
            rig.tick()
            alive["claude-opus-5-5"] = False
            rig.clock.advance(ralph.PROBE_OK_TTL_S)
            rig.tick(2)
        argvs = rig.procs.session_argvs()
        self.assertGreaterEqual(len(argvs), 2, argvs)
        self.assertEqual(argvs[0][0], shim)
        self.assertEqual(argvs[0][argvs[0].index("--model") + 1], "claude-opus-5-5")
        self.assertEqual(argvs[1][0], opencode)
        self.assertEqual(argvs[1][argvs[1].index("--model") + 1], "prov/x")

    def test_a_director_dispatch_takes_the_resolve_roster_too(self):
        # An escalated row's director runs RESOLVE_MODEL, routed by the same
        # grammar: the parked row's director died on the weekly limit while
        # the battery was healthy (2026-10-07). FAILING INPUT: the director's
        # provider/model name spawned on the shim.
        shim, opencode = "/nonexistent/claude-shim", "/nonexistent/opencode"
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n", charter="decide",
                  max_strikes=0,
                  models={"MODEL": "prov/w", "RESOLVE_MODEL": "claude-opus-5-5,prov/x"},
                  manifest=manifest_stub(shim),
                  probe=lambda m: (m == "prov/x", "weekly limit"))
        rig.procs.sessions.append(lambda s: FOREVER)
        with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": opencode}):
            rig.tick()
        argv = rig.procs.session_argvs()[0]
        self.assertEqual(argv[0], opencode)
        self.assertEqual(argv[argv.index("--model") + 1], "prov/x")

    def test_permission_rejections_are_named_in_the_strike(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")

        def rejected(s):
            pathlib.Path(rig.procs.spawned[-1][3]).write_text("auto-rejecting Read ~/.cargo\n")
            return 0

        rig.procs.sessions.append(rejected)
        rig.tick(2)
        self.assertIn("1 permission auto-rejection", rig.entry("a")["strike_why"][0])

    def test_a_run_that_vanishes_resumes_its_unit_at_once(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")
        rig.procs.sessions += [lambda s: s.result("await", "6h", "--", "true") and 0]
        rig.procs.runs.append(lambda argv, cwd, env: (1, None))     # ends with no status
        rig.tick(2)
        self.assertIs(rig.state("a"), U.AWAITING)
        rig.tick(2)
        self.assertIs(rig.state("a"), U.RUNNING)
        self.assertIn("gone without an exit status", rig.procs.session_argvs()[1][-1])

    def test_a_run_over_its_budget_is_ended_and_struck(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")
        rig.procs.sessions += [lambda s: s.result("await", "60", "--", "true") and 0]
        rig.procs.runs.append(lambda argv, cwd, env: (FOREVER, None))
        rig.tick(2)
        rig.tick(2)
        self.assertEqual(rig.entry("a")["strikes"], 1)
        self.assertIn("passed its 1m00s budget", rig.entry("a")["strike_why"][0])
        self.assertEqual(len(rig.procs.killed), 1)

    def test_files_an_agent_writes_are_not_read(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n- [ ] b — depends []\n")

        def file_protocol(s):
            for rel in ("ralph/waiting", "ralph/NEEDS_HUMAN.md", "ralph/lanes/a.done",
                        "ralph/DONE"):
                write(s.cwd, rel, "ralph/lanes/a.done\nprose\n")
            return 0

        rig.procs.sessions += [file_protocol, lambda s: FOREVER]
        rig.tick(2)
        # No result and no commit: one strike, and the unit runs again. The
        # waiting file, the package, the lane marker and DONE mean nothing.
        self.assertEqual(rig.entry("a")["strikes"], 1)
        self.assertIs(rig.state("a"), U.RUNNING)
        self.assertFalse((rig.wd / "ralph/parked").exists())
        self.assertIsNot(rig.tick(), ralph.Pool.DONE)

    def test_a_merge_conflict_goes_back_to_the_lane(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n", lane_mode=True, lanes=1,
                  extra={"shared.txt": "base\n"})

        def edit_then_done(s):
            s.commit("shared.txt", "lane\n")
            s.result("done")

        def resolve(s):
            self.assertIn("MERGE IN PROGRESS", s.prompt)
            write(s.cwd, "shared.txt", "both\n")
            git(s.cwd, "add", "shared.txt")
            git(s.cwd, "commit", "-q", "--no-edit")
            s.result("done")

        rig.procs.sessions += [edit_then_done, resolve]
        rig.tick()
        write(rig.wd, "shared.txt", "main moved\n")
        git(rig.wd, "commit", "-q", "-am", "main moved")
        rig.tick()
        self.assertIs(rig.state("a"), U.RUNNING)       # struck, back to its lane at once
        self.assertEqual(rig.entry("a")["strikes"], 1)
        self.assertIn("merging ralph/a", rig.procs.session_argvs()[1][-1])
        rig.tick()
        self.assertIs(rig.row("a").status, ralph.Status.DONE)
        self.assertEqual(rig.entry("a"), {})           # done: its counters go with it
        self.assertEqual((rig.wd / "shared.txt").read_text(), "both\n")

    def test_a_red_merge_check_sends_the_lane_back_unmerged(self):
        # The project's own pre-merge check (ralph/merge-check, optional) runs
        # in the lane before its branch lands. Red is a strike carrying the
        # check's own words, and the base never sees the lane: ersilia merged
        # r12-step-instruction with its lint and discovery rows red on main
        # (2026-10-09) because "done" was the only guard.
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n", lane_mode=True, lanes=1)
        check = write(rig.wd, "ralph/merge-check",
                      "#!/bin/sh\n[ -f fixed.txt ] && exit 0\n"
                      "echo 'row lint failed: fmt diff in cluster.rs:700'\nexit 1\n")
        check.chmod(0o755)
        git(rig.wd, "add", "ralph/merge-check")
        git(rig.wd, "commit", "-q", "-m", "a merge check")

        def done_but_red(s):
            s.commit("work.txt", "lane\n")
            s.result("done")

        def fix(s):
            self.assertIn("ralph/merge-check", s.prompt)
            self.assertIn("fmt diff in cluster.rs:700", s.prompt)
            s.commit("fixed.txt", "fixed\n")
            s.result("done")

        rig.procs.sessions += [done_but_red, fix]
        rig.tick()
        rig.tick()
        self.assertIs(rig.state("a"), U.RUNNING)          # struck, back to its lane at once
        self.assertEqual(rig.entry("a")["strikes"], 1)
        self.assertFalse((rig.wd / "work.txt").exists())   # the base never saw the red lane
        rig.tick()
        self.assertIs(rig.row("a").status, ralph.Status.DONE)
        self.assertTrue((rig.wd / "work.txt").exists())
        self.assertTrue((rig.wd / "fixed.txt").exists())

    def test_a_run_that_fails_resumes_its_lane_with_the_code_and_the_log(self):
        rig = Rig(self.tmp.name, "- [ ] r12-release-preview — depends []\n",
                  lane_mode=True, lanes=1)

        def cut(s):
            s.commit()
            return s.result("await", "6h", "--", "sh", "-c", "exit 1")

        def judge(s):
            self.assertIn("exited 1", s.prompt)
            self.assertIn(".await.log", s.prompt)
            s.commit("fix.txt")
            s.result("done")

        rig.procs.sessions += [cut, judge]
        rig.procs.runs.append(lambda argv, cwd, env: (1, 1))
        rig.tick(2)
        self.assertIs(rig.state("r12-release-preview"), U.AWAITING)
        argv, cwd, *_ = rig.procs.spawned[-1]
        self.assertEqual(argv[4:], ["sh", "-c", "exit 1"])
        self.assertEqual(cwd, rig.tmp / "lanes" / "r12-release-preview")
        rig.tick()                                       # one tick after the run exits
        self.assertIs(rig.state("r12-release-preview"), U.RUNNING)
        rig.tick()
        self.assertIs(rig.row("r12-release-preview").status, ralph.Status.DONE)

    def test_the_loops_logs_are_never_under_target(self):
        # A host purges target/ under disk pressure; the transcript and the run
        # log a resumed session is told to read live in the control dir's log/.
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")

        def cut(s):
            s.commit()
            return s.result("await", "1h", "--", "true")

        rig.procs.sessions.append(cut)
        rig.procs.runs.append(lambda argv, cwd, env: (FOREVER, None))
        rig.tick(2)
        self.assertIs(rig.state("a"), U.AWAITING)
        logs = [log.relative_to(rig.wd).as_posix() for *_, log in rig.procs.spawned]
        self.assertEqual(logs, ["ralph/log/sessions/a-1.out", "ralph/log/sessions/a-1.await.log"])
        self.assertEqual(rig.entry("a")["run"]["log"], str(rig.wd / logs[1]))

    def test_a_run_seen_late_reports_when_it_ended(self):
        # The loop was stopped while zoracite's demo ran; the next start told the
        # session the run took 14h09m. Its exit file says when it ended.
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")

        def cut(s):
            s.commit()
            return s.result("await", "1h", "--", "true")

        def judge(s):
            self.assertIn("exited 0 after 29m00s (the loop saw it 13h40m later)", s.prompt)
            s.result("done")

        rig.procs.sessions += [cut, judge]
        rig.procs.runs.append(lambda argv, cwd, env: (FOREVER, None))
        rig.tick(2)
        run = rig.entry("a")["run"]
        exit_file = write(run["exit_file"], "", "0\n")
        os.utime(exit_file, (run["begun"] + 29 * 60,) * 2)
        rig.clock.t = run["begun"] + 14 * 3600 + 9 * 60
        rig.tick(3)
        self.assertIs(rig.row("a").status, ralph.Status.DONE)


# ---------------------------------------------------------------------------
# End to end: real processes, the real wrapper on the session's PATH.

WORKER = r"""#!/bin/sh
# A stand-in worker binary: `<bin> run [--model M] [--variant V] <prompt>`.
for prompt; do :; done
n=$(ls "$RALPH_WORKSPACE"/turn-* 2>/dev/null | wc -l | tr -d ' ')
echo "turn $n" > "$RALPH_WORKSPACE/turn-$n"
git -C "$RALPH_WORKSPACE" add "turn-$n" && git -C "$RALPH_WORKSPACE" commit -q -m "turn $n"
case "$prompt" in
  *"exited 1"*) ralph-result done ;;
  *) ralph-result await 5m -- sh -c 'echo the cut refused: dirty tree; exit 1' ;;
esac
"""


class EndToEndTests(unittest.TestCase):
    def test_a_run_that_fails_comes_back_to_a_real_session_with_its_code(self):
        with tempfile.TemporaryDirectory() as tmp:
            wd = make_repo(tmp, "- [ ] cut — depends []\n")
            worker = write(tmp, "bin/worker", WORKER)
            worker.chmod(0o755)
            notes = []
            loop = ralph.Loop(ralph.Paths(wd), label="e2e", state_dir=pathlib.Path(tmp) / "st",
                              lane_root=pathlib.Path(tmp) / "lanes", tick_s=0.2,
                              probe=lambda m: (True, ""), disk_free_gb=lambda: 100,
                              code_digest=lambda: "same", models={},
                              notifier=lambda t, b, e: notes.append(t))
            deadline = time.time() + 60
            with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": str(worker)}), \
                    mock.patch.object(ralph.signal, "signal"):
                loop.sleep = lambda s: (time.sleep(s), time.time() > deadline
                                        and (_ for _ in ()).throw(AssertionError("timeout")))
                self.assertEqual(loop.run(), 0)
            row = ralph.Queue(wd / "ralph/STATE.md").by_id()["cut"]
            self.assertIs(row.status, ralph.Status.DONE)
            log = (wd / "ralph/log/sessions/cut-1.await.log").read_text()
            self.assertIn("the cut refused: dirty tree", log)
            self.assertEqual(sorted(p.name for p in wd.glob("turn-*")), ["turn-0", "turn-1"])
            self.assertIn("DONE — campaign complete", notes)


class FollowTests(unittest.TestCase):
    """`ralph.py follow`: the status, then each held unit's log as it grows,
    switching logs when the unit moves from its session to its run."""

    def test_the_pipe_follows_a_unit_from_its_session_to_its_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            wd = make_repo(tmp, "- [ ] a — depends []\n")
            paths = ralph.Paths(wd)
            ledger = ralph.Ledger(pathlib.Path(tmp) / "state" / "loop.json")
            session_log = write(tmp, "s.out", "".join(f"line {i}\n" for i in range(50)))
            run_log = write(tmp, "r.log", "")
            ledger.unit("a").update(state="running", session={
                "role": "worker", "n": 1, "pid": 1, "begun": time.time(), "log": str(session_log)})
            ledger.save()
            write(wd, "ralph/.heartbeat", "1 running sessions=1 awaiting=0\n")
            steps = []

            def sleep(_):
                steps.append(1)
                if len(steps) == 1:
                    with open(session_log, "a") as fh:
                        fh.write("committed the parser\npartial")
                elif len(steps) == 2:
                    ledger.units["a"].update(state="awaiting", run={
                        "argv": ["cargo", "xtask", "cut"], "pid": 2, "budget_s": 3600,
                        "log": str(run_log), "begun": time.time()})
                    ledger.save()
                    run_log.write_text("the cut refused: dirty tree\n")
                    write(wd, "ralph/.heartbeat", "2 idle sessions=0 awaiting=1\n")

            out = io.StringIO()
            with mock.patch.object(ralph, "state_dir_for", lambda p, l: pathlib.Path(tmp) / "state"):
                ralph.follow(paths, "t", lines=3, out=out, sleep=sleep, stop=lambda: len(steps) >= 3)
            text = out.getvalue()
            self.assertIn("a: running — worker session 1", text)
            self.assertIn("[a] line 49", text)
            self.assertNotIn("line 46", text)             # attach shows the tail only
            self.assertIn("[a] committed the parser", text)
            self.assertNotIn("partial", text)              # a line is written once it ends
            self.assertIn("== a: awaiting — run ['cargo', 'xtask', 'cut']", text)
            self.assertIn("[a] the cut refused: dirty tree", text)
            self.assertIn("idle sessions=0 awaiting=1", text)
            self.assertLess(text.index("committed the parser"), text.index("the cut refused"))


# ---------------------------------------------------------------------------
# Taking over from the file protocol.


class AdoptTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.tmp.cleanup()

    def test_a_lane_waiting_on_a_marker_becomes_a_loop_owned_watcher(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n- [ ] b — depends []\n",
                  lane_mode=True, lanes=2)
        wt = rig.tmp / "lanes" / "a"
        git(rig.wd, "worktree", "add", "-q", "-b", "ralph/a", str(wt), "main")
        write(wt, "ralph/waiting", "ralph/cut6.done\nnotes: the cut writes it on every end\n")
        git(wt, "add", "-f", "ralph/waiting")
        git(wt, "commit", "-q", "-m", "waiting")
        rig.procs.runs.append(lambda argv, cwd, env: (FOREVER, None))
        rig.procs.sessions.append(lambda s: FOREVER)
        rig.loop.boot()
        self.assertIs(rig.state("a"), U.AWAITING)
        argv = rig.procs.spawned[0][0]
        self.assertIn(str(wt / "ralph/cut6.done"), argv)
        self.assertFalse((wt / "ralph/waiting").exists())
        self.assertEqual(git(wt, "status", "--porcelain"), "")
        rig.tick()
        self.assertIs(rig.state("b"), U.RUNNING)       # the rest of the pool runs

    def test_a_marker_named_only_in_the_notes_or_a_lane_marker_is_not_waited_on(self):
        rig = Rig(self.tmp.name, "- [~] a — depends []\n")
        write(rig.wd, "ralph/waiting", "the field run, then ralph/lanes/a.done\n")
        rig.loop.boot()
        self.assertIsNot(rig.state("a"), U.AWAITING)
        self.assertIn("the field run", rig.entry("a")["notes"][0])
        self.assertFalse((rig.wd / "ralph/waiting").exists())

    def test_a_halt_package_parks_the_row_it_names(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n- [ ] b — depends []\n")
        write(rig.wd, "ralph/NEEDS_HUMAN.md", "# lane b left this package\n\nwhich option?\n")
        write(rig.wd, "ralph/STOP", "halt: lane b\n")
        rig.loop.boot()
        self.assertIn("which option?", (rig.wd / "ralph/parked/b.md").read_text())
        self.assertFalse((rig.wd / "ralph/NEEDS_HUMAN.md").exists())
        self.assertFalse((rig.wd / "ralph/STOP").exists())
        rig.tick()
        self.assertIs(rig.state("b"), U.HELD)

    def test_the_pools_counters_carry_over(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")
        write(rig.wd, "target/ralph/pool-state.json",
              json.dumps({"failures": {"a": 1}, "merge_failures": {"a": 2},
                          "continuations": {"a": 4}}))
        rig.loop.boot()
        self.assertEqual((rig.entry("a")["strikes"], rig.entry("a")["continuations"]), (2, 4))
        self.assertFalse((rig.wd / "target/ralph/pool-state.json").exists())

    def test_a_stale_done_marker_is_removed(self):
        rig = Rig(self.tmp.name, "- [ ] a — depends []\n")
        write(rig.wd, "ralph/DONE", "")
        rig.loop.boot()
        self.assertFalse((rig.wd / "ralph/DONE").exists())

    def test_adoption_runs_once(self):
        rig = Rig(self.tmp.name, "- [~] a — depends []\n")
        rig.loop.boot()
        write(rig.wd, "ralph/waiting", "ralph/x.done\n")
        rig.loop = rig.new_loop()
        rig.loop.boot()
        self.assertTrue((rig.wd / "ralph/waiting").exists())


if __name__ == "__main__":
    unittest.main()
