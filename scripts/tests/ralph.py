#!/usr/bin/env python3
"""ralph.py — the state machine's tests, in-process (no model calls).

The FSM is exercised through its seams: session runners, clocks and
notifiers are injected, so every gate runs in milliseconds and its failing
input is explicit.
"""
import os
import pathlib
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
import ralph  # noqa: E402


def write(root, rel, text):
    p = pathlib.Path(root) / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)
    return p


class QueueTests(unittest.TestCase):
    def test_parses_both_orders_and_statuses(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md",
                  "- [x] dm-a abc1234 — depends [] — did a\n"
                  "- [x] deadbee dm-b — depends [dm-a] — legacy hash-first\n"
                  "- [~] dm-c — depends [dm-b] — active\n"
                  "- [ ] dm-d — depends [dm-c, dm-a] — pending\n")
            q = ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md")
            self.assertEqual([r.id for r in q.rows], ["dm-a", "dm-b", "dm-c", "dm-d"])
            self.assertEqual(q.by_id()["dm-b"].hash, "deadbee")
            self.assertEqual(q.done_count(), 2)
            self.assertEqual(q.current().id, "dm-c")
            self.assertFalse(q.deps_met(q.by_id()["dm-d"]))

    def test_current_prefers_first_ready_pending(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md",
                  "- [ ] dm-b — depends [dm-a]\n- [ ] dm-a — depends []\n")
            q = ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md")
            self.assertEqual(q.current().id, "dm-a")

    def test_duplicate_id_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md",
                  "- [ ] dm-a — depends []\n- [ ] dm-a — depends []\n")
            with self.assertRaises(ValueError):
                ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md")

    def test_unknown_dep_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md", "- [ ] dm-a — depends [dm-ghost]\n")
            with self.assertRaises(ValueError):
                ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md")


