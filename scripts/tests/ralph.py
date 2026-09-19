#!/usr/bin/env python3
"""ralph.py — the state machine's tests, in-process (no model calls).

The FSM is exercised through its seams: session runners, clocks and
notifiers are injected, so every gate runs in milliseconds and its failing
input is explicit.
"""
import contextlib
import io
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


MANIFEST = """\
label = "alpha"
session_timeout = 7200
worker_bin = "scripts/ralph-claude-shim.sh"
settings = "ralph/claude-settings.json"

[models]
worker = "w/x"
review = "r/y"
resolve = "d/z"
variant = "high"

[checks]
hello = ["sh", "-c", "echo from-a"]
"""


class QueueManifestTests(unittest.TestCase):
    def test_loads_every_key_and_defaults_the_paths_from_the_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/next/a/queue.toml", MANIFEST)
            m = ralph.load_manifest(tmp, "a")
            self.assertEqual((m.name, m.label), ("a", "alpha"))
            self.assertEqual(m.state, "ralph/next/a/STATE.md")
            self.assertEqual(m.prompt, "ralph/next/a/PROMPT.md")
            self.assertEqual(m.charter, "ralph/next/a/CHARTER.md")
            self.assertEqual(m.conflicts, "ralph/next/a/conflicts.txt")
            self.assertEqual(m.heavy, "ralph/next/a/heavy.txt")
            self.assertEqual(m.control_dir, "ralph/next/a/ctl")
            self.assertEqual(m.models, {"MODEL": "w/x", "REVIEW_MODEL": "r/y",
                                        "RESOLVE_MODEL": "d/z", "VARIANT": "high"})
            self.assertEqual(m.checks, {"hello": ("sh", "-c", "echo from-a")})
            self.assertEqual(m.session_timeout, 7200)
            self.assertEqual(m.worker_bin, "scripts/ralph-claude-shim.sh")
            self.assertEqual(m.settings, "ralph/claude-settings.json")

    def test_an_empty_manifest_is_all_defaults(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/next/b/queue.toml", "")
            m = ralph.load_manifest(tmp, "b")
            self.assertEqual(m.label, "b")
            self.assertEqual((m.models, m.checks, m.session_timeout, m.worker_bin),
                             ({}, {}, None, ""))

    def test_a_missing_manifest_is_refused_by_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(ValueError, "ralph/next/ghost/queue.toml"):
                ralph.load_manifest(tmp, "ghost")

    def test_schema_errors_name_the_key(self):
        bad = {"lable = 'x'\n": "lable",
               "[models]\nworkr = 'x'\n": "workr",
               "[checks]\nhello = 'echo hi'\n": "hello",
               "[checks]\nlint = ['true']\n": "lint",
               "session_timeout = 'long'\n": "session_timeout"}
        for text, key in bad.items():
            with tempfile.TemporaryDirectory() as tmp:
                write(tmp, "ralph/next/a/queue.toml", text)
                with self.assertRaisesRegex(ValueError, key):
                    ralph.load_manifest(tmp, "a")

    def test_a_name_is_one_path_segment(self):
        with tempfile.TemporaryDirectory() as tmp:
            for name in ("../x", "a/b", ""):
                with self.assertRaises(ValueError):
                    ralph.load_manifest(tmp, name)


class ModelTests(unittest.TestCase):
    def test_load_and_review_routing(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = write(tmp, "ralph/models.env",
                      "# comment\nMODEL=w/x\nREVIEW_MODEL=r/y\nRESOLVE_MODEL=d/z\nVARIANT=high\nOTHER=z\n")
            m = ralph.load_models(p)
            self.assertEqual(m, {"MODEL": "w/x", "REVIEW_MODEL": "r/y",
                                 "RESOLVE_MODEL": "d/z", "VARIANT": "high"})
            self.assertEqual(ralph.select_model_args("dm-x", "w", "r", "high"),
                             ["--model", "w", "--variant", "high"])
            self.assertEqual(ralph.select_model_args("REVIEW-build-x", "w", "r", "high"),
                             ["--model", "r", "--variant", "high"])


class ModelPrecedenceTests(unittest.TestCase):
    def resolved(self, tmp, argv):
        args = ralph.build_parser().parse_args(argv)
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            paths = ralph.paths_for(args)
            models = ralph.resolve_models(args, paths)
        return args, models, buf.getvalue()

    def test_manifest_then_flag_then_models_env(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/next/a/queue.toml",
                  'session_timeout = 7200\n[models]\nworker = "m/worker"\nvariant = "high"\n')
            write(tmp, "ralph/models.env", "MODEL=e/worker\nREVIEW_MODEL=e/review\n"
                                           "RESOLVE_MODEL=e/resolve\nVARIANT=low\n")
            args, models, said = self.resolved(
                tmp, ["supervise", "--workdir", tmp, "--queue", "a", "--model", "f/worker",
                      "--review-model", "f/review", "--session-timeout", "60"])
            self.assertEqual(models, {"MODEL": "m/worker", "REVIEW_MODEL": "f/review",
                                      "RESOLVE_MODEL": "e/resolve", "VARIANT": "high"})
            self.assertEqual(args.session_timeout, 7200)
            self.assertIn("ignoring --model f/worker", said)
            self.assertIn("ignoring --session-timeout 60", said)

    def test_a_legacy_line_is_flag_then_models_env(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/models.env", "MODEL=e/worker\nREVIEW_MODEL=e/review\n")
            args, models, said = self.resolved(
                tmp, ["run", "--workdir", tmp, "--model", "f/worker", "--session-timeout", "7200"])
            self.assertEqual(models, {"MODEL": "f/worker", "REVIEW_MODEL": "e/review",
                                      "RESOLVE_MODEL": "", "VARIANT": ""})
            self.assertEqual((args.session_timeout, said), (7200, ""))

    def test_models_with_a_queue_rewrites_the_manifest_and_not_the_shared_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            manifest = write(tmp, "ralph/next/a/queue.toml",
                             '# the a queue\nlabel = "alpha"\n\n[models]\n'
                             'worker = "old/worker"  # was cheap\nvariant = "high"\n\n'
                             '[checks]\nhello = ["echo", "hi"]\n')
            write(tmp, "ralph/next/b/queue.toml", 'label = "beta"\n')
            rc, out, _ = quiet_main(["models", "--workdir", tmp, "--queue", "a",
                                     "--model", "new/worker", "--review-model", 'r/"q"'])
            self.assertEqual(rc, 0)
            self.assertIn("ralph/next/a/queue.toml", out)
            self.assertFalse((pathlib.Path(tmp) / "ralph/models.env").exists())
            text = manifest.read_text()
            self.assertIn("# the a queue\n", text)
            self.assertIn('[checks]\nhello = ["echo", "hi"]\n', text)
            self.assertNotIn("old/worker", text)
            m = ralph.load_manifest(tmp, "a")
            self.assertEqual(m.models, {"MODEL": "new/worker", "REVIEW_MODEL": 'r/"q"',
                                        "VARIANT": "high"})
            self.assertEqual((m.label, m.checks), ("alpha", {"hello": ("echo", "hi")}))
            quiet_main(["models", "--workdir", tmp, "--queue", "b", "--resolve-model", "d/z"])
            self.assertEqual(ralph.load_manifest(tmp, "b").models, {"RESOLVE_MODEL": "d/z"})
            self.assertEqual(ralph.load_manifest(tmp, "b").label, "beta")


class ResolverPromptTests(unittest.TestCase):
    def test_without_charter_the_resolver_defers(self):
        paths = ralph.Paths(pathlib.Path("/tmp/x"))
        prompt = ralph.resolver_prompt(paths, 1, 4, "blocked")
        self.assertIn("leave", prompt)
        self.assertNotIn("DIRECTOR", prompt)

    def test_with_charter_the_resolver_decides(self):
        paths = ralph.Paths(pathlib.Path("/tmp/x"))
        prompt = ralph.resolver_prompt(paths, 1, 4, "blocked", charter="# charter body")
        self.assertIn("DIRECTOR", prompt)
        self.assertIn("# charter body", prompt)
        self.assertIn("DECISIONS.md", prompt)


class ReportTests(unittest.TestCase):
    def test_report_prints_queue_decisions_and_ranges(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md",
                  "- [x] dm-a abc1234 — depends [] — done\n- [ ] dm-b — depends []\n")
            write(tmp, "ralph/DECISIONS.md",
                  "- 2026-09-16 dm-b: chose X. REVIEW-AFTER: no\n")
            write(tmp, "ralph/.director-commits",
                  "1789500000 attempt=1 deadbee..cafe123 — blocker\n")
            subprocess.run(["git", "init", "-q", tmp], check=True)
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                ralph.main(["report", "--workdir", tmp])
            out = buf.getvalue()
            self.assertIn("done 1", out)
            self.assertIn("REVIEW-AFTER", out)
            self.assertIn("deadbee..cafe123", out)


class PromoteTests(unittest.TestCase):
    STAGED = "- [ ] h-a — depends [] — do a\n- [ ] h-b — depends [h-a] — do b\n"

    def active_and_staged(self, tmp, markers=("DONE",), beat_age=None):
        write(tmp, "ralph/STATE.md", "- [x] dm-a abc1234 — depends [] — done\n")
        write(tmp, "ralph/PROMPT.md", "old prompt\n")
        write(tmp, "ralph/DECISIONS.md", "- old decision\n")
        write(tmp, "ralph/lanes/dm-a.done", "")
        write(tmp, "ralph/models.env", "MODEL=x\n")
        for m in markers:
            write(tmp, f"ralph/{m}", "")
        if beat_age is not None:
            beat = write(tmp, "ralph/.heartbeat", "1 session\n")
            then = time.time() - beat_age
            os.utime(beat, (then, then))
        write(tmp, "ralph/next/handed/STATE.md", self.STAGED)
        write(tmp, "ralph/next/handed/PROMPT.md", "new prompt\n")

    def promote(self, tmp, *extra):
        err = io.StringIO()
        with mock.patch.object(ralph, "job_running", return_value=False), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            rc = ralph.main(["promote", "handed", "--workdir", tmp, *extra])
        return rc, err.getvalue()

    def test_dry_run_parses_the_staged_queue_and_moves_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.active_and_staged(tmp)
            rc, _ = self.promote(tmp, "--dry-run")
            self.assertEqual(rc, 0)
            self.assertEqual((pathlib.Path(tmp) / "ralph/PROMPT.md").read_text(), "old prompt\n")

    def test_refuses_while_a_loop_is_live(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.active_and_staged(tmp, markers=("STOP",), beat_age=5)
            rc, err = self.promote(tmp)
            self.assertEqual(rc, 2)
            self.assertIn("still live", err)
            self.assertTrue((pathlib.Path(tmp) / "ralph/next/handed/STATE.md").exists())

    def test_refuses_a_campaign_that_neither_finished_nor_was_stopped(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.active_and_staged(tmp, markers=())
            rc, err = self.promote(tmp)
            self.assertEqual(rc, 2)
            self.assertIn("neither DONE nor an operator STOP", err)

    def test_promotes_archives_the_old_campaign_and_keeps_host_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.active_and_staged(tmp, markers=("DONE",), beat_age=STALE)
            rc, _ = self.promote(tmp)
            self.assertEqual(rc, 0)
            root = pathlib.Path(tmp)
            self.assertEqual((root / "ralph/STATE.md").read_text(), self.STAGED)
            self.assertFalse((root / "ralph/DONE").exists())
            self.assertFalse((root / "ralph/next/handed").exists())
            self.assertFalse((root / "ralph/lanes").exists())
            self.assertEqual((root / "ralph/models.env").read_text(), "MODEL=x\n")
            (archive,) = (root / "ralph/archive").iterdir()
            self.assertEqual((archive / "DECISIONS.md").read_text(), "- old decision\n")
            self.assertTrue((archive / "lanes/dm-a.done").exists())

    def test_report_names_a_staged_queue_that_does_not_parse(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.active_and_staged(tmp)
            write(tmp, "ralph/next/handed/STATE.md",
                  "- [ ] h-a — depends [nope] — do a\n")
            subprocess.run(["git", "init", "-q", tmp], check=True)
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                ralph.main(["report", "--workdir", tmp])
            self.assertIn("DOES NOT PARSE", buf.getvalue())

    def test_report_names_the_next_campaign(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.active_and_staged(tmp)
            subprocess.run(["git", "init", "-q", tmp], check=True)
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                ralph.main(["report", "--workdir", tmp])
            self.assertIn("next up: handed (2 rows", buf.getvalue())


STALE = ralph.STALL_SECS + 60


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

    def test_the_unit_note_names_the_queue_it_was_launched_on(self):
        with tempfile.TemporaryDirectory() as tmp:
            seen = []
            c = self.make(tmp, "", session_run=lambda args, prompt, log: seen.append(prompt),
                          max_stall=1)
            write(tmp, "ralph/next/x/STATE.md", "- [ ] qx-1 — depends []\n")
            c.paths.state = "ralph/next/x/STATE.md"
            with mock.patch.object(ralph, "head_of", return_value="a" * 40):
                c.run()
            self.assertIn("its row in ralph/next/x/STATE.md is", seen[0])

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


class TwoQueueControlTests(unittest.TestCase):
    """Two loops in one checkout share no control file."""

    def campaign(self, tmp, name, seen):
        paths = ralph.paths_for(ralph.build_parser().parse_args(
            ["run", "--workdir", tmp, "--queue", name]))
        return ralph.Campaign(paths, notify_enabled=False, sleep=lambda s: None, max_stall=1,
                              session_run=lambda args, prompt, log: seen.append((name, prompt, log)))

    def test_stop_in_a_stops_a_and_not_b_and_a_stale_done_ends_neither(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            write(tmp, "ralph/DONE", "")            # the finished campaign's leftover
            write(tmp, "ralph/next/a/ctl/STOP", "")
            seen = []
            with mock.patch.object(ralph, "head_of", return_value="a" * 40):
                ra = self.campaign(tmp, "a", seen).run()
                rb = self.campaign(tmp, "b", seen).run()
            self.assertEqual(ra.outcome, ralph.Outcome.OPERATOR_STOP)
            self.assertEqual(rb.outcome, ralph.Outcome.HALT)       # b ran its session and stalled
            self.assertEqual([name for name, _, _ in seen], ["b"])
            _, prompt, log = seen[0]
            self.assertIn("ralph/next/b/ctl/NEEDS_HUMAN.md", prompt)
            self.assertIn("target/ralph/b/", log)
            root = pathlib.Path(tmp)
            self.assertIn("without a commit", (root / "ralph/next/b/ctl/NEEDS_HUMAN.md").read_text())
            self.assertTrue((root / "ralph/next/b/ctl/.heartbeat").exists())
            self.assertFalse((root / "ralph/NEEDS_HUMAN.md").exists())
            self.assertFalse((root / "ralph/STOP").exists())

    def test_watch_stop_and_report_read_the_queues_markers(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            write(tmp, "ralph/next/a/ctl/NEEDS_HUMAN.md", "# a is blocked\n")
            pa, pb = (ralph.paths_for(ralph.build_parser().parse_args(
                ["watch", "--workdir", tmp, "--queue", q])) for q in ("a", "b"))
            wa = ralph.Watch(pa, label="a", running=lambda: True, disk_free_mb=lambda: 99999)
            wb = ralph.Watch(pb, label="b", running=lambda: True, disk_free_mb=lambda: 99999)
            self.assertTrue(wa.condition()[0].startswith("needs-human:"))
            self.assertIsNone(wb.condition())
            with mock.patch.object(ralph, "job_running", return_value=False):
                rc, _, _ = quiet_main(["stop", "--workdir", tmp, "--queue", "b", "--timeout", "1"])
            self.assertEqual(rc, 0)
            self.assertTrue((pathlib.Path(tmp) / "ralph/next/b/ctl/STOP").exists())
            self.assertFalse((pathlib.Path(tmp) / "ralph/STOP").exists())
            _, out, _ = quiet_main(["report", "--workdir", tmp, "--queue", "a"])
            self.assertIn("markers: NEEDS_HUMAN.md", out)


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

    def test_director_commit_range_is_recorded(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/NEEDS_HUMAN.md", "# blocker\n")
            heads = {"h": "a" * 40}

            def resolver(attempt, reason):
                heads["h"] = f"{attempt:040x}"

            s = self.make(tmp, run_inner=lambda: None, resolver_run=resolver, resolve_max=1)
            with mock.patch.object(ralph, "head_of", side_effect=lambda wd: heads["h"]):
                s.run()
            log = (pathlib.Path(tmp) / "ralph/.director-commits").read_text()
            self.assertIn("a" * 40 + ".." + f"{1:040x}", log)

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
            pool = self.make(root, lambda cwd, env=None: FakeLane(cwd))
            self.assertEqual(pool.run(), 0)
            self.assertTrue((root / "ralph/DONE").exists())
            self.assertTrue((root / "dm-a.txt").exists())
            self.assertTrue((root / "dm-b.txt").exists())
            q = ralph.Queue(root / "ralph/STATE.md")
            self.assertTrue(q.all_done())

    def test_resumed_lane_is_refreshed_onto_the_base(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-a — depends []\n")
            seed = subprocess.run(["git", "-C", tmp, "rev-parse", "HEAD"],
                                  capture_output=True, text=True).stdout.strip()
            # A harness fix lands on the base AFTER the lane worktree exists.
            write(tmp, "harness.txt", "fixed")
            subprocess.run(["git", "-C", tmp, "add", "-A"], check=True)
            subprocess.run(["git", "-C", tmp, "commit", "-q", "-m", "fix"], check=True)
            wt = root / ".ralph" / "wt" / "dm-a"
            subprocess.run(["git", "-C", tmp, "worktree", "add", "-q", "-b",
                            "ralph/dm-a", str(wt), seed], check=True)

            class HarnessLane(FakeLane):
                def run(self, model_args, prompt, log):
                    self.body = (self.cwd / "harness.txt").read_text()
                    return super().run(model_args, prompt, log)

            pool = self.make(root, lambda cwd, env=None: HarnessLane(cwd))
            self.assertEqual(pool.run(), 0)
            self.assertEqual((root / "dm-a.txt").read_text(), "fixed")

    def test_lane_worktree_gets_host_pointer_dirs(self):
        # A row's `read: O8` names `.sovereign/features/<id>/order.md`, which
        # is gitignored (`.gitignore:44`), so `git worktree add` never brings
        # it and the lane cannot execute its row (dm-vocab-compile-fail-test,
        # 2026-09-17, three waves). The pool provisions the per-host pointers.
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-a — depends []\n")
            write(tmp, ".gitignore", ".sovereign/features/\n")
            write(tmp, ".sovereign/features/dm-a/order.md", "the order")
            subprocess.run(["git", "-C", tmp, "add", "-A"], check=True)
            subprocess.run(["git", "-C", tmp, "commit", "-q", "-m", "pointers"], check=True)

            class PointerLane(FakeLane):
                def run(self, model_args, prompt, log):
                    self.body = (self.cwd / ".sovereign/features/dm-a/order.md").read_text()
                    return super().run(model_args, prompt, log)

            pool = self.make(root, lambda cwd, env=None: PointerLane(cwd))
            self.assertEqual(pool.run(), 0)
            self.assertEqual((root / "dm-a.txt").read_text(), "the order")

    def test_merge_conflict_halts(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-a — depends []\n- [ ] dm-b — depends []\n")
            pool = self.make(root, lambda cwd, env=None: FakeLane(cwd, body=cwd.name, shared=True))
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
            pool = self.make(root, lambda cwd, env=None: FakeLane(cwd))
            self.assertEqual(pool.run(), 0)


class GuardTests(unittest.TestCase):
    def test_guarded_turns_an_io_error_into_a_package(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md", "- [ ] dm-a — depends []\n")
            paths = ralph.Paths(pathlib.Path(tmp))

            def boom():
                raise OSError(28, "No space left on device")

            rc = ralph.guarded(boom, paths, notify_enabled=False)
            self.assertEqual(rc, 3)
            self.assertIn("No space left",
                          (pathlib.Path(tmp) / "ralph/NEEDS_HUMAN.md").read_text())


REPO = pathlib.Path(os.environ.get("RALPH_TEST_REPO")
                    or pathlib.Path(__file__).resolve().parents[2])


def launch_lines(state_rel):
    """The `ralph.py <verb> ...` argvs inside a staged queue's fenced launch block."""
    import shlex
    text = (REPO / state_rel).read_text()
    block = next(b for b in text.split("```")[1::2] if "scripts/ralph.py" in b)
    tokens = shlex.split(block.replace("\\\n", " "), comments=True)
    starts = [i for i, t in enumerate(tokens) if t == "scripts/ralph.py"]
    out = []
    for n, i in enumerate(starts):
        end = starts[n + 1] if n + 1 < len(starts) else len(tokens)
        argv = tokens[i + 1:end]
        for stopper in ("--", ">>"):
            if stopper in argv:
                argv = argv[:argv.index(stopper)]
        out.append(argv)
    return out


def quiet_main(argv):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        rc = ralph.main(argv)
    return rc, out.getvalue(), err.getvalue()


def two_queues(tmp):
    subprocess.run(["git", "init", "-q", tmp], check=True)
    for name, row in (("a", "qa-1"), ("b", "qb-1")):
        write(tmp, f"ralph/next/{name}/queue.toml",
              f'[checks]\nhello = ["sh", "-c", "echo from-{name}"]\n')
        write(tmp, f"ralph/next/{name}/STATE.md", f"- [ ] {row} — depends [] — do it\n")
        write(tmp, f"ralph/next/{name}/PROMPT.md", f"prompt of {name}\n")
    write(tmp, "ralph/STATE.md", "- [ ] legacy-1 — depends [] — the default queue\n")


class PathsForTests(unittest.TestCase):
    def test_a_queue_by_flag_is_never_swapped_for_the_default(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            for verb in ("run", "supervise", "watch", "stop", "start", "models", "plan",
                         "report"):
                args = ralph.build_parser().parse_args([verb, "--workdir", tmp, "--queue", "b"])
                p = ralph.paths_for(args)
                self.assertEqual((verb, p.state, p.queue),
                                 (verb, "ralph/next/b/STATE.md", "b"))
                self.assertEqual(p.charter, "ralph/next/b/CHARTER.md")
                if verb != "models":
                    self.assertEqual(args.label, "b")

    def test_a_queues_control_files_live_under_its_control_dir(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            write(tmp, "ralph/next/b/queue.toml", 'control_dir = "var/b"\n')
            pa, pb = (ralph.paths_for(ralph.build_parser().parse_args(
                ["run", "--workdir", tmp, "--queue", q])) for q in ("a", "b"))
            ctl = "ralph/next/a/ctl"
            self.assertEqual(
                (pa.control_dir, pa.done, pa.stop, pa.needs_human, pa.waiting, pa.heartbeat,
                 pa.director_commits, pa.log_dir),
                (ctl, f"{ctl}/DONE", f"{ctl}/STOP", f"{ctl}/NEEDS_HUMAN.md", f"{ctl}/waiting",
                 f"{ctl}/.heartbeat", f"{ctl}/.director-commits", "target/ralph/a"))
            self.assertEqual((pb.stop, pb.log_dir), ("var/b/STOP", "target/ralph/b"))
            self.assertEqual(ralph.runtime_markers(pa), (f"{ctl}/",))

    def test_plan_prints_the_named_queues_head(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            rc, out, _ = quiet_main(["plan", "--workdir", tmp, "--queue", "b"])
            self.assertEqual(rc, 0)
            self.assertIn("unit qb-1", out)
            self.assertIn("queue: ralph/next/b/STATE.md", out)
            self.assertNotIn("legacy-1", out)

    def test_queue_wins_over_a_legacy_flag_and_says_so(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            args = ralph.build_parser().parse_args(
                ["run", "--workdir", tmp, "--queue", "a", "--state", "ralph/STATE.md"])
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                p = ralph.paths_for(args)
            self.assertEqual(p.state, "ralph/next/a/STATE.md")
            self.assertIn("ignoring --state ralph/STATE.md", buf.getvalue())

    def test_a_missing_manifest_is_exit_two_not_the_default_queue(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            rc, out, err = quiet_main(["plan", "--workdir", tmp, "--queue", "ghost"])
            self.assertEqual(rc, 2)
            self.assertIn("ralph/next/ghost/queue.toml", err)
            self.assertNotIn("legacy-1", out)

    def test_no_subcommand_builds_paths_beside_paths_for(self):
        src = pathlib.Path(ralph.__file__).read_text()
        body = src.replace(src[src.index("def paths_for("):src.index("def queue_flags(")], "")
        self.assertNotRegex(body, r"[^`\w]Paths\(")

    def test_the_staged_launch_lines_resolve_as_they_always_did(self):
        # The order's kill condition: these lines are not edited, so they must parse
        # and land on their own queue with the per-checkout control files.
        for name in ("ring-doc", "ring-room", "ring-room-rr2", "ei7-stage0"):
            lines = launch_lines(f"ralph/next/{name}/STATE.md")
            self.assertEqual([argv[0] for argv in lines], ["supervise", "run"])
            for argv in lines:
                args = ralph.build_parser().parse_args(argv)
                p = ralph.paths_for(args)
                self.assertEqual((p.queue, p.manifest), ("", None))
                self.assertEqual(p.state, f"ralph/next/{name}/STATE.md")
                self.assertEqual(p.prompt, f"ralph/next/{name}/PROMPT.md")
                self.assertEqual(args.label, name)
                self.assertEqual((p.done, p.stop, p.needs_human, p.heartbeat, p.models),
                                 ("ralph/DONE", "ralph/STOP", "ralph/NEEDS_HUMAN.md",
                                  "ralph/.heartbeat", "ralph/models.env"))
            sup = ralph.paths_for(ralph.build_parser().parse_args(lines[0]))
            self.assertEqual(sup.charter, f"ralph/next/{name}/CHARTER.md")

    def test_the_default_launch_line_is_the_legacy_queue(self):
        args = ralph.build_parser().parse_args(["run"])
        p = ralph.paths_for(args)
        self.assertEqual((p.state, p.prompt, p.charter, args.label, args.session_timeout),
                         ("ralph/STATE.md", "ralph/PROMPT.md", "ralph/CHARTER.md",
                          "campaign", 3600))

    def test_run_flags_reach_paths(self):
        import argparse, tempfile
        with tempfile.TemporaryDirectory() as d:
            a = argparse.Namespace(workdir=d, prompt="ralph/next/x/PROMPT.md", state="ralph/next/x/STATE.md")
            p = ralph.paths_for(a)
            self.assertEqual(p.state, "ralph/next/x/STATE.md")
            self.assertEqual(p.prompt, "ralph/next/x/PROMPT.md")

    def test_absent_flags_keep_the_defaults(self):
        import argparse, tempfile
        with tempfile.TemporaryDirectory() as d:
            p = ralph.paths_for(argparse.Namespace(workdir=d))
            self.assertEqual(p.state, "ralph/STATE.md")
            self.assertEqual(p.prompt, "ralph/PROMPT.md")


if __name__ == "__main__":
    unittest.main()
