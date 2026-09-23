#!/usr/bin/env python3
"""ralph.py — the state machine's tests, in-process (no model calls).

The FSM is exercised through its seams: session runners, clocks and
notifiers are injected, so every gate runs in milliseconds and its failing
input is explicit.
"""
import contextlib
from dataclasses import replace as dataclasses_replace
import io
import os
import pathlib
import shutil
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


class AuditCountTests(unittest.TestCase):
    def queue(self, tmp, rows):
        return ralph.Queue(write(tmp, "ralph/STATE.md", rows))

    def test_counts_done_rows_after_the_last_done_audit_and_skips_human_rows(self):
        with tempfile.TemporaryDirectory() as tmp:
            q = self.queue(tmp, "- [x] u-1 abcdef1 — depends []\n"
                                "- [x] REVIEW-audit-u-1 abcdef2 — depends [u-1]\n"
                                "- [x] u-2 abcdef3 — depends []\n"
                                "- [x] HUMAN-u-look abcdef4 — depends []\n"
                                "- [x] REVIEW-build-u-3 abcdef5 — depends []\n"
                                "- [ ] REVIEW-audit-u-2 — depends []\n"     # not run: resets nothing
                                "- [~] u-4 — depends []\n")
            self.assertEqual(q.units_since_audit(), 2)

    def test_counts_from_the_top_when_no_audit_has_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            q = self.queue(tmp, "- [x] u-1 abcdef1 — depends []\n- [x] u-2 abcdef2 — depends []\n"
                                "- [ ] u-3 — depends []\n")
            self.assertEqual(q.units_since_audit(), 2)

    def test_the_generated_row_parses_and_lands_above_the_named_row(self):
        with tempfile.TemporaryDirectory() as tmp:
            q = self.queue(tmp, "# queue\n- [x] u-1 abcdef1 — depends []\n\n"
                                "- [ ] u-2 — depends [u-1] — do it\n")
            line = ralph.audit_row("REVIEW-audit-u-auto-1", "u-1")
            q.insert_before("u-2", line)
            self.assertEqual([r.id for r in q.rows], ["u-1", "REVIEW-audit-u-auto-1", "u-2"])
            row = ralph.Queue(q.path).by_id()["REVIEW-audit-u-auto-1"]      # from the file
            self.assertEqual((row.status, row.deps, row.hash, row.line),
                             (ralph.Status.PENDING, ("u-1",), None, line))
            self.assertTrue(ralph.is_review(row))
            self.assertIn(ralph.AUDIT_ROW_BODY, line)
            self.assertRegex(line, r" — read: `sovereign/ARCH_PRINCIPLES.md` — check: LINT$")
            self.assertEqual(q.path.read_text().splitlines()[0], "# queue")
            self.assertEqual(q.current().id, "REVIEW-audit-u-auto-1")


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
               "session_timeout = 'long'\n": "session_timeout",
               "audit_every = 1\n": "`audit_every` must be an integer >= 2",
               "audit_every = 'six'\n": "`audit_every` must be an integer >= 2",
               "audit_every = true\n": "`audit_every` must be an integer >= 2"}
        for text, key in bad.items():
            with tempfile.TemporaryDirectory() as tmp:
                write(tmp, "ralph/next/a/queue.toml", text)
                with self.assertRaisesRegex(ValueError, key):
                    ralph.load_manifest(tmp, "a")

    def test_audit_every_is_optional_and_loads_as_an_integer(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/next/a/queue.toml", "audit_every = 2\n")
            write(tmp, "ralph/next/b/queue.toml", "")
            self.assertEqual(ralph.load_manifest(tmp, "a").audit_every, 2)
            self.assertIsNone(ralph.load_manifest(tmp, "b").audit_every)

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

    def test_roster_values_parse_and_wait_limit_loads(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = write(tmp, "ralph/models.env",
                      "MODEL=a/1, b/2,,\nREVIEW_MODEL=r/1\nWAIT_LIMIT_S=43200\n")
            m = ralph.load_models(p)
            self.assertEqual(m, {"MODEL": "a/1, b/2,,", "REVIEW_MODEL": "r/1",
                                 "WAIT_LIMIT_S": "43200"})
            self.assertEqual(ralph.parse_roster(m["MODEL"]), ["a/1", "b/2"])
            self.assertEqual(ralph.parse_roster(""), [])
            self.assertEqual(ralph.parse_roster(None), [])

    def test_a_models_rewrite_preserves_wait_limit_s(self):
        # `models` rewrites models.env wholesale; dropping WAIT_LIMIT_S there
        # would silently revert the wait limit to the default (never silently
        # substitute).
        with tempfile.TemporaryDirectory() as tmp:
            p = write(tmp, "ralph/models.env", "MODEL=w/x\nWAIT_LIMIT_S=86400\n")
            rc, out, _ = quiet_main(["models", "--workdir", tmp, "--model", "n/y",
                                     "--no-restart"])
            self.assertEqual(rc, 0)
            text = p.read_text()
            self.assertIn("MODEL=n/y", text)
            self.assertIn("WAIT_LIMIT_S=86400", text)
            self.assertIn("WAIT_LIMIT_S=86400", out)


class WaitLimitTests(unittest.TestCase):
    """WAIT_LIMIT_S in models.env configures the detached-wait limit (default
    raised 7200 -> 86400): full gates legitimately exceed 2h on this box and
    every such wait was a false "never wrote its marker" halt."""

    def resolved(self, tmp, *extra):
        args = ralph.build_parser().parse_args(["run", "--workdir", tmp, *extra])
        return args, ralph.resolve_wait_limit(args, ralph.paths_for(args))

    def test_wait_limit_s_is_read_and_honored(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md", "- [ ] dm-a — depends []\n")
            write(tmp, "ralph/PROMPT.md", "Execute the selected unit.")
            write(tmp, "ralph/models.env", "WAIT_LIMIT_S=120\n")
            w = write(tmp, "ralph/waiting", "never.done\n")
            old = time.time() - 3600          # past 120s, well under the old 7200
            os.utime(w, (old, old))
            args, limit = self.resolved(tmp)
            self.assertEqual(limit, 120)
            paths = ralph.paths_for(args)
            c = ralph.Campaign(paths, session_run=lambda *a: 0, notify_enabled=False,
                               sleep=lambda s: None, marker_timeout=limit)
            result = c.run()
            self.assertEqual(result.outcome, ralph.Outcome.HALT)
            self.assertIn("never.done", result.reason)

    def test_without_wait_limit_s_the_default_is_a_day_not_two_hours(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md", "- [ ] dm-a — depends []\n")
            write(tmp, "ralph/PROMPT.md", "Execute the selected unit.")
            w = write(tmp, "ralph/waiting", "never.done\n")
            old = time.time() - 7800          # past the old 7200, under 86400
            os.utime(w, (old, old))

            def tick(secs):                   # the detached run lands on the first poll
                write(tmp, "never.done", "")

            def session(*a):
                write(tmp, "ralph/DONE", "")
                return 0
            args, limit = self.resolved(tmp)
            self.assertEqual(limit, 86400)
            c = ralph.Campaign(ralph.paths_for(args), session_run=session,
                               notify_enabled=False, sleep=tick, marker_timeout=limit)
            self.assertEqual(c.run().outcome, ralph.Outcome.DONE)

    def test_an_explicit_marker_timeout_flag_wins_over_models_env(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/models.env", "WAIT_LIMIT_S=120\n")
            _, limit = self.resolved(tmp, "--marker-timeout", "60")
            self.assertEqual(limit, 60)

    def test_a_bad_wait_limit_s_is_refused_by_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/models.env", "WAIT_LIMIT_S=soon\n")
            with self.assertRaisesRegex(ValueError, "WAIT_LIMIT_S='soon'"):
                self.resolved(tmp)
            write(tmp, "ralph/models.env", "WAIT_LIMIT_S=0\n")
            with self.assertRaisesRegex(ValueError, "WAIT_LIMIT_S=0"):
                self.resolved(tmp)


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


SCRIPTS = pathlib.Path(os.environ.get("RALPH_TEST_SCRIPTS")
                       or pathlib.Path(__file__).resolve().parents[1])


def git_repo(tmp):
    subprocess.run(["git", "init", "-q", "-b", "main", tmp], check=True)
    subprocess.run(["git", "-C", tmp, "config", "user.email", "t@t"], check=True)
    subprocess.run(["git", "-C", tmp, "config", "user.name", "t"], check=True)


def commit_all(tmp, msg="seed"):
    subprocess.run(["git", "-C", tmp, "add", "-A"], check=True)
    subprocess.run(["git", "-C", tmp, "commit", "-q", "-m", msg], check=True)
    return subprocess.run(["git", "-C", tmp, "rev-parse", "--short", "HEAD"],
                          capture_output=True, text=True).stdout.strip()


def install_script(tmp, name):
    dst = pathlib.Path(tmp) / "scripts" / name
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text((SCRIPTS / name).read_text())
    dst.chmod(0o755)
    return dst


def stub_worker(tmp, body):
    """A worker binary that spends no tokens: `<bin> run [flags] <prompt>` runs `body`."""
    stub = write(tmp, "stub-worker.sh", "#!/usr/bin/env bash\n" + body)
    stub.chmod(0o755)
    return stub


class SessionEnvTests(unittest.TestCase):
    def run_session(self, tmp, argv):
        args = ralph.build_parser().parse_args(argv)
        paths = ralph.paths_for(args)
        session = ralph.Session(paths, timeout=20, poll=0.2, notify_enabled=False)
        with contextlib.redirect_stdout(io.StringIO()):
            rc = session.run([], "the prompt", str(pathlib.Path(tmp) / "target/out.log"))
        return rc, (pathlib.Path(tmp) / "env.txt").read_text().split("|")

    DUMP = ('printf "%s|%s|%s|%s" "$RALPH_QUEUE" "$RALPH_STATE" "$RALPH_CONTROL_DIR" '
            '"${RALPH_CLAUDE_SETTINGS:-}" > env.txt\n')

    def test_a_queue_session_is_told_its_queue_state_and_control_dir(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = str(pathlib.Path(tmp).resolve())
            git_repo(tmp)
            stub_worker(tmp, self.DUMP)
            write(tmp, "ralph/next/a/queue.toml",
                  'worker_bin = "./stub-worker.sh"\nsettings = "ralph/next/a/settings.json"\n')
            with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": "/nonexistent/worker"}):
                rc, env = self.run_session(tmp, ["run", "--workdir", tmp, "--queue", "a"])
            self.assertEqual(rc, 0)
            self.assertEqual(env, ["a", "ralph/next/a/STATE.md", "ralph/next/a/ctl",
                                   f"{tmp}/ralph/next/a/settings.json"])

    def test_a_legacy_session_is_told_its_state_too(self):
        with tempfile.TemporaryDirectory() as tmp:
            git_repo(tmp)
            stub = stub_worker(tmp, self.DUMP)
            with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": str(stub),
                                              "RALPH_QUEUE": "leaked-from-a-parent-loop"}):
                rc, env = self.run_session(
                    tmp, ["run", "--workdir", tmp, "--state", "ralph/next/ring-room/STATE.md"])
            self.assertEqual(rc, 0)
            self.assertEqual(env, ["", "ralph/next/ring-room/STATE.md", "ralph", ""])


class RalphMarkTests(unittest.TestCase):
    ROWS = "- [~] qa-1 — depends [] — do it\n- [ ] qa-2 — depends [qa-1] — next\n"

    def fixture(self, tmp):
        git_repo(tmp)
        install_script(tmp, "ralph-mark.sh")
        write(tmp, "ralph/next/a/STATE.md", self.ROWS)
        write(tmp, "ralph/next/b/STATE.md", self.ROWS)
        return commit_all(tmp)

    def mark(self, tmp, *argv, state_env=None):
        env = {k: v for k, v in os.environ.items() if k != "RALPH_STATE"}
        if state_env is not None:
            env["RALPH_STATE"] = state_env
        return subprocess.run([str(pathlib.Path(tmp) / "scripts/ralph-mark.sh"), *argv],
                              capture_output=True, text=True, env=env)

    def test_the_explicit_third_argument_still_works_and_wins(self):
        with tempfile.TemporaryDirectory() as tmp:
            sha = self.fixture(tmp)
            r = self.mark(tmp, "qa-1", sha, "ralph/next/a/STATE.md",
                          state_env="ralph/next/b/STATE.md")
            self.assertEqual(r.returncode, 0, r.stderr)
            root = pathlib.Path(tmp)
            self.assertIn(f"- [x] qa-1 {sha} — depends", (root / "ralph/next/a/STATE.md").read_text())
            self.assertEqual((root / "ralph/next/b/STATE.md").read_text(), self.ROWS)

    def test_ralph_state_names_the_queue_when_no_argument_does(self):
        with tempfile.TemporaryDirectory() as tmp:
            sha = self.fixture(tmp)
            r = self.mark(tmp, "qa-1", sha, state_env="ralph/next/b/STATE.md")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn(f"- [x] qa-1 {sha}",
                          (pathlib.Path(tmp) / "ralph/next/b/STATE.md").read_text())

    def test_no_argument_and_no_ralph_state_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            sha = self.fixture(tmp)
            write(tmp, "ralph/next/ring-doc/STATE.md", self.ROWS)   # the old default's path
            r = self.mark(tmp, "qa-1", sha)
            self.assertEqual(r.returncode, 2)
            self.assertIn("RALPH_STATE", r.stderr)
            self.assertEqual((pathlib.Path(tmp) / "ralph/next/ring-doc/STATE.md").read_text(),
                             self.ROWS)


class RalphCheckTests(unittest.TestCase):
    """`ralph-check.sh <name>` runs what the worker's OWN queue.toml declares."""

    def fixture(self, tmp):
        two_queues(tmp)
        install_script(tmp, "ralph-check.sh")
        install_script(tmp, "ralph.py")
        write(tmp, "ralph/next/a/queue.toml",
              '[checks]\nhello = ["sh", "-c", "echo from-a \\"$@\\"", "sh"]\n'
              'red = ["sh", "-c", "echo went-red; exit 7"]\n')

    def check(self, tmp, queue, *argv):
        env = {k: v for k, v in os.environ.items() if k != "RALPH_QUEUE"}
        if queue is not None:
            env["RALPH_QUEUE"] = queue
        return subprocess.run([str(pathlib.Path(tmp) / "scripts/ralph-check.sh"), *argv],
                              capture_output=True, text=True, env=env)

    def test_each_queue_runs_its_own_declaration(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.fixture(tmp)
            a = self.check(tmp, "a", "hello", "extra arg")
            b = self.check(tmp, "b", "hello")
            self.assertEqual((a.returncode, b.returncode), (0, 0), a.stderr + b.stderr)
            self.assertIn("exit=0\nfrom-a extra arg\n", a.stdout)
            self.assertIn("exit=0\nfrom-b\n", b.stdout)
            root = pathlib.Path(tmp)
            self.assertEqual((root / "target/ralph/a/hello.log").read_text(), "from-a extra arg\n")
            self.assertEqual((root / "target/ralph/b/hello.log").read_text(), "from-b\n")

    def test_a_declared_check_exits_with_its_own_code(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.fixture(tmp)
            r = self.check(tmp, "a", "red")
            self.assertEqual(r.returncode, 7)
            self.assertIn("exit=7\nwent-red", r.stdout)

    def test_an_undeclared_name_is_usage_with_or_without_a_queue(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.fixture(tmp)
            for queue in ("b", None, ""):
                r = self.check(tmp, queue, "red")
                self.assertEqual(r.returncode, 2, (queue, r.stdout, r.stderr))
                self.assertIn("usage:", r.stderr)

    def test_a_refused_manifest_fails_the_check_by_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.fixture(tmp)
            write(tmp, "ralph/next/b/queue.toml", "[checks]\nlint = ['true']\n")
            r = self.check(tmp, "b", "hello")
            self.assertEqual(r.returncode, 2)
            self.assertIn("ralph/next/b/queue.toml", r.stderr)


class PromptRenderTests(unittest.TestCase):
    BASE = ("not rendered\n<!-- section: intro -->\n# {{queue}}\n"
            "<!-- section: checks -->\n| LINT |\n<!-- section: rules -->\n- mark {{state}}\n")
    BUILTINS = {"queue": "a", "state": "ralph/next/a/STATE.md",
                "control_dir": "ralph/next/a/ctl", "log_dir": "target/ralph/a"}

    def test_ring_room_renders_byte_for_byte(self):
        # Nothing was lost in the factoring: base + ring-room's addendum, with the
        # legacy control files, IS the PROMPT.md its launch line still reads.
        rendered = ralph.render_prompt(
            (REPO / "ralph/PROMPT.base.md").read_text(),
            (REPO / "ralph/next/ring-room/PROMPT.addendum.md").read_text(),
            {"queue": "ring-room", "state": "ralph/next/ring-room/STATE.md",
             "control_dir": "ralph", "log_dir": "target/ralph"})
        self.assertEqual(rendered.encode(),
                         (REPO / "ralph/next/ring-room/PROMPT.md").read_bytes())

    def test_the_base_alone_renders_with_no_placeholder_left(self):
        text = ralph.render_prompt((REPO / "ralph/PROMPT.base.md").read_text(),
                                   "<!-- section: vars -->\nprefix = qa\n", self.BUILTINS)
        self.assertNotIn("{{", text)
        self.assertNotIn("<!-- section", text)
        self.assertIn("`ralph/next/a/ctl/NEEDS_HUMAN.md`", text)
        self.assertIn("| `REVIEW-mint-qa-` |", text)

    def test_an_addendum_replaces_appends_places_and_adds_sections(self):
        addendum = ("<!-- section: vars -->\nprefix = qa\n"
                    "<!-- section: checks append -->\n| PILOT {{prefix}} |\n"
                    "<!-- section: rules -->\n- never push\n"
                    "<!-- section: window after=intro -->\nONE gpu window\n"
                    "<!-- section: tail -->\nthe end\n")
        self.assertEqual(ralph.render_prompt(self.BASE, addendum, self.BUILTINS),
                         "# a\nONE gpu window\n| LINT |\n| PILOT qa |\n- never push\nthe end\n")

    def test_what_cannot_be_rendered_is_refused_by_name(self):
        for addendum, word in (("<!-- section: rules -->\n{{prefx}}\n", "prefx"),
                               ("<!-- section: vars -->\nstate = elsewhere\n", "state"),
                               ("<!-- section: ghost append -->\nx\n", "ghost"),
                               ("<!-- section: w after=ghost -->\nx\n", "ghost"),
                               ("<!-- section: rules -->\na\n<!-- section: rules -->\nb\n",
                                "rules")):
            with self.assertRaisesRegex(ValueError, word):
                ralph.render_prompt(self.BASE, addendum, self.BUILTINS)

    def campaign_prompt(self, tmp, name):
        paths = ralph.paths_for(ralph.build_parser().parse_args(
            ["run", "--workdir", tmp, "--queue", name]))
        seen = []
        c = ralph.Campaign(paths, notify_enabled=False, sleep=lambda s: None, max_stall=2,
                           session_run=lambda args, prompt, log: seen.append(prompt))
        buf = io.StringIO()
        with mock.patch.object(ralph, "head_of", return_value="a" * 40), \
                contextlib.redirect_stdout(buf):
            c.run()
        return seen, buf.getvalue()

    def test_a_queue_with_an_addendum_runs_on_the_render_and_logs_its_hash(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            write(tmp, "ralph/PROMPT.base.md", self.BASE)
            write(tmp, "ralph/next/a/PROMPT.addendum.md", "<!-- section: rules -->\n- a only\n")
            seen, said = self.campaign_prompt(tmp, "a")
            self.assertTrue(seen[0].endswith("# a\n| LINT |\n- a only\n"), seen[0])
            import hashlib
            digest = hashlib.sha256(b"# a\n| LINT |\n- a only\n").hexdigest()[:16]
            self.assertEqual(said.count(f"sha256={digest}"), 1)     # once, not per iteration
            self.assertIn("ralph/PROMPT.base.md + ralph/next/a/PROMPT.addendum.md", said)
            seen_b, said_b = self.campaign_prompt(tmp, "b")
            self.assertTrue(seen_b[0].endswith("prompt of b\n"))
            self.assertIn("prompt: ralph/next/b/PROMPT.md sha256=", said_b)

    def test_a_declared_prompt_is_read_as_written_even_beside_an_addendum(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            write(tmp, "ralph/PROMPT.base.md", self.BASE)
            write(tmp, "ralph/next/a/PROMPT.addendum.md", "<!-- section: rules -->\n- a only\n")
            write(tmp, "ralph/next/a/queue.toml", 'prompt = "ralph/next/a/PROMPT.md"\n')
            seen, _ = self.campaign_prompt(tmp, "a")
            self.assertTrue(seen[0].endswith("prompt of a\n"))


class HostTests(unittest.TestCase):
    """Backend SELECTION and the absent-backend path, with the platform and the
    `which` lookup injected. The Linux backends are not exercised on a Linux host here."""

    def host(self, platform, present, home):
        calls = []

        def run(argv, **kw):
            calls.append(list(argv))
            return subprocess.CompletedProcess(argv, 0, "", "")

        host = ralph.host_for(platform=platform, run=run, home=pathlib.Path(home),
                              which=lambda tool: tool if tool in present else None)
        return host, calls

    def said(self, fn):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            result = fn()
        return result, buf.getvalue()

    def test_the_platform_picks_the_backend(self):
        with tempfile.TemporaryDirectory() as home:
            for platform, cls in (("darwin", ralph.MacHost), ("linux", ralph.LinuxHost),
                                  ("win32", ralph.Host)):
                self.assertIs(type(self.host(platform, (), home)[0]), cls)

    def test_each_backend_notifies_with_its_own_tool(self):
        with tempfile.TemporaryDirectory() as home:
            mac, mac_calls = self.host("darwin", ("/usr/bin/osascript",), home)
            linux, linux_calls = self.host("linux", ("notify-send",), home)
            self.assertTrue(mac.notify("OPERATOR — halt", "reason"))
            self.assertTrue(linux.notify("OPERATOR — halt", "reason"))
            self.assertEqual(mac_calls[0][:2], ["/usr/bin/osascript", "-e"])
            self.assertEqual(linux_calls, [["notify-send", "ralph: OPERATOR — halt", "reason"]])

    def test_an_absent_notifier_is_reported_with_the_message_it_could_not_show(self):
        with tempfile.TemporaryDirectory() as home:
            for platform, tool in (("linux", "notify-send"), ("darwin", "/usr/bin/osascript"),
                                   ("win32", "no notification backend")):
                host, calls = self.host(platform, (), home)
                delivered, said = self.said(lambda: host.notify("OPERATOR — loop down", "b is down"))
                self.assertFalse(delivered)
                self.assertEqual(calls, [])
                self.assertIn(tool, said)
                self.assertIn("OPERATOR — loop down: b is down", said)

    def test_linux_installs_and_starts_a_systemd_run_job(self):
        with tempfile.TemporaryDirectory() as home:
            host, calls = self.host("linux", ("systemd-run", "systemctl"), home)
            spec = host.install_job("dev.ralph.x-a", ["python3", "ralph.py", "run", "--queue", "a"],
                                    "/w", "/w/log.txt")
            self.assertTrue(spec.is_file())
            host.start_job("dev.ralph.x-a")
            start = calls[-1]
            self.assertEqual(start[:4], ["systemd-run", "--user", "--unit", "dev.ralph.x-a"])
            self.assertIn("--working-directory=/w", start)
            self.assertEqual(start[-5:], ["python3", "ralph.py", "run", "--queue", "a"])
            host.install_job("dev.ralphwatch.x-a", ["python3", "ralph.py", "watch"], "/w",
                             "/w/watch.log", interval=120)
            host.start_job("dev.ralphwatch.x-a")
            self.assertIn("--on-unit-active=120", calls[-1])

    def test_an_absent_job_backend_refuses_to_start_and_says_it_cannot_see(self):
        with tempfile.TemporaryDirectory() as home:
            for platform, tool in (("linux", "systemd-run"), ("darwin", "launchctl"),
                                   ("win32", "no job backend")):
                host, calls = self.host(platform, (), home)
                with self.assertRaisesRegex(ralph.HostError, tool):
                    host.start_job("dev.ralph.x-a")
                running, said = self.said(lambda: host.job_running("dev.ralph.x-a"))
                self.assertFalse(running)
                self.assertIn("cannot tell whether dev.ralph.x-a is running", said)
                self.assertEqual(calls, [])

    def test_start_reports_an_absent_backend_as_exit_two(self):
        with tempfile.TemporaryDirectory() as tmp:
            host, _ = self.host("linux", (), tmp)
            with mock.patch.object(ralph, "host", return_value=host):
                rc, _, err = quiet_main(["start", "--workdir", tmp])
            self.assertEqual(rc, 2)
            self.assertIn("start:", err)


class ShimTests(unittest.TestCase):
    def test_a_dropped_variant_is_said_once(self):
        # The empty prompt makes the shim exit 2 BEFORE it reaches `claude`: no session.
        r = subprocess.run([str(SCRIPTS / "ralph-claude-shim.sh"), "run", "--model", "m",
                            "--variant", "high", "--variant", "max", ""],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)
        self.assertIn("empty prompt", r.stderr)
        self.assertEqual(r.stderr.count("--variant"), 1, r.stderr)
        self.assertIn("high", r.stderr)


class ReviewRoutingTests(unittest.TestCase):
    def test_review_is_the_prefix_or_the_row_tag_never_a_substring(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md",
                  "- [ ] REVIEW-audit-1 — depends [] — audit\n"
                  "- [ ] qa-peer-review-form — depends [] — build the review form\n"
                  "- [ ] qa-hard — depends [] — review = true — needs judgment\n"
                  "- [ ] qa-prose — depends [] — say review = true in the docs\n")
            rows = ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md").by_id()
            routed = {i: ralph.select_model_args(r, "w", "r", "")[1] for i, r in rows.items()}
            self.assertEqual(routed, {"REVIEW-audit-1": "r", "qa-peer-review-form": "w",
                                      "qa-hard": "r", "qa-prose": "w"})
            self.assertEqual(ralph.select_model_args("qa-peer-review-form", "w", "r", ""),
                             ["--model", "w"])


class Ei7Stage0MigrationTests(unittest.TestCase):
    """The first queue on a manifest. Its STATE.md launch block and PROMPT.md are
    not edited (a running loop reads them); the manifest must agree with both."""

    def paths(self, verb="run"):
        args = ralph.build_parser().parse_args([verb, "--workdir", str(REPO),
                                                "--queue", "ei7-stage0"])
        with contextlib.redirect_stdout(io.StringIO()):
            return args, ralph.paths_for(args)

    def test_the_manifest_carries_what_the_launch_line_carried_by_flag_and_env(self):
        args, paths = self.paths("supervise")
        legacy = ralph.paths_for(ralph.build_parser().parse_args(
            launch_lines("ralph/next/ei7-stage0/STATE.md")[0]))
        self.assertEqual((paths.state, paths.charter), (legacy.state, legacy.charter))
        self.assertEqual((args.label, args.session_timeout), ("ei7-stage0", 7200))
        self.assertEqual(ralph.resolve_models(args, paths),
                         {"MODEL": "claude-opus-5", "REVIEW_MODEL": "claude-opus-5",
                          "RESOLVE_MODEL": "claude-opus-5", "VARIANT": "high"})
        self.assertEqual(ralph.worker_bin(paths), str(REPO / "scripts/ralph-claude-shim.sh"))
        self.assertEqual(paths.control_dir, "ralph/next/ei7-stage0/ctl")
        for check in ("desktop", "pilot", "py", "campaign", "node"):
            self.assertIn(check, paths.manifest.checks)
        self.assertEqual(paths.manifest.checks["pilot"],
                         ("research/ontology-retrieval/pilot/run-pilot.sh",))

    def test_the_rendered_prompt_keeps_every_line_of_the_hand_made_one(self):
        _, paths = self.paths()
        self.assertEqual(paths.prompt_addendum, "ralph/next/ei7-stage0/PROMPT.addendum.md")
        legacy = dataclasses_replace(paths, **ralph.Paths.control_files("ralph"),
                                     log_dir="target/ralph")
        rendered = set(ralph.prompt_text(legacy)[0].splitlines())
        # The two lines the render is MEANT to change: the mark call no longer needs the
        # third argument (RALPH_STATE), and a sed-made anecdote that never happened to e7.
        meant = ("ralph-mark.sh <unit-id> <short-hash> ralph/next/ei7-stage0/STATE.md",
                 "e7-1-scaffold")
        lost = [l for l in (REPO / "ralph/next/ei7-stage0/PROMPT.md").read_text().splitlines()
                if l not in rendered and not any(m in l for m in meant)]
        self.assertEqual(lost, [])
        live = ralph.prompt_text(paths)[0]
        self.assertNotIn("{{", live)
        self.assertIn("`ralph/next/ei7-stage0/ctl/NEEDS_HUMAN.md`", live)
        self.assertNotIn("`ralph/NEEDS_HUMAN.md`", live)


class FastSession(ralph.Session):
    def __init__(self, *args, **kwargs):
        kwargs["poll"] = 0.2
        super().__init__(*args, **kwargs)


class TwoQueueDemoTests(unittest.TestCase):
    """The order's Demo, end to end, with a stub worker (no model session): two
    `run --queue` loops in one checkout, then the unedited ring-room launch line."""

    WORKER = (
        'set -eu\n'
        'if [ "$RALPH_QUEUE" = a ]; then\n'          # a never finishes: it waits to be stopped
        '    while [ ! -f "$RALPH_CONTROL_DIR/STOP" ]; do sleep 0.1; done; exit 0\n'
        'fi\n'
        'unit=$(sed -n "s/^- \\[[ ~]\\] \\([^ ]*\\) .*/\\1/p" "$RALPH_STATE" | head -1)\n'
        'scripts/ralph-check.sh hello > "check-${RALPH_QUEUE:-legacy}.out" 2>&1 || true\n'
        'echo work > "$unit.txt"; git add "$unit.txt"; git commit -q -m "$unit: work"\n'
        'scripts/ralph-mark.sh "$unit" "$(git rev-parse --short HEAD)"\n'   # two arguments
        'mkdir -p "$RALPH_CONTROL_DIR"; : > "$RALPH_CONTROL_DIR/DONE"\n')

    def test_two_queues_run_beside_each_other_and_the_legacy_line_still_starts(self):
        import threading
        with tempfile.TemporaryDirectory() as tmp:
            tmp = str(pathlib.Path(tmp).resolve())
            git_repo(tmp)
            two_queues(tmp)
            for name in ("a", "b"):
                manifest = pathlib.Path(tmp) / f"ralph/next/{name}/queue.toml"
                manifest.write_text('worker_bin = "./stub-worker.sh"\n' + manifest.read_text())
            for script in ("ralph-check.sh", "ralph-mark.sh", "ralph.py"):
                install_script(tmp, script)
            stub = stub_worker(tmp, self.WORKER)
            ring_room = launch_lines("ralph/next/ring-room/STATE.md")[1]
            write(tmp, "ralph/next/ring-room/STATE.md", "- [ ] rr-1 — depends [] — do it\n")
            write(tmp, "ralph/next/ring-room/PROMPT.md", "legacy prompt\n")
            write(tmp, ".gitignore", "target/\ncheck-*.out\nralph/next/*/ctl/\n"
                                     "ralph/DONE\nralph/.heartbeat\n")
            commit_all(tmp)
            root = pathlib.Path(tmp)
            rcs = {}

            def loop(name):
                args = ralph.build_parser().parse_args(
                    ["run", "--workdir", tmp, "--queue", name, "--max-stall", "2"])
                rcs[name] = ralph.cmd_run(args)

            out = io.StringIO()
            with mock.patch.object(ralph, "Session", FastSession), contextlib.redirect_stdout(out):
                threads = {name: threading.Thread(target=loop, args=(name,)) for name in "ab"}
                for t in threads.values():
                    t.start()
                threads["b"].join(timeout=60)
                self.assertFalse(threads["b"].is_alive(), out.getvalue())
                self.assertEqual(rcs.get("b"), 0, out.getvalue())
                self.assertTrue(threads["a"].is_alive())        # b's DONE did not end a
                (root / "ralph/next/a/ctl/STOP").write_text("")
                threads["a"].join(timeout=60)
                self.assertFalse(threads["a"].is_alive(), out.getvalue())
            self.assertEqual(rcs, {"a": 0, "b": 0})
            self.assertIn("campaign: operator-stop", out.getvalue())
            self.assertIn("campaign: done", out.getvalue())
            self.assertIn("from-b", (root / "check-b.out").read_text())
            self.assertRegex((root / "ralph/next/b/STATE.md").read_text(), r"- \[x\] qb-1 [0-9a-f]{7}")
            self.assertIn("- [ ] qa-1", (root / "ralph/next/a/STATE.md").read_text())
            self.assertFalse((root / "ralph/next/b/ctl/STOP").exists())
            self.assertFalse((root / "ralph/STOP").exists())
            self.assertFalse((root / "ralph/DONE").exists())

            # Then the ring-room launch line, exactly as its STATE.md has it (`--workdir .`).
            cwd = os.getcwd()
            os.chdir(tmp)
            try:
                with mock.patch.object(ralph, "Session", FastSession), \
                        mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": str(stub)}), \
                        contextlib.redirect_stdout(out):
                    rc = ralph.cmd_run(ralph.build_parser().parse_args(ring_room))
            finally:
                os.chdir(cwd)
            self.assertEqual(rc, 0, out.getvalue())
            self.assertRegex((root / "ralph/next/ring-room/STATE.md").read_text(),
                             r"- \[x\] rr-1 [0-9a-f]{7}")
            self.assertTrue((root / "ralph/DONE").exists())          # legacy control files
            self.assertTrue((root / "ralph/.heartbeat").exists())
            self.assertIn("usage:", (root / "check-legacy.out").read_text())   # no queue, no `hello`


class AuditCadenceTests(unittest.TestCase):
    """`audit_every` in queue.toml: the run loop owes an audit row, not the queue's author."""

    ROWS = "".join(f"- [ ] u-{n} — depends [] — do it\n" for n in (1, 2, 3))
    TWO_DONE = ("- [x] u-1 abcdef1 — depends []\n- [x] u-2 abcdef2 — depends []\n"
                "- [ ] u-3 — depends []\n")

    def run_queue(self, tmp, manifest, rows, *, finish=True, commit_rc=0, files=()):
        """Runs queue `q` with a session that marks the unit it was handed (or
        leaves it, `finish=False`). Returns (result, dispatched ids, commit subjects)."""
        write(tmp, "ralph/next/q/queue.toml", manifest)
        state = write(tmp, "ralph/next/q/STATE.md", rows)
        write(tmp, "ralph/next/q/PROMPT.md", "Execute the selected unit.\n")
        for rel, text in files:
            write(tmp, rel, text)
        paths = ralph.paths_for(ralph.build_parser().parse_args(
            ["run", "--workdir", tmp, "--queue", "q"]))
        seen, commits, heads = [], [], {"h": 0}

        def session(model_args, prompt, log):
            unit = prompt.split()[2]                     # "Your unit: <id> — ..."
            seen.append(unit)
            if finish:
                q = ralph.Queue(state)
                q.set_status(unit, ralph.Status.DONE)
                heads["h"] += 1
                if q.all_done():
                    write(tmp, "ralph/next/q/ctl/DONE", "")

        def commit(p, subject):
            commits.append((p.state, subject))
            return subprocess.CompletedProcess([], commit_rc, "", "index.lock exists")

        c = ralph.Campaign(paths, session_run=session, notify_enabled=False,
                           sleep=lambda s: None, max_stall=2, max_iter=12)
        with mock.patch.object(ralph, "head_of", side_effect=lambda wd: str(heads["h"])), \
                mock.patch.object(ralph, "commit_state", side_effect=commit, create=True), \
                contextlib.redirect_stdout(io.StringIO()):
            result = c.run()
        return result, seen, commits

    def test_an_audit_row_is_inserted_and_run_after_two_units_and_not_before(self):
        with tempfile.TemporaryDirectory() as tmp:
            result, seen, commits = self.run_queue(tmp, "audit_every = 2\n", self.ROWS)
            self.assertEqual(result.outcome, ralph.Outcome.DONE)
            self.assertEqual(seen, ["u-1", "u-2", "REVIEW-audit-q-auto-1", "u-3"])
            self.assertEqual(commits, [("ralph/next/q/STATE.md",
                                        "ralph: audit due after 2 units — REVIEW-audit-q-auto-1")])
            rows = ralph.Queue(pathlib.Path(tmp) / "ralph/next/q/STATE.md").rows
            self.assertEqual([(r.id, r.deps) for r in rows][2:],
                             [("REVIEW-audit-q-auto-1", ("u-2",)), ("u-3", ())])

    def test_the_id_takes_the_addendums_prefix_var_and_counts_its_own_kind(self):
        with tempfile.TemporaryDirectory() as tmp:
            rows = "- [x] REVIEW-audit-zz-auto-1 abcdef0 — depends []\n" + self.TWO_DONE
            _, seen, _ = self.run_queue(tmp, "audit_every = 2\n", rows, files=(
                ("ralph/PROMPT.base.md", "<!-- section: main -->\nwork on {{prefix}}\n"),
                ("ralph/next/q/PROMPT.addendum.md", "<!-- section: vars -->\nprefix = zz\n")))
            self.assertEqual(seen, ["REVIEW-audit-zz-auto-2", "u-3"])

    def test_a_prefix_var_that_cannot_be_an_id_falls_back_to_the_queue_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, seen, _ = self.run_queue(tmp, "audit_every = 2\n", self.TWO_DONE, files=(
                ("ralph/PROMPT.base.md", "<!-- section: main -->\nwork on {{prefix}}\n"),
                ("ralph/next/q/PROMPT.addendum.md", "<!-- section: vars -->\nprefix = e7 or rb\n")))
            self.assertEqual(seen, ["REVIEW-audit-q-auto-1", "u-3"])

    def test_a_queue_without_the_key_never_gets_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            result, seen, commits = self.run_queue(tmp, "", self.ROWS)
            self.assertEqual(result.outcome, ralph.Outcome.DONE)
            self.assertEqual((seen, commits), (["u-1", "u-2", "u-3"], []))
            self.assertNotIn("REVIEW-audit",
                             (pathlib.Path(tmp) / "ralph/next/q/STATE.md").read_text())

    def test_a_next_row_that_is_already_an_audit_is_not_given_a_second(self):
        with tempfile.TemporaryDirectory() as tmp:
            rows = ("- [x] u-1 abcdef1 — depends []\n- [x] u-2 abcdef2 — depends []\n"
                    "- [ ] REVIEW-audit-q — depends [u-2] — the author's own\n"
                    "- [ ] u-3 — depends [REVIEW-audit-q]\n")
            _, seen, commits = self.run_queue(tmp, "audit_every = 2\n", rows)
            self.assertEqual((seen, commits), (["REVIEW-audit-q", "u-3"], []))

    def test_an_audit_the_worker_left_open_is_run_again_not_inserted_again(self):
        with tempfile.TemporaryDirectory() as tmp:
            result, seen, commits = self.run_queue(tmp, "audit_every = 2\n", self.TWO_DONE,
                                                   finish=False)
            self.assertEqual(result.outcome, ralph.Outcome.HALT)           # stalled
            self.assertEqual(seen, ["REVIEW-audit-q-auto-1"] * 2)
            self.assertEqual(len(commits), 1)

    def test_a_resumed_row_is_finished_first(self):
        """`current()` hands back the [~] row until it closes, so a row inserted
        above one would never be dispatched and would be inserted again each pass."""
        with tempfile.TemporaryDirectory() as tmp:
            rows = self.TWO_DONE.replace("[ ] u-3", "[~] u-3") + "- [ ] u-4 — depends []\n"
            _, seen, commits = self.run_queue(tmp, "audit_every = 2\n", rows)
            self.assertEqual(seen, ["u-3", "REVIEW-audit-q-auto-1", "u-4"])
            self.assertEqual(len(commits), 1)

    def test_a_refused_commit_halts_with_gits_words(self):
        with tempfile.TemporaryDirectory() as tmp:
            result, seen, _ = self.run_queue(tmp, "audit_every = 2\n", self.TWO_DONE,
                                             commit_rc=1)
            self.assertEqual((result.outcome, seen), (ralph.Outcome.HALT, []))
            self.assertIn("index.lock exists", result.reason)


class AuditCadenceDemoTests(unittest.TestCase):
    """The same, through `run --queue` with a stub worker and a real git."""

    WORKER = (
        'set -eu\n'
        'unit=$(sed -n "s/^- \\[[ ~]\\] \\([^ ]*\\) .*/\\1/p" "$RALPH_STATE" | head -1)\n'
        'echo work > "$unit.txt"; git add "$unit.txt"; git commit -q -m "$unit: work" -- "$unit.txt"\n'
        'scripts/ralph-mark.sh "$unit" "$(git rev-parse --short HEAD)"\n'
        'grep -q "^- \\[[ ~]\\]" "$RALPH_STATE" || '
        '{ mkdir -p "$RALPH_CONTROL_DIR"; : > "$RALPH_CONTROL_DIR/DONE"; }\n')

    def test_the_audit_row_is_committed_alone_and_worked_between_the_second_and_third_unit(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = str(pathlib.Path(tmp).resolve())
            git_repo(tmp)
            write(tmp, "ralph/next/q/queue.toml",
                  'worker_bin = "./stub-worker.sh"\naudit_every = 2\n')
            write(tmp, "ralph/next/q/STATE.md", AuditCadenceTests.ROWS)
            write(tmp, "ralph/next/q/PROMPT.md", "prompt of q\n")
            write(tmp, ".gitignore", "target/\nralph/next/*/ctl/\n")
            for script in ("ralph-mark.sh", "ralph.py"):
                install_script(tmp, script)
            stub_worker(tmp, self.WORKER)
            commit_all(tmp)
            write(tmp, "stray.txt", "someone else's staged work\n")
            subprocess.run(["git", "-C", tmp, "add", "stray.txt"], check=True)
            out = io.StringIO()
            with mock.patch.object(ralph, "Session", FastSession), contextlib.redirect_stdout(out):
                rc = ralph.cmd_run(ralph.build_parser().parse_args(
                    ["run", "--workdir", tmp, "--queue", "q", "--max-stall", "2"]))
            self.assertEqual(rc, 0, out.getvalue())

            def git(*args):
                return subprocess.run(["git", "-C", tmp, *args], capture_output=True,
                                      text=True, check=True).stdout
            subjects = git("log", "--reverse", "--format=%s").splitlines()[1:]
            audit = "REVIEW-audit-q-auto-1"
            self.assertEqual(subjects, [
                "u-1: work", "ralph: u-1 done", "u-2: work", "ralph: u-2 done",
                f"ralph: audit due after 2 units — {audit}",
                f"{audit}: work", f"ralph: {audit} done", "u-3: work", "ralph: u-3 done"])
            self.assertEqual(git("show", "--name-only", "--format=", "HEAD~4").split(),
                             ["ralph/next/q/STATE.md"])
            self.assertIn("A  stray.txt", git("status", "--porcelain"))      # staged, never swept in
            self.assertIn(f"unit {audit} — ", out.getvalue())


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


class WaitingLane:
    """A lane session whose detached run outlives it (the r9-boundary-sweep
    shape, struck out twice for it, 2026-09-19): it banks `ralph/waiting`
    naming `ralph/field-x.done` and ends. Only once the marker has landed
    does a respawned session finish the unit."""

    runs = 0

    def __init__(self, cwd):
        self.cwd = pathlib.Path(cwd)

    def run(self, model_args, prompt, log):
        WaitingLane.runs += 1
        unit = self.cwd.name
        if (self.cwd / "ralph" / "field-x.done").exists():
            (self.cwd / "ralph" / "lanes").mkdir(parents=True, exist_ok=True)
            (self.cwd / "ralph" / "lanes" / f"{unit}.done").write_text("")
            subprocess.run(["git", "-C", str(self.cwd), "add",
                            f"ralph/lanes/{unit}.done"], check=True)
        else:
            (self.cwd / "ralph" / "waiting").write_text(
                f"waiting on ralph/field-x.done — health check #{WaitingLane.runs}\n")
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

    def test_a_conflicts_line_of_n_ids_means_every_pair(self):
        # ring-doc's line names three rows; until 2026-09-19 only the first two conflicted.
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write(tmp, "ralph/conflicts.txt", (REPO / "ralph/next/ring-doc/conflicts.txt").read_text())
            write(tmp, "ralph/STATE.md", "".join(
                f"- [ ] {u} — depends []\n"
                for u in ("rd-1-adapter", "rd-1-awareness", "rd-1-attribution")))
            pairs = self.make(root, lambda cwd, env=None: None)._conflict_pairs()
            for a, b in (("rd-1-adapter", "rd-1-awareness"), ("rd-1-adapter", "rd-1-attribution"),
                         ("rd-1-awareness", "rd-1-attribution")):
                self.assertIn(frozenset((a, b)), pairs)
            self.assertEqual(ralph.Queue(root / "ralph/STATE.md").pick_wave(3, pairs),
                             ["rd-1-adapter"])
            self.assertEqual(ralph.conflict_pairs("a b  # why\n# c d\n\nsolo\n"),
                             {frozenset(("a", "b"))})

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


class PoolWaitingTests(unittest.TestCase):
    """A lane that ends on `ralph/waiting` naming a marker is WAITING, not a
    failure: no strike, the tick polls the marker and respawns the lane when
    it lands, and waiting past LANE_MAX_WAIT_SECS escalates with a package
    naming the unit, the marker and the elapsed time."""

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
        kwargs.setdefault("sleep", lambda s: None)
        return ralph.Pool(paths, session_for=session_for, notify_enabled=False,
                          lanes=2, base_branch="main", **kwargs)

    def test_a_waiting_end_does_not_count_as_a_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-wait — depends []\n")
            pool = self.make(root, lambda cwd, env=None: WaitingLane(cwd))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertIsNone(pool.run_wave(["dm-wait"]))
            self.assertNotIn("dm-wait", pool._lane_failures)
            self.assertTrue((root / ".ralph/wt/dm-wait/ralph/waiting").exists())
            self.assertIn("waiting on ralph/field-x.done", out.getvalue())
            self.assertNotIn("(failure 1/", out.getvalue())

    def test_the_marker_appearing_respawns_the_lane(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-wait — depends []\n")
            ticks = []

            def tick(secs):
                ticks.append(secs)
                if len(ticks) == 2:      # the detached run finishes on the 2nd poll
                    write(root / ".ralph/wt/dm-wait", "ralph/field-x.done", "")

            pool = self.make(root, lambda cwd, env=None: WaitingLane(cwd), sleep=tick)
            WaitingLane.runs = 0
            self.assertEqual(pool.run(), 0)
            self.assertTrue((root / "ralph/DONE").exists())
            self.assertFalse((root / ".ralph/wt/dm-wait/ralph/waiting").exists())
            self.assertEqual(WaitingLane.runs, 2)
            self.assertTrue(ralph.Queue(root / "ralph/STATE.md").all_done())
            # The lane committed its waiting file, and the finishing session
            # commits by name — the resume must still end the waiting ON THE
            # LANE BRANCH, or the merge parks the main tree's loop on a marker
            # that only ever existed in the worktree.
            self.assertFalse((root / "ralph/waiting").exists())

    def test_a_lane_waiting_past_max_wait_escalates(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-wait — depends []\n")

            def tick(secs):
                old = time.time() - 49 * 3600   # past the 48h bound
                os.utime(root / ".ralph/wt/dm-wait/ralph/waiting", (old, old))

            pool = self.make(root, lambda cwd, env=None: WaitingLane(cwd), sleep=tick)
            WaitingLane.runs = 0
            self.assertEqual(pool.run(), 3)
            pkg = (root / "ralph/NEEDS_HUMAN.md").read_text()
            self.assertIn("dm-wait", pkg)
            self.assertIn("field-x.done", pkg)
            self.assertIn("49h", pkg)
            self.assertEqual(WaitingLane.runs, 1)


class RosterProbeTests(unittest.TestCase):
    """MODEL/REVIEW_MODEL are rosters (order ralph-model-roster): at wave
    dispatch the pool probes each model in order — one minimal chat call
    through the lane client, 20s — and the first healthy one runs the wave,
    stamped in the log line. An all-dead roster parks the campaign naming
    every cause, so a director reads one line, not three transcripts."""

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
        kwargs.setdefault("sleep", lambda s: None)
        return ralph.Pool(paths, session_for=session_for, notify_enabled=False,
                          lanes=2, base_branch="main", **kwargs)

    def test_a_dead_first_roster_falls_through_to_the_healthy_model(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-a — depends []\n")
            probed = []

            def probe(model):
                probed.append(model)
                if model == "prov/alive":
                    return True, ""
                return False, "Usage limit reached for 5 hour (resets 05:37Z)"

            seen = []

            class ModelLane(FakeLane):
                def run(self, model_args, prompt, log):
                    seen.append(model_args)
                    return super().run(model_args, prompt, log)

            pool = self.make(root, lambda cwd, env=None: ModelLane(cwd),
                             model="prov/dead,prov/alive", probe=probe)
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(pool.run(), 0)
            self.assertEqual(probed, ["prov/dead", "prov/alive"])
            self.assertEqual(seen[0][:2], ["--model", "prov/alive"])
            self.assertIn("pool: probe prov/dead — Usage limit reached", out.getvalue())
            self.assertIn("pool: wave dm-a · model prov/alive", out.getvalue())

    def test_a_single_model_value_is_probed_too(self):
        # "Single-model values behave exactly as today (the probe still
        # runs; it replaces the wave-failure discovery)."
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-a — depends []\n")
            probed = []

            def probe(model):
                probed.append(model)
                return True, ""

            pool = self.make(root, lambda cwd, env=None: FakeLane(cwd),
                             model="prov/solo", probe=probe)
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(pool.run(), 0)
            self.assertEqual(probed, ["prov/solo"])
            self.assertIn("pool: wave dm-a · model prov/solo", out.getvalue())

    def test_an_all_dead_roster_parks_naming_each_cause(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-a — depends []\n")
            causes = {"prov/quota": "Usage limit reached for 5 hour (resets 05:37Z)",
                      "prov/gone": "Unexpected server error",
                      "prov/slow": "timeout after 20s"}
            sessions = []
            pool = self.make(root, lambda cwd, env=None: sessions.append(cwd),
                             model=",".join(causes),
                             probe=lambda m: (False, causes[m]))
            self.assertEqual(pool.run(), 3)
            self.assertEqual(sessions, [])          # no lane ever ran
            self.assertFalse((root / ".ralph" / "wt" / "dm-a").exists())
            pkg = (root / "ralph/NEEDS_HUMAN.md").read_text()
            self.assertIn("no healthy model", pkg)
            for model, cause in causes.items():
                self.assertIn(f"{model}: {cause}", pkg)
            self.assertIn("halt: no healthy model", (root / "ralph/STOP").read_text())

    def test_the_review_roster_probes_and_stamps_the_review_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] REVIEW-a — depends []\n")
            probed = []

            def probe(model):
                probed.append(model)
                if model == "prov/rok":
                    return True, ""
                return False, "Unexpected server error"

            seen = []

            class RecordingReview(FakeReview):
                def run(self, model_args, prompt, log):
                    seen.append(model_args)
                    return super().run(model_args, prompt, log)

            pool = self.make(root, lambda cwd: RecordingReview(cwd),
                             model="prov/worker",
                             review_model="prov/rdead,prov/rok", probe=probe)
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(pool.run(), 0)
            self.assertEqual(probed, ["prov/rdead", "prov/rok"])
            self.assertEqual(seen[0][:2], ["--model", "prov/rok"])
            self.assertIn("serial review REVIEW-a (main tree) attempt 1 · model prov/rok",
                          out.getvalue())


class ProbeTests(unittest.TestCase):
    def test_the_probe_hits_the_configured_client_and_keeps_causes(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths = ralph.Paths(pathlib.Path(tmp))
            echo, false = shutil.which("echo"), shutil.which("false")
            with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": echo}):
                self.assertEqual(ralph.probe_model("prov/x", paths), (True, ""))
            with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": false}):
                ok, cause = ralph.probe_model("prov/x", paths)
                self.assertFalse(ok)
                self.assertIn("exit 1", cause)
            slow = pathlib.Path(tmp) / "slow-client.sh"
            slow.write_text("#!/bin/sh\nsleep 5\n")
            slow.chmod(0o755)
            with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": str(slow)}):
                self.assertEqual(ralph.probe_model("prov/x", paths, timeout=1),
                                 (False, "timeout after 1s"))

    def test_the_probe_never_targets_localhost(self):
        # The seam: probe the provider, never the mesh daemon. An entry whose
        # provider is declared with a loopback baseURL is refused by name; a
        # bare id names no provider at all.
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, ".opencode/opencode.json",
                  '{"provider": {"mesh": {"options": {"baseURL": '
                  '"http://localhost:9741/v1"}}}}')
            paths = ralph.Paths(pathlib.Path(tmp))
            ok, cause = ralph.probe_model("mesh/fast", paths)
            self.assertFalse(ok)
            self.assertIn("localhost", cause)
            ok, cause = ralph.probe_model("bare", paths)
            self.assertFalse(ok)
            self.assertIn("no provider", cause)
            with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": shutil.which("echo")}):
                self.assertEqual(ralph.probe_model("prov/x", paths), (True, ""))


class HaltTailTests(unittest.TestCase):
    """A strikeout halt carries the failing lane's last error-shaped
    transcript line (quota / permission path / provider error / crash tail),
    so launchd.log alone is diagnostic."""

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
        kwargs.setdefault("sleep", lambda s: None)
        return ralph.Pool(paths, session_for=session_for, notify_enabled=False,
                          lanes=2, base_branch="main", **kwargs)

    def test_a_lane_strikeout_halt_carries_the_last_error_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] dm-a — depends []\n")

            class DyingLane:
                def __init__(self, cwd):
                    self.cwd = pathlib.Path(cwd)

                def run(self, model_args, prompt, log):
                    p = pathlib.Path(log)
                    p.parent.mkdir(parents=True, exist_ok=True)
                    p.write_text("working...\n"
                                 "provider: Usage limit reached for 5 hour\n"
                                 "Error: Unexpected server error (500)\n")
                    return 0                # ends without its done marker

            pool = self.make(root, lambda cwd, env=None: DyingLane(cwd),
                             max_lane_failures=1)
            self.assertEqual(pool.run(), 3)
            pkg = (root / "ralph/NEEDS_HUMAN.md").read_text()
            self.assertIn("lane dm-a failed 1 waves", pkg)
            self.assertIn("target/ralph/lane-dm-a.out", pkg)
            self.assertIn("last error: Error: Unexpected server error (500)", pkg)

    def test_a_review_exhaustion_halt_carries_the_last_error_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.fixture(tmp, "- [ ] REVIEW-a — depends []\n")

            class FailingReview:
                def run(self, model_args, prompt, log):
                    p = pathlib.Path(log)
                    p.parent.mkdir(parents=True, exist_ok=True)
                    p.write_text("analyzing...\napi error: 429 quota exhausted\n")
                    return 0                # never marks its row [x]

            pool = self.make(root, lambda cwd: FailingReview(),
                             max_review_attempts=1)
            self.assertEqual(pool.run(), 3)
            pkg = (root / "ralph/NEEDS_HUMAN.md").read_text()
            self.assertIn("review REVIEW-a did not finish after 1 attempts", pkg)
            self.assertIn("last error: api error: 429 quota exhausted", pkg)


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

    def test_plan_prints_the_units_since_the_last_audit_and_the_cadence(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            rows = ("- [x] qb-0 abcdef1 — depends []\n- [x] REVIEW-audit-b abcdef2 — depends []\n"
                    "- [x] qb-1 abcdef3 — depends []\n- [x] qb-2 abcdef4 — depends []\n"
                    "- [ ] qb-3 — depends []\n")
            write(tmp, "ralph/next/b/STATE.md", rows)
            write(tmp, "ralph/STATE.md", rows)
            write(tmp, "ralph/PROMPT.md", "legacy prompt\n")
            _, out, _ = quiet_main(["plan", "--workdir", tmp, "--queue", "b"])
            self.assertIn("units since audit: 2  head:", out)
            write(tmp, "ralph/next/b/queue.toml", "audit_every = 6\n")
            _, out, _ = quiet_main(["plan", "--workdir", tmp, "--queue", "b"])
            self.assertIn("units since audit: 2 (audit every 6)  head:", out)
            _, out, _ = quiet_main(["plan", "--workdir", tmp])          # the legacy line
            self.assertIn("units since audit: 2  head:", out)
            self.assertNotIn("n/a", out)

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