class ModelTests(unittest.TestCase):
    def test_load_and_review_routing(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = write(tmp, "ralph/models.env",
                      "# comment\nMODEL=w/x\nREVIEW_MODEL=r/y\nVARIANT=high\nOTHER=z\n")
            m = ralph.load_models(p)
            self.assertEqual(m, {"MODEL": "w/x", "REVIEW_MODEL": "r/y", "VARIANT": "high"})
            self.assertEqual(ralph.select_model_args("dm-x", "w", "r", "high"),
                             ["--model", "w", "--variant", "high"])
            self.assertEqual(ralph.select_model_args("REVIEW-build-x", "w", "r", "high"),
                             ["--model", "r", "--variant", "high"])


class CampaignTests(unittest.TestCase):
    def make(self, tmp, rows, *, session_run, max_stall=3, max_iter=10,
             marker_timeout=60):
        write(tmp, "ralph/STATE.md", rows)
        write(tmp, "ralph/PROMPT.md", "Execute the selected unit.")
        paths = ralph.Paths(pathlib.Path(tmp))
        return ralph.Campaign(paths, session_run=session_run, notify_enabled=False,
                              sleep=lambda s: None, max_stall=max_stall,
                              max_iter=max_iter, marker_timeout=marker_timeout)

    def test_no_ready_unit_halts(self):
        with tempfile.TemporaryDirectory() as tmp:
            c = self.make(tmp, "- [ ] dm-a — depends [dm-b]\n- [ ] dm-b — depends [dm-a]\n",
                          session_run=lambda *a: 0)
            with mock.patch.object(ralph, "head_of", return_value="a" * 40):
                result = c.run()
            self.assertEqual(result.outcome, ralph.Outcome.HALT)
            self.assertIn("no ready unit", result.reason)
            self.assertTrue((pathlib.Path(tmp) / "ralph/NEEDS_HUMAN.md").exists())

    def test_stall_halts_after_max_stall(self):
        with tempfile.TemporaryDirectory() as tmp:
            c = self.make(tmp, "- [ ] dm-a — depends []\n",
                          session_run=lambda *a: 0, max_stall=2)
            with mock.patch.object(ralph, "head_of", return_value="a" * 40):
                result = c.run()
            self.assertEqual(result.outcome, ralph.Outcome.HALT)
            self.assertIn("without a commit", result.reason)

    def test_commit_progresses_to_done(self):
        with tempfile.TemporaryDirectory() as tmp:
            heads = {"h": "a" * 40}

            def session(*a):
                heads["h"] = "b" * 40
                write(tmp, "ralph/DONE", "")
                return 0
            c = self.make(tmp, "- [ ] dm-a — depends []\n", session_run=session,
                          max_stall=1)
            with mock.patch.object(ralph, "head_of", side_effect=lambda wd: heads["h"]):
                result = c.run()
            self.assertEqual(result.outcome, ralph.Outcome.DONE)

    def test_ready_human_halts(self):
        with tempfile.TemporaryDirectory() as tmp:
            c = self.make(tmp, "- [ ] HUMAN-design-review — depends []\n",
                          session_run=lambda *a: 0)
            with mock.patch.object(ralph, "head_of", return_value="a" * 40):
                result = c.run()
            self.assertEqual(result.outcome, ralph.Outcome.HALT)
            self.assertIn("HUMAN-design-review", result.reason)

    def test_stop_file_is_operator_stop(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STOP", "")
            c = self.make(tmp, "- [ ] dm-a — depends []\n", session_run=lambda *a: 0)
            with mock.patch.object(ralph, "head_of", return_value="a" * 40):
                result = c.run()
            self.assertEqual(result.outcome, ralph.Outcome.OPERATOR_STOP)

    def test_stale_marker_halts(self):
        with tempfile.TemporaryDirectory() as tmp:
            w = write(tmp, "ralph/waiting", "never.done\n")
            old = time.time() - 7200
            os.utime(w, (old, old))
            c = self.make(tmp, "- [ ] dm-a — depends []\n", session_run=lambda *a: 0,
                          marker_timeout=60)
            result = c.run()
            self.assertEqual(result.outcome, ralph.Outcome.HALT)
            self.assertIn("never.done", result.reason)

    def test_present_marker_resumes(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/waiting", "ralph/job.done\n")
            write(tmp, "ralph/job.done", "")

            def session(*a):
                write(tmp, "ralph/DONE", "")
                return 0
            c = self.make(tmp, "- [ ] dm-a — depends []\n", session_run=session)
            with mock.patch.object(ralph, "head_of", return_value="a" * 40):
                result = c.run()
            self.assertEqual(result.outcome, ralph.Outcome.DONE)
            self.assertFalse((pathlib.Path(tmp) / "ralph/waiting").exists())


class SupervisorTests(unittest.TestCase):
    def make(self, tmp, *, run_inner, resolver_run, resolve_max=2):
        write(tmp, "ralph/STATE.md", "- [ ] dm-a — depends []\n")
        paths = ralph.Paths(pathlib.Path(tmp))
        return ralph.Supervisor(paths, run_inner=run_inner, resolver_run=resolver_run,
                                notify_enabled=False, resolve_max=resolve_max)

    def test_done_returns_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/DONE", "")
            s = self.make(tmp, run_inner=lambda: None, resolver_run=lambda *a: None)
            self.assertEqual(s.run(), 0)

    def test_operator_stop_is_silent_and_terminal(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STOP", "")
            calls = []
            s = self.make(tmp, run_inner=lambda: calls.append("inner"),
                          resolver_run=lambda *a: calls.append("resolver"))
            self.assertEqual(s.run(), 0)
            self.assertEqual(calls, [])

    def test_human_row_exits_two_without_resolver(self):
        with tempfile.TemporaryDirectory() as tmp:
            calls = []
            s = self.make(tmp, run_inner=lambda: calls.append("inner"),
                          resolver_run=lambda *a: calls.append("resolver"))
            write(tmp, "ralph/STATE.md", "- [ ] HUMAN-design-review — depends []\n")
            self.assertEqual(s.run(), 2)
            self.assertEqual(calls, [])

    def test_noop_resolution_escalates_immediately(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/NEEDS_HUMAN.md", "# blocker\n")
            attempts = []
            s = self.make(tmp, run_inner=lambda: None,
                          resolver_run=lambda attempt, reason: attempts.append(attempt))
            with mock.patch.object(ralph, "head_of", return_value="a" * 40):
                self.assertEqual(s.run(), 2)
            self.assertEqual(attempts, [1])

    def test_junk_commits_do_not_reset_the_bound(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/NEEDS_HUMAN.md", "# blocker\n")
            heads = {"h": "a" * 40}
            attempts = []

            def resolver(attempt, reason):
                attempts.append(attempt)
                heads["h"] = f"{attempt:040x}"

            s = self.make(tmp, run_inner=lambda: None, resolver_run=resolver,
                          resolve_max=2)
            with mock.patch.object(ralph, "head_of", side_effect=lambda wd: heads["h"]):
                self.assertEqual(s.run(), 2)
            self.assertEqual(attempts, [1, 2])

    def test_halt_stop_without_package_dispatches(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STOP", "halt: boom\n")
            attempts = []

            def resolver(attempt, reason):
                attempts.append(attempt)
                (pathlib.Path(tmp) / "ralph/NEEDS_HUMAN.md").unlink(missing_ok=True)
                write(tmp, "ralph/DONE", "")

            s = self.make(tmp, run_inner=lambda: None, resolver_run=resolver)
            with mock.patch.object(ralph, "head_of", return_value="a" * 40):
                self.assertEqual(s.run(), 0)
            self.assertEqual(attempts, [1])


class WatchTests(unittest.TestCase):
    def make(self, tmp, *, running=True, free=100_000, stall_secs=300):
        (pathlib.Path(tmp) / "ralph").mkdir(exist_ok=True)
        notes = []
        paths = ralph.Paths(pathlib.Path(tmp))
        w = ralph.Watch(paths, label="t", running=lambda: running,
                        disk_free_mb=lambda: free, stall_secs=stall_secs,
                        notifier=lambda title, body, enabled: notes.append((title, body)))
        return w, notes

    def test_needs_human_done_and_operator_stop(self):
        with tempfile.TemporaryDirectory() as tmp:
            w, _ = self.make(tmp)
            pkg = write(tmp, "ralph/NEEDS_HUMAN.md", "# x\n")
            self.assertTrue(w.condition()[0].startswith("needs-human"))
            pkg.unlink()
            done = write(tmp, "ralph/DONE", "")
            self.assertIsNone(w.condition())
            done.unlink()
            write(tmp, "ralph/STOP", "")
            self.assertIsNone(w.condition())

    def test_down_stalled_disk(self):
        with tempfile.TemporaryDirectory() as tmp:
            w, _ = self.make(tmp, running=False)
            self.assertEqual(w.condition()[0], "down")
        with tempfile.TemporaryDirectory() as tmp:
            w, _ = self.make(tmp)
            hb = write(tmp, "ralph/.heartbeat", "1 x\n")
            old = time.time() - 600
            os.utime(hb, (old, old))
            self.assertEqual(w.condition()[0], "stalled")
        with tempfile.TemporaryDirectory() as tmp:
            w, _ = self.make(tmp, free=100)
            self.assertEqual(w.condition()[0], "disk-low")

    def test_dedup_and_renag_on_new_content(self):
        with tempfile.TemporaryDirectory() as tmp:
            w, notes = self.make(tmp)
            state = pathlib.Path(tmp) / "watch.state"
            write(tmp, "ralph/NEEDS_HUMAN.md", "# x\n")
            w.run(state)
            w.run(state)
            self.assertEqual(len(notes), 1)
            write(tmp, "ralph/NEEDS_HUMAN.md", "# y\n")
            w.run(state)
            self.assertEqual(len(notes), 2)
            (pathlib.Path(tmp) / "ralph/NEEDS_HUMAN.md").unlink()
            w.run(state)
            self.assertEqual(state.read_text(), "")


class FakeLane:
    """A lane session: writes its unit's file, commits, and marks done."""

    def __init__(self, cwd, body=None, shared=False):
        self.cwd = pathlib.Path(cwd)
        self.body = body if body is not None else self.cwd.name
        self.shared = shared

    def run(self, model_args, prompt, log):
        unit = self.cwd.name
        name = "shared.txt" if self.shared else f"{unit}.txt"
        (self.cwd / name).write_text(self.body)
        (self.cwd / "ralph" / "lanes").mkdir(parents=True, exist_ok=True)
        (self.cwd / "ralph" / "lanes" / f"{unit}.done").write_text("")
        subprocess.run(["git", "-C", str(self.cwd), "add", "-A"], check=True)
        subprocess.run(["git", "-C", str(self.cwd), "commit", "-q", "-m", unit], check=True)
        return 0


class FakeReview:
    """A main-tree review session: marks its row [x] and commits."""

    def __init__(self, root, unit="REVIEW-a"):
        self.root = pathlib.Path(root)
        self.unit = unit

    def run(self, model_args, prompt, log):
        q = ralph.Queue(self.root / "ralph/STATE.md")
        q.set_status(self.unit, ralph.Status.DONE)
        subprocess.run(["git", "-C", str(self.root), "add", "-A"], check=True)
        subprocess.run(["git", "-C", str(self.root), "commit", "-q", "-m", "review"], check=True)
        return 0


class PoolTests(unittest.TestCase):
    def fixture(self, tmp, rows):
        subprocess.run(["git", "init", "-q", "-b", "main", tmp], check=True)
        subprocess.run(["git", "-C", tmp, "config", "user.email", "t@t"], check=True)
        subprocess.run(["git", "-C", tmp, "config", "user.name", "t"], check=True)
        write(tmp, "ralph/STATE.md", rows)
        write(tmp, "ralph/PROMPT.md", "Execute the selected unit.")
        write(tmp, "seed.txt", "seed")
        subprocess.run(["git", "-C", tmp, "add", "-A"], check=True)
        subprocess.run(["git", "-C", tmp, "commit", "-q", "-m", "seed"], check=True)
        return pathlib.Path(tmp)

    def make(self, root, session_for, **kwargs):
        paths = ralph.Paths(root)
        return ralph.Pool(paths, session_for=session_for, notify_enabled=False,
                          lanes=2, base_branch="main", sleep=lambda s: None, **kwargs)

    def test_pick_wave_excludes_reviews_deps_and_conflicts(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md",
                  "- [ ] dm-a — depends []\n"
                  "- [ ] REVIEW-r — depends []\n"
                  "- [ ] dm-b — depends [dm-a]\n"
                  "- [~] dm-c — depends []\n"
                  "- [ ] dm-d — depends []\n")
            q = ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md")
            self.assertEqual(q.pick_wave(2, set()), ["dm-a", "dm-c"])
            self.assertEqual(q.pick_wave(4, {frozenset(("dm-a", "dm-c"))}),
                             ["dm-a", "dm-d"])

    def test_wave_merges_marks_and_writes_done(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-a — depends []\n- [ ] dm-b — depends []\n")
            pool = self.make(root, lambda cwd: FakeLane(cwd))
            self.assertEqual(pool.run(), 0)
            self.assertTrue((root / "ralph/DONE").exists())
            self.assertTrue((root / "dm-a.txt").exists())
            self.assertTrue((root / "dm-b.txt").exists())
            q = ralph.Queue(root / "ralph/STATE.md")
            self.assertTrue(q.all_done())

    def test_merge_conflict_halts(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-a — depends []\n- [ ] dm-b — depends []\n")
            pool = self.make(root, lambda cwd: FakeLane(cwd, body=cwd.name, shared=True))
            self.assertEqual(pool.run(), 3)
            pkg = (root / "ralph/NEEDS_HUMAN.md").read_text()
            self.assertIn("merge conflict", pkg)
            self.assertIn("halt: merge conflict", (root / "ralph/STOP").read_text())

    def test_review_runs_serially_and_completes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] REVIEW-a — depends []\n")
            pool = self.make(root, lambda cwd: FakeReview(cwd))
            self.assertEqual(pool.run(), 0)
            self.assertTrue((root / "ralph/DONE").exists())

    def test_operator_stop_is_terminal(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-a — depends []\n")
            write(tmp, "ralph/STOP", "")
            pool = self.make(root, lambda cwd: FakeLane(cwd))
            self.assertEqual(pool.run(), 0)


if __name__ == "__main__":
    unittest.main()
