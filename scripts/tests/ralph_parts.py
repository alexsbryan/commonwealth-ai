#!/usr/bin/env python3
"""ralph.py — the tests of the pieces the rewrite carried over from the
file-protocol loop: the queue grammar, manifests, models, prompts, paths, hosts, the
probe, the shell scripts, the watchdog and the lane mechanics (`Lanes`).
The engine (Loop, TRANSITIONS, Ledger, cmd_result, adopt_legacy) is tested
in scripts/tests/ralph.py. In-process, temp git repos, no model calls.

Ported from the file-protocol loop's scripts/tests/ralph.py (before the swap). Classes there that are NOT here, on purpose:
  WaitLimitTests          WAIT_LIMIT_S / resolve_wait_limit are deleted: a session's
                          wait is `ralph-result await <budget>`, bounded by
                          --marker-timeout / AWAIT_MAX_S (engine).
  ModelTests (part)       test_a_models_rewrite_preserves_wait_limit_s: same reason.
  ResolverPromptTests     resolver_prompt is deleted; the director prompt is
                          Loop._director_prompt (engine).
  CampaignTests           Campaign is replaced by Loop (engine). Its dispatch_requires
                          cases that need no driver are in DispatchRequiresTests.
  WaitingMarkerTests,     the ralph/waiting marker protocol and machine_boot_time are
  MainTreeWaitTests,      gone; adopt_legacy reads a waiting file once (engine).
  PoolWaitingTests
  TwoQueueControlTests    its Campaign half is the engine's; the stop/report/watch
    (part)                half is kept below.
  SessionEnvTests (part)  Session is gone: session_env is tested directly, not read
                          back from a stub worker.
  PromptRenderTests       the two Campaign-driven tests now read prompt_text and the
    (part)                `prompt` verb; "the prompt hash is logged once" was a
                          Campaign log line the Loop does not write (engine's call).
  TwoQueueDemoTests,      end-to-end Campaign/Session demos (engine). The audit
  AuditCadenceDemoTests   demo's "the audit row is committed alone, a stray staged
                          file is not swept in" is kept in AuditCadenceTests.
  AuditCadenceTests       re-written as direct tests of audit_due / closing_audit_due /
    (Campaign-driven)     insert_audit; the dispatch ORDER they asserted is the engine's.
  SupervisorTests, SupervisorCooldownTests, SupervisorUnreadableQueueTests,
  SupervisorResolverDidNotRunTests, SurvivalTests
                          the supervisor is retired: the host restarts the loop
                          (install_job keep_alive, HostTests) and the loop holds its
                          own escalations (engine).
  GuardTests              guarded is deleted; Loop._tick_error is the net (engine).
  WatchTests (part)       the needs-human and disk-low conditions are gone: Watch
                          reports only down and stalled.
  LaneRootTests (part)    the Pool half; lane_root_for is kept.
  PoolTests               conflict_pairs is in ConflictPairsTests (wave rules: engine suite); the lane refresh,
                          merge-in and host pointers are in LanesTests; wave merge,
                          conflict strikes, the done-commit failure, serial reviews and
                          the operator stop are the engine's (Loop._land_lane, _mark_done).
  RosterProbeTests        pool dispatch over a roster is the engine's; parse_roster,
                          probe_refusal and probe_model are in ProbeTests.
  HaltTailTests           strikes and continuations are the engine's; error_tail and
                          halt_tail_suffix are in ErrorTailTests.
  PoolReexecTests,        the re-exec between waves is Loop._maybe_reexec (engine).
  PoolDeploysOnlyCommittedCodeTests
  PoolQueueTests          target clone, evidence, worktree removal, disk reclaim and
                          decision renumbering are in LanesTests / DecisionRenumberTests;
                          the cargo-jobs split is CargoJobsShareTests; the legacy pool
                          defaults are in PathsForTests. Running a queue, the cadence in
                          the pool, scope/parked/refused dispatch and the memory/disk
                          floor waits are the engine's. The stale lane-done marker test is
                          retired: a lane ends by `ralph-result`, no marker file is read.
"""
import contextlib
import io
import json
import os
import pathlib
import plistlib
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
import ralph  # noqa: E402


def setUpModule():
    # No test here waits on this host's free memory; the split itself is
    # tested against stubbed probes (CargoJobsShareTests).
    patcher = mock.patch.object(ralph, "cargo_jobs_share", lambda lanes: (2, "test budget"))
    patcher.start()
    unittest.addModuleCleanup(patcher.stop)


def write(root, rel, text):
    p = pathlib.Path(root) / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)
    return p


SCRIPTS = pathlib.Path(os.environ.get("RALPH_TEST_SCRIPTS")
                       or pathlib.Path(__file__).resolve().parents[1])
REPO = pathlib.Path(os.environ.get("RALPH_TEST_REPO")
                    or pathlib.Path(__file__).resolve().parents[2])


def git_repo(tmp):
    subprocess.run(["git", "init", "-q", "-b", "main", str(tmp)], check=True)
    subprocess.run(["git", "-C", str(tmp), "config", "user.email", "t@t"], check=True)
    subprocess.run(["git", "-C", str(tmp), "config", "user.name", "t"], check=True)


def commit_all(tmp, msg="seed"):
    subprocess.run(["git", "-C", str(tmp), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(tmp), "commit", "-q", "-m", msg], check=True)
    return subprocess.run(["git", "-C", str(tmp), "rev-parse", "--short", "HEAD"],
                          capture_output=True, text=True).stdout.strip()


def git(cwd, *args):
    return subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True)


def install_script(tmp, name, source=None):
    dst = pathlib.Path(tmp) / "scripts" / name
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text((SCRIPTS / (source or name)).read_text())
    dst.chmod(0o755)
    return dst


def stub_worker(tmp, body):
    """A worker binary that spends no tokens: `<bin> run [flags] <prompt>` runs `body`."""
    stub = write(tmp, "stub-worker.sh", "#!/usr/bin/env bash\n" + body)
    stub.chmod(0o755)
    return stub


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


def said(fn, *args, **kwargs):
    """(fn's result, what it said on stdout)."""
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        result = fn(*args, **kwargs)
    return result, out.getvalue()


def two_queues(tmp):
    subprocess.run(["git", "init", "-q", str(tmp)], check=True)
    for name, row in (("a", "qa-1"), ("b", "qb-1")):
        write(tmp, f"ralph/next/{name}/queue.toml",
              f'[checks]\nhello = ["sh", "-c", "echo from-{name}"]\n')
        write(tmp, f"ralph/next/{name}/STATE.md", f"- [ ] {row} — depends [] — do it\n")
        write(tmp, f"ralph/next/{name}/PROMPT.md", f"prompt of {name}\n")
    write(tmp, "ralph/STATE.md", "- [ ] legacy-1 — depends [] — the default queue\n")


def queue_paths(tmp, name, verb="run"):
    return ralph.paths_for(ralph.build_parser().parse_args(
        [verb, "--workdir", str(tmp), "--queue", name]))


def in_tree_lanes(paths):
    """The lane tests keep lane worktrees inside their temporary directory, so
    nothing outlives it; production's default sits beside the main tree
    (`lane_root_for`, LaneRootTests)."""
    return paths.workdir / ".ralph" / "wt"


# -- the queue grammar --------------------------------------------------------


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

    def test_a_ready_human_row_waits_while_the_rows_past_it_run(self):
        # phase-b-31: HUMAN-pb-lanes-dials-serve at the head held 40 rows for 9 h.
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md",
                  "- [ ] HUMAN-read — depends []\n- [ ] dm-after — depends [HUMAN-read]\n"
                  "- [ ] dm-free — depends []\n")
            q = ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md")
            self.assertEqual(q.current().id, "dm-free")
            self.assertEqual(q.awaiting_operator(), ["HUMAN-read"])

    def test_a_parked_row_waits_even_when_it_is_the_active_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md",
                  "- [~] dm-stuck — depends []\n- [ ] dm-next — depends [dm-stuck]\n"
                  "- [ ] dm-free — depends []\n")
            q = ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md")
            self.assertEqual(q.current(frozenset({"dm-stuck"})).id, "dm-free")
            self.assertEqual(q.awaiting_operator(frozenset({"dm-stuck"})), ["dm-stuck"])
            self.assertIsNone(ralph.Queue(q.path).current(frozenset({"dm-stuck", "dm-free"})))

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

    def test_a_cycle_among_open_rows_is_refused_by_name(self):
        # Rows nothing can ever make ready: the loop would hold nothing and run nothing.
        for rows, cycle in (
                ("- [ ] a — depends [b]\n- [ ] b — depends [a]\n", "a -> b -> a"),
                ("- [ ] a — depends [a]\n", "a -> a"),
                ("- [x] z abc1234 — depends []\n- [ ] a — depends [c, z]\n"
                 "- [ ] b — depends [a]\n- [ ] c — depends [b]\n", "a -> c -> b -> a")):
            with tempfile.TemporaryDirectory() as tmp:
                write(tmp, "ralph/STATE.md", rows)
                with self.assertRaises(ValueError) as cm:
                    ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md")
                self.assertIn(f"dependency cycle {cycle}", str(cm.exception))

    def test_a_cycle_whose_rows_are_all_done_is_accepted(self):
        # ersilia's queue has one: REVIEW-16's rows closed all [x] in a ring. A
        # done row blocks nothing, so only open-to-open edges can be a cycle.
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/STATE.md",
                  "- [x] REVIEW-16-a abc1231 — depends [REVIEW-16-c]\n"
                  "- [x] REVIEW-16-b abc1232 — depends [REVIEW-16-a]\n"
                  "- [x] REVIEW-16-c abc1233 — depends [REVIEW-16-b]\n"
                  "- [ ] r-next — depends [REVIEW-16-c]\n")
            self.assertEqual(ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md").current().id,
                             "r-next")
            # Half closed: the open row's partner is done, so it is ready, not stuck.
            write(tmp, "ralph/STATE.md", "- [x] a abc1234 — depends [b]\n- [ ] b — depends [a]\n")
            self.assertEqual(ralph.Queue(pathlib.Path(tmp) / "ralph/STATE.md").current().id, "b")

    def test_mark_done_names_the_commit_after_the_id_when_the_row_names_none(self):
        with tempfile.TemporaryDirectory() as tmp:
            state = write(tmp, "ralph/STATE.md",
                          "# queue\n"
                          "- [~] dm-a — depends [] — doing it\n"
                          "  - trial: kept\n"
                          "- [ ] dm-b deadbee — depends [] — already names one\n"
                          "- [ ] dm-c — depends []\n")
            q = ralph.Queue(state)
            q.mark_done("dm-a", "abc1234")
            q.mark_done("dm-b", "abc1234")
            q.mark_done("dm-c")
            self.assertEqual(state.read_text().splitlines(), [
                "# queue",
                "- [x] dm-a abc1234 — depends [] — doing it",
                "  - trial: kept",
                "- [x] dm-b deadbee — depends [] — already names one",
                "- [x] dm-c — depends []"])
            by = ralph.Queue(state).by_id()
            self.assertEqual((by["dm-a"].hash, by["dm-b"].hash, by["dm-c"].hash),
                             ("abc1234", "deadbee", None))
            self.assertTrue(q.all_done())

    def test_mark_done_refuses_a_row_that_moved_under_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            state = write(tmp, "ralph/STATE.md", "- [ ] dm-a — depends []\n")
            q = ralph.Queue(state)
            write(tmp, "ralph/STATE.md", "- [ ] dm-a — depends [] — edited since\n")
            with self.assertRaisesRegex(ValueError, "row moved under mark_done"):
                q.mark_done("dm-a", "abc1234")
            self.assertEqual(state.read_text(), "- [ ] dm-a — depends [] — edited since\n")


class ConflictPairsTests(unittest.TestCase):
    """conflicts.txt, read once (Loop._pick applies it; its wave rules are held
    by the engine suite's PoolTests)."""

    def test_a_conflicts_line_of_n_ids_means_every_pair(self):
        # ring-doc's line names three rows; until 2026-09-19 only the first two conflicted.
        pairs = ralph.conflict_pairs((REPO / "ralph/next/ring-doc/conflicts.txt").read_text())
        for a, b in (("rd-1-adapter", "rd-1-awareness"), ("rd-1-adapter", "rd-1-attribution"),
                     ("rd-1-awareness", "rd-1-attribution")):
            self.assertIn(frozenset((a, b)), pairs)
        self.assertEqual(ralph.conflict_pairs("a b  # why\n# c d\n\nsolo\n"),
                         {frozenset(("a", "b"))})

    def test_a_star_marks_a_row_that_runs_alone(self):
        # phase-c's -measure rows paired themselves with every row by hand, and
        # the seven rows added later were paired with nothing (2026-10-02).
        self.assertIn(frozenset(("dm-m", ralph.ALONE)),
                      ralph.conflict_pairs("dm-m *  # a reading: no lane compiles beside it\n"))


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

    def test_counts_closes_after_the_last_audit_close_not_rows_below_it(self):
        # The failing input: u-old closed BEFORE the audit but sits BELOW it, as
        # minted and parked rows do. Positionally that is 2 units; by time, 1.
        with tempfile.TemporaryDirectory() as tmp:
            git_repo(tmp)
            state = "ralph/STATE.md"

            def close(text, subject):
                write(tmp, state, text)
                subprocess.run(["git", "-C", tmp, "add", state], check=True)
                subprocess.run(["git", "-C", tmp, "commit", "-q", "-m", subject], check=True)

            close("- [ ] u-new — depends []\n- [x] u-old abc0001 — depends []\n",
                  "ralph: u-old done")
            close("- [x] REVIEW-audit-u-auto-1 abc0002 — depends []\n"
                  "- [ ] u-new — depends []\n- [x] u-old abc0001 — depends []\n",
                  "ralph: REVIEW-audit-u-auto-1 done")
            close("- [x] REVIEW-audit-u-auto-1 abc0002 — depends []\n"
                  "- [x] u-new abc0003 — depends []\n- [x] u-old abc0001 — depends []\n",
                  "ralph: u-new done")
            q = ralph.Queue(pathlib.Path(tmp) / state)
            self.assertEqual(q.units_since_audit(), 2)          # the positional reading
            self.assertEqual(ralph.units_since_audit(ralph.Paths(pathlib.Path(tmp)), q), 1)

    def test_a_queue_never_marked_through_git_keeps_the_positional_count(self):
        with tempfile.TemporaryDirectory() as tmp:
            git_repo(tmp)
            q = self.queue(tmp, "- [x] u-1 abcdef1 — depends []\n- [x] u-2 abcdef2 — depends []\n")
            commit_all(tmp, "seed")
            self.assertEqual(ralph.units_since_audit(ralph.Paths(pathlib.Path(tmp)), q), 2)

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
            self.assertRegex(line, r" — read: `docs/ARCH_PRINCIPLES.md` — check: LINT$")
            self.assertEqual(q.path.read_text().splitlines()[0], "# queue")
            self.assertEqual(q.current().id, "REVIEW-audit-u-auto-1")


class AuditCadenceTests(unittest.TestCase):
    """`audit_every` in queue.toml: the loop owes an audit row, not the queue's
    author. The deciders and the insertion, without a loop to drive them."""

    ROWS = "".join(f"- [ ] u-{n} — depends [] — do it\n" for n in (1, 2, 3))
    TWO_DONE = ("- [x] u-1 abcdef1 — depends []\n- [x] u-2 abcdef2 — depends []\n"
                "- [ ] u-3 — depends []\n")

    def queue(self, tmp, manifest, rows, files=()):
        git_repo(tmp)
        write(tmp, "ralph/next/q/queue.toml", manifest)
        state = write(tmp, "ralph/next/q/STATE.md", rows)
        write(tmp, "ralph/next/q/PROMPT.md", "Execute the selected unit.\n")
        for rel, text in files:
            write(tmp, rel, text)
        commit_all(tmp)
        return queue_paths(tmp, "q"), ralph.Queue(state)

    def test_an_audit_is_due_after_the_cadence_and_not_before(self):
        with tempfile.TemporaryDirectory() as tmp:
            rows = "- [x] u-1 abcdef1 — depends []\n- [ ] u-2 — depends []\n"
            paths, q = self.queue(tmp, "audit_every = 2\n", rows)
            self.assertFalse(ralph.audit_due(paths, q, q.by_id()["u-2"]))
        with tempfile.TemporaryDirectory() as tmp:
            paths, q = self.queue(tmp, "audit_every = 2\n", self.TWO_DONE)
            self.assertTrue(ralph.audit_due(paths, q, q.by_id()["u-3"]))

    def test_the_row_lands_above_the_unit_and_is_committed_alone(self):
        # The demo's check: someone's staged work is never swept into the audit commit.
        with tempfile.TemporaryDirectory() as tmp:
            paths, q = self.queue(tmp, "audit_every = 2\n", self.TWO_DONE)
            write(tmp, "stray.txt", "someone else's staged work\n")
            subprocess.run(["git", "-C", tmp, "add", "stray.txt"], check=True)
            (row, refused), out = said(ralph.insert_audit, paths, q, q.by_id()["u-3"])
            self.assertEqual(refused, "")
            self.assertEqual((row.id, row.deps), ("REVIEW-audit-q-auto-1", ("u-2",)))
            q = ralph.Queue(q.path)
            self.assertEqual([r.id for r in q.rows], ["u-1", "u-2", "REVIEW-audit-q-auto-1", "u-3"])
            self.assertEqual(q.current().id, "REVIEW-audit-q-auto-1")
            subject = "ralph: audit due after 2 units — REVIEW-audit-q-auto-1"
            self.assertIn(subject, out)
            self.assertEqual(git(tmp, "log", "-1", "--format=%s").stdout.strip(), subject)
            self.assertEqual(git(tmp, "show", "--name-only", "--format=", "HEAD").stdout.split(),
                             ["ralph/next/q/STATE.md"])
            self.assertIn("A  stray.txt", git(tmp, "status", "--porcelain").stdout)

    def test_a_resumed_row_is_finished_first(self):
        """`current()` hands back the [~] row until it closes, so a row inserted
        above one would never be dispatched and would be inserted again each pass."""
        with tempfile.TemporaryDirectory() as tmp:
            rows = self.TWO_DONE.replace("[ ] u-3", "[~] u-3") + "- [ ] u-4 — depends []\n"
            paths, q = self.queue(tmp, "audit_every = 2\n", rows)
            self.assertFalse(ralph.audit_due(paths, q, q.by_id()["u-3"]))
            self.assertTrue(ralph.audit_due(paths, q, q.by_id()["u-4"]))

    def test_a_row_that_is_already_an_audit_is_not_given_a_second(self):
        with tempfile.TemporaryDirectory() as tmp:
            rows = ("- [x] u-1 abcdef1 — depends []\n- [x] u-2 abcdef2 — depends []\n"
                    "- [ ] REVIEW-audit-q — depends [u-2] — the author's own\n"
                    "- [ ] u-3 — depends [REVIEW-audit-q]\n")
            paths, q = self.queue(tmp, "audit_every = 2\n", rows)
            self.assertFalse(ralph.audit_due(paths, q, q.by_id()["REVIEW-audit-q"]))

    def test_a_queue_without_the_key_never_gets_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths, q = self.queue(tmp, "", self.TWO_DONE.replace("[ ] u-3", "[x] u-3 abcdef3"))
            self.assertFalse(ralph.audit_due(paths, q, q.by_id()["u-3"]))
            self.assertFalse(ralph.closing_audit_due(paths, q))
            self.assertFalse(ralph.closing_audit_due(ralph.Paths(pathlib.Path(tmp)), q))

    def test_a_queue_that_ends_on_its_cadence_closes_with_an_audit(self):
        """svrngs u3, 2026-10-05: audit_due runs only where a unit is dispatched,
        so four units that landed with no row after them closed unaudited."""
        with tempfile.TemporaryDirectory() as tmp:
            rows = "- [x] u-1 abcdef1 — depends []\n- [x] u-2 abcdef2 — depends []\n"
            paths, q = self.queue(tmp, "audit_every = 2\n", rows)
            self.assertTrue(ralph.closing_audit_due(paths, q))
            (row, refused), out = said(ralph.insert_audit, paths, q, None)
            self.assertEqual(refused, "")
            rows = ralph.Queue(q.path).rows
            self.assertEqual((rows[-1].id, rows[-1].deps), ("REVIEW-audit-q-auto-1", ("u-2",)))
            self.assertIn("ralph: closing audit after 2 units — REVIEW-audit-q-auto-1", out)
        with tempfile.TemporaryDirectory() as tmp:
            rows = ("- [x] u-1 abcdef1 — depends []\n- [x] REVIEW-audit-q-auto-1 abcdef2 — "
                    "depends [u-1]\n- [x] u-3 abcdef3 — depends []\n")
            paths, q = self.queue(tmp, "audit_every = 2\n", rows)
            self.assertTrue(ralph.closing_audit_due(paths, q))
            (row, _), out = said(ralph.insert_audit, paths, q, None)
            self.assertEqual(row.id, "REVIEW-audit-q-auto-2")
            self.assertIn("ralph: closing audit after 1 unit — REVIEW-audit-q-auto-2", out)

    def test_a_queue_whose_last_row_is_an_audit_closes_without_another(self):
        with tempfile.TemporaryDirectory() as tmp:
            rows = ("- [x] u-1 abcdef1 — depends []\n"
                    "- [x] REVIEW-audit-q abcdef2 — depends [u-1] — the author's own\n")
            paths, q = self.queue(tmp, "audit_every = 2\n", rows)
            self.assertFalse(ralph.closing_audit_due(paths, q))
        with tempfile.TemporaryDirectory() as tmp:          # not finished, or nothing in it
            paths, q = self.queue(tmp, "audit_every = 2\n", self.TWO_DONE)
            self.assertFalse(ralph.closing_audit_due(paths, q))
            write(tmp, "ralph/next/q/STATE.md", "# no rows yet\n")
            self.assertFalse(ralph.closing_audit_due(paths, ralph.Queue(q.path)))

    def test_the_id_takes_the_addendums_prefix_var_and_counts_its_own_kind(self):
        with tempfile.TemporaryDirectory() as tmp:
            rows = "- [x] REVIEW-audit-zz-auto-1 abcdef0 — depends []\n" + self.TWO_DONE
            paths, q = self.queue(tmp, "audit_every = 2\n", rows, files=(
                ("ralph/PROMPT.base.md", "<!-- section: main -->\nwork on {{prefix}}\n"),
                ("ralph/next/q/PROMPT.addendum.md", "<!-- section: vars -->\nprefix = zz\n")))
            (row, _), _ = said(ralph.insert_audit, paths, q, q.by_id()["u-3"])
            self.assertEqual(row.id, "REVIEW-audit-zz-auto-2")

    def test_a_prefix_var_that_cannot_be_an_id_falls_back_to_the_queue_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths, q = self.queue(tmp, "audit_every = 2\n", self.TWO_DONE, files=(
                ("ralph/PROMPT.base.md", "<!-- section: main -->\nwork on {{prefix}}\n"),
                ("ralph/next/q/PROMPT.addendum.md", "<!-- section: vars -->\nprefix = e7 or rb\n")))
            (row, _), out = said(ralph.insert_audit, paths, q, q.by_id()["u-3"])
            self.assertEqual(row.id, "REVIEW-audit-q-auto-1")
            self.assertIn("'e7 or rb' cannot be part of a row id", out)

    def test_a_refused_commit_returns_gits_words(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths, q = self.queue(tmp, "audit_every = 2\n", self.TWO_DONE)
            refusal = subprocess.CompletedProcess([], 1, "", "index.lock exists\n")
            with mock.patch.object(ralph, "commit_state", return_value=refusal):
                (row, refused), _ = said(ralph.insert_audit, paths, q, q.by_id()["u-3"])
            self.assertEqual(refused, "index.lock exists")
            self.assertIn(row.id, q.path.read_text())


class DispatchRequiresTests(unittest.TestCase):
    """`dispatch_requires` (phase-b-29: census before code), read by dispatch_refusal."""

    def queue(self, tmp, rows, requires):
        write(tmp, "ralph/next/q/queue.toml", f"dispatch_requires = {requires!r}\n")
        state = write(tmp, "ralph/next/q/STATE.md", rows)
        return queue_paths(tmp, "q"), ralph.Queue(state)

    def test_a_work_row_without_its_census_is_refused_by_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            rows = ("- [ ] pb-a — depends [] — OUTCOME: x\n  - finish: edge a\n"
                    "- [ ] pb-b — depends [] — OUTCOME: y\n  - trial: COMPILE ok\n")
            paths, q = self.queue(tmp, rows, ["trial:"])
            refusal = ralph.dispatch_refusal(paths, q, q.by_id()["pb-a"])
            self.assertIn("pb-a", refusal)
            self.assertIn("'trial:'", refusal)
            self.assertIn("ralph/next/q/queue.toml", refusal)
            self.assertIsNone(ralph.dispatch_refusal(paths, q, q.by_id()["pb-b"]))

    def test_a_row_carrying_its_markers_dispatches_and_the_next_rows_do_not_lend_theirs(self):
        with tempfile.TemporaryDirectory() as tmp:
            rows = ("- [ ] pb-a — depends [] — OUTCOME: x\n  - trial: COMPILE ok\n"
                    "- [ ] pb-b — depends [] — OUTCOME: y\n")
            paths, q = self.queue(tmp, rows, ["trial:"])
            self.assertIsNone(ralph.dispatch_refusal(paths, q, q.by_id()["pb-a"]))
            self.assertEqual(q.unmet_requirements(q.by_id()["pb-b"], ("trial:",)), ["trial:"])
            self.assertIsNotNone(ralph.dispatch_refusal(paths, q, q.by_id()["pb-b"]))

    def test_reviews_and_human_rows_are_exempt_from_dispatch_requires(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths, q = self.queue(tmp, "- [ ] REVIEW-x — depends []\n"
                                       "- [ ] HUMAN-y — depends []\n", ["trial:"])
            for rid in ("REVIEW-x", "HUMAN-y"):
                self.assertEqual(q.unmet_requirements(q.by_id()[rid], ("trial:",)), [])
                self.assertIsNone(ralph.dispatch_refusal(paths, q, q.by_id()[rid]))

    def test_a_legacy_line_requires_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            q = ralph.Queue(write(tmp, "ralph/STATE.md", "- [ ] pb-a — depends []\n"))
            self.assertIsNone(ralph.dispatch_refusal(ralph.Paths(pathlib.Path(tmp)), q,
                                                     q.by_id()["pb-a"]))


# -- manifests, models, prompts ----------------------------------------------


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

    def test_dispatch_requires_loads_as_a_tuple_and_refuses_a_bad_type(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/next/a/queue.toml", "dispatch_requires = ['trial:', 'finish:']\n")
            self.assertEqual(ralph.load_manifest(tmp, "a").dispatch_requires,
                             ("trial:", "finish:"))
            for bad in ("dispatch_requires = 'trial:'\n", "dispatch_requires = ['']\n"):
                write(tmp, "ralph/next/b/queue.toml", bad)
                with self.assertRaisesRegex(ValueError, "dispatch_requires"):
                    ralph.load_manifest(tmp, "b")

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

    def test_roster_values_stay_raw_and_parse_in_declared_order(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = write(tmp, "ralph/models.env", "MODEL=a/1, b/2,,\nREVIEW_MODEL=r/1\n")
            m = ralph.load_models(p)
            self.assertEqual(m, {"MODEL": "a/1, b/2,,", "REVIEW_MODEL": "r/1"})
            self.assertEqual(ralph.parse_roster(m["MODEL"]), ["a/1", "b/2"])
            self.assertEqual(ralph.parse_roster(""), [])
            self.assertEqual(ralph.parse_roster(None), [])


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
            args, models, said_ = self.resolved(
                tmp, ["supervise", "--workdir", tmp, "--queue", "a", "--model", "f/worker",
                      "--review-model", "f/review", "--session-timeout", "60"])
            self.assertEqual(models, {"MODEL": "m/worker", "REVIEW_MODEL": "f/review",
                                      "RESOLVE_MODEL": "e/resolve", "VARIANT": "high"})
            self.assertEqual(args.session_timeout, 7200)
            self.assertIn("ignoring --model f/worker", said_)
            self.assertIn("ignoring --session-timeout 60", said_)

    def test_a_legacy_line_is_flag_then_models_env(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "ralph/models.env", "MODEL=e/worker\nREVIEW_MODEL=e/review\n")
            args, models, said_ = self.resolved(
                tmp, ["run", "--workdir", tmp, "--model", "f/worker", "--session-timeout", "7200"])
            self.assertEqual(models, {"MODEL": "f/worker", "REVIEW_MODEL": "e/review",
                                      "RESOLVE_MODEL": "", "VARIANT": ""})
            self.assertEqual((args.session_timeout, said_), (7200, ""))

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


# The legacy queues' hand-made PROMPT.md files were factored from the base as
# of 865cdc92b. five-programs-65 (1cc169511) moved the base on purpose, and
# those files are their queues' to change, so the factoring proofs render
# against the base they were factored from. Top level programs (#68, 6c51bba85)
# rewrote the repo paths inside those files, so the base carries the same
# renames; nothing else in it moved.
FACTORED_BASE = "865cdc92b"
LAYOUT_MOVE_RENAMES = (("sovereign/ARCH_PRINCIPLES.md", "docs/ARCH_PRINCIPLES.md"),
                       ("sovereign/SYSTEM_OVERVIEW.md", "docs/SYSTEM_OVERVIEW.md"),
                       ("sovereign/apps/", "cmnwlth/apps/"))


def factored_base():
    base = subprocess.run(["git", "-C", str(REPO), "show",
                           f"{FACTORED_BASE}:ralph/PROMPT.base.md"],
                          capture_output=True, text=True, check=True).stdout
    for old, new in LAYOUT_MOVE_RENAMES:
        base = base.replace(old, new)
    return base


class PromptRenderTests(unittest.TestCase):
    BASE = ("not rendered\n<!-- section: intro -->\n# {{queue}}\n"
            "<!-- section: checks -->\n| LINT |\n<!-- section: rules -->\n- mark {{state}}\n")
    # prompt_text's builtins: the four the loop always filled, and `result`.
    BUILTINS = {"queue": "a", "state": "ralph/next/a/STATE.md",
                "control_dir": "ralph/next/a/ctl", "log_dir": "target/ralph/a",
                "result": "ralph-result"}

    def test_ring_room_renders_byte_for_byte(self):
        # Nothing was lost in the factoring: base + ring-room's addendum, with the
        # legacy control files, IS the PROMPT.md its launch line still reads.
        rendered = ralph.render_prompt(
            factored_base(),
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
                               ("<!-- section: vars -->\nresult = mine\n", "result"),
                               ("<!-- section: ghost append -->\nx\n", "ghost"),
                               ("<!-- section: w after=ghost -->\nx\n", "ghost"),
                               ("<!-- section: rules -->\na\n<!-- section: rules -->\nb\n",
                                "rules")):
            with self.assertRaisesRegex(ValueError, word):
                ralph.render_prompt(self.BASE, addendum, self.BUILTINS)

    def test_a_queue_with_an_addendum_runs_on_the_render(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            write(tmp, "ralph/PROMPT.base.md", self.BASE + "<!-- section: end -->\n{{result}}\n")
            write(tmp, "ralph/next/a/PROMPT.addendum.md", "<!-- section: rules -->\n- a only\n")
            text, source = ralph.prompt_text(queue_paths(tmp, "a"))
            self.assertEqual(text, "# a\n| LINT |\n- a only\nralph-result\n")
            self.assertEqual(source, "ralph/PROMPT.base.md + ralph/next/a/PROMPT.addendum.md")
            rc, out, _ = quiet_main(["prompt", "--workdir", tmp, "--queue", "a"])
            self.assertEqual((rc, out), (0, text))
            self.assertEqual(ralph.prompt_text(queue_paths(tmp, "b")),
                             ("prompt of b\n", "ralph/next/b/PROMPT.md"))

    def test_a_declared_prompt_is_read_as_written_even_beside_an_addendum(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            write(tmp, "ralph/PROMPT.base.md", self.BASE)
            write(tmp, "ralph/next/a/PROMPT.addendum.md", "<!-- section: rules -->\n- a only\n")
            write(tmp, "ralph/next/a/queue.toml", 'prompt = "ralph/next/a/PROMPT.md"\n')
            self.assertEqual(ralph.prompt_text(queue_paths(tmp, "a"))[0], "prompt of a\n")


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
        rendered = set(ralph.render_prompt(
            factored_base(), (REPO / paths.prompt_addendum).read_text(),
            {"queue": paths.queue, "state": paths.state, "control_dir": "ralph",
             "log_dir": "target/ralph"}).splitlines())
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


# -- paths, session env, reports ----------------------------------------------


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
            pa, pb = (queue_paths(tmp, q) for q in ("a", "b"))
            ctl = "ralph/next/a/ctl"
            self.assertEqual(
                (pa.control_dir, pa.done, pa.stop, pa.needs_human, pa.waiting, pa.parked,
                 pa.heartbeat, pa.director_commits, pa.log_dir),
                (ctl, f"{ctl}/DONE", f"{ctl}/STOP", f"{ctl}/NEEDS_HUMAN.md", f"{ctl}/waiting",
                 f"{ctl}/parked", f"{ctl}/.heartbeat", f"{ctl}/.director-commits",
                 "target/ralph/a"))
            self.assertEqual((pb.stop, pb.log_dir), ("var/b/STOP", "target/ralph/b"))
            # The loop's log dir too: a session log written after the dispatch
            # snapshot would otherwise read as the session's untracked file.
            self.assertEqual(ralph.runtime_markers(pa), (f"{ctl}/", "target/ralph/a/"))
            legacy = ralph.runtime_markers(ralph.Paths(pathlib.Path(tmp)))
            self.assertEqual(legacy[-1], "target/ralph/")
            self.assertIn("ralph/parked/", legacy)

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

    def test_the_legacy_pool_keeps_its_defaults(self):
        args = ralph.build_parser().parse_args(["pool", "--workdir", "."])
        with contextlib.redirect_stdout(io.StringIO()):
            paths = ralph.paths_for(args)
        self.assertEqual((paths.state, paths.prompt, paths.conflicts, paths.heavy, paths.stop),
                         ("ralph/STATE.md", "ralph/PROMPT.md", "ralph/conflicts.txt",
                          "ralph/heavy.txt", "ralph/STOP"))
        self.assertEqual((args.label, args.session_timeout), ("campaign", 3600))

    def test_run_flags_reach_paths(self):
        import argparse
        with tempfile.TemporaryDirectory() as d:
            a = argparse.Namespace(workdir=d, prompt="ralph/next/x/PROMPT.md", state="ralph/next/x/STATE.md")
            p = ralph.paths_for(a)
            self.assertEqual(p.state, "ralph/next/x/STATE.md")
            self.assertEqual(p.prompt, "ralph/next/x/PROMPT.md")

    def test_absent_flags_keep_the_defaults(self):
        import argparse
        with tempfile.TemporaryDirectory() as d:
            p = ralph.paths_for(argparse.Namespace(workdir=d))
            self.assertEqual(p.state, "ralph/STATE.md")
            self.assertEqual(p.prompt, "ralph/PROMPT.md")


class SessionEnvTests(unittest.TestCase):
    """What every session is told about its loop. Loop._start_session lays
    session_env over os.environ, so a key present here, even empty, replaces
    whatever a parent loop leaked into the environment."""

    def test_a_queue_session_is_told_its_queue_state_and_control_dir(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = str(pathlib.Path(tmp).resolve())
            git_repo(tmp)
            stub_worker(tmp, "exit 0\n")
            write(tmp, "ralph/next/a/queue.toml",
                  'worker_bin = "./stub-worker.sh"\nsettings = "ralph/next/a/settings.json"\n')
            paths = queue_paths(tmp, "a")
            self.assertEqual(ralph.session_env(paths), {
                "RALPH_QUEUE": "a", "RALPH_STATE": "ralph/next/a/STATE.md",
                "RALPH_CONTROL_DIR": "ralph/next/a/ctl", "RALPH_WORKDIR": tmp,
                "RALPH_CLAUDE_SETTINGS": f"{tmp}/ralph/next/a/settings.json"})
            with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": "/nonexistent/worker"}):
                self.assertEqual(ralph.worker_bin(paths), f"{tmp}/stub-worker.sh")

    def test_a_legacy_session_is_told_its_state_too(self):
        with tempfile.TemporaryDirectory() as tmp:
            git_repo(tmp)
            stub = stub_worker(tmp, "exit 0\n")
            paths = ralph.paths_for(ralph.build_parser().parse_args(
                ["run", "--workdir", tmp, "--state", "ralph/next/ring-room/STATE.md"]))
            env = {"RALPH_QUEUE": "leaked-from-a-parent-loop", **ralph.session_env(paths)}
            self.assertEqual([env[k] for k in ("RALPH_QUEUE", "RALPH_STATE", "RALPH_CONTROL_DIR",
                                               "RALPH_CLAUDE_SETTINGS")],
                             ["", "ralph/next/ring-room/STATE.md", "ralph", ""])
            with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": str(stub)}):
                self.assertEqual(ralph.worker_bin(paths), str(stub))


class ExcludeTests(unittest.TestCase):
    def test_a_linked_worktree_writes_the_common_exclude(self):
        # The failing input: a linked worktree, whose `.git` is a file. On
        # 2026-09-30 supervise died there on mkdir(<worktree>/.git/info).
        with tempfile.TemporaryDirectory() as tmp:
            main, side = os.path.join(tmp, "main"), os.path.join(tmp, "side")
            git_repo(main)
            write(main, "a.txt", "a\n")
            commit_all(main)
            subprocess.run(["git", "-C", main, "worktree", "add", "-q", "--detach", side],
                           check=True)
            ralph.ensure_excludes(side, ("ralph/STOP",))
            ralph.ensure_excludes(main, ("ralph/DONE",))
            lines = pathlib.Path(main, ".git", "info", "exclude").read_text().splitlines()
            self.assertIn("ralph/STOP", lines)
            self.assertIn("ralph/DONE", lines)
            self.assertTrue(pathlib.Path(side, ".git").is_file())

    def test_outside_a_repository_it_writes_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            ralph.ensure_excludes(tmp, ("ralph/STOP",))
            self.assertFalse(pathlib.Path(tmp, ".git").exists())


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


STALE = ralph.STALL_SECS + 60


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


class TwoQueueControlTests(unittest.TestCase):
    """Two loops in one checkout share no control file."""

    def test_stop_report_and_watch_read_the_queues_markers(self):
        with tempfile.TemporaryDirectory() as tmp:
            two_queues(tmp)
            write(tmp, "ralph/next/a/ctl/NEEDS_HUMAN.md", "# a is blocked\n")
            with mock.patch.object(ralph, "job_running", return_value=False):
                rc, _, _ = quiet_main(["stop", "--workdir", tmp, "--queue", "b", "--timeout", "1"])
            self.assertEqual(rc, 0)
            self.assertTrue((pathlib.Path(tmp) / "ralph/next/b/ctl/STOP").exists())
            self.assertFalse((pathlib.Path(tmp) / "ralph/STOP").exists())
            _, out, _ = quiet_main(["report", "--workdir", tmp, "--queue", "a"])
            self.assertIn("markers: NEEDS_HUMAN.md", out)
            _, out, _ = quiet_main(["report", "--workdir", tmp, "--queue", "b"])
            self.assertIn("markers: STOP", out)
            pa, pb = (queue_paths(tmp, q, "watch") for q in ("a", "b"))
            self.assertEqual(ralph.Watch(pa, label="a", running=lambda: False).condition()[0],
                             "down")
            self.assertIsNone(ralph.Watch(pb, label="b", running=lambda: False).condition())


# -- the watchdog -------------------------------------------------------------


class WatchTests(unittest.TestCase):
    def make(self, tmp, *, running=True, stall_secs=300):
        (pathlib.Path(tmp) / "ralph").mkdir(exist_ok=True)
        notes = []
        paths = ralph.Paths(pathlib.Path(tmp))
        w = ralph.Watch(paths, label="t", running=lambda: running, stall_secs=stall_secs,
                        notifier=lambda title, body, enabled: notes.append((title, body)))
        return w, notes

    def test_down_unless_done_or_an_operator_stop(self):
        with tempfile.TemporaryDirectory() as tmp:
            w, _ = self.make(tmp, running=False)
            self.assertEqual(w.condition()[0], "down")
            done = write(tmp, "ralph/DONE", "")
            self.assertIsNone(w.condition())
            done.unlink()
            stop = write(tmp, "ralph/STOP", "")
            self.assertIsNone(w.condition())
            # A drain stop is rewritten empty once drained: a loop down while it
            # still reads `drain` died mid-drain.
            stop.write_text("drain\n")
            self.assertEqual(w.condition()[0], "down")

    def test_stalled_is_a_heartbeat_older_than_stall_secs(self):
        with tempfile.TemporaryDirectory() as tmp:
            w, _ = self.make(tmp)
            self.assertIsNone(w.condition())               # running, no beat yet
            hb = write(tmp, "ralph/.heartbeat", "1 x\n")
            self.assertIsNone(w.condition())
            old = time.time() - 600
            os.utime(hb, (old, old))
            self.assertEqual(w.condition()[0], "stalled")

    def test_a_condition_is_said_once_until_it_changes_or_clears(self):
        with tempfile.TemporaryDirectory() as tmp:
            w, notes = self.make(tmp, running=False)
            state = pathlib.Path(tmp) / "watch.state"
            self.assertEqual(w.run(state), "down")
            w.run(state)
            self.assertEqual(notes, [("OPERATOR — loop down", notes[0][1])])
            w.run(state, nag_secs=0)                       # the nag interval re-says it
            self.assertEqual(len(notes), 2)
            w.running = lambda: True
            hb = write(tmp, "ralph/.heartbeat", "1 x\n")
            old = time.time() - 600
            os.utime(hb, (old, old))
            self.assertEqual(w.run(state), "stalled")
            self.assertEqual(notes[-1][0], "OPERATOR — loop hung")
            os.utime(hb, None)
            self.assertIsNone(w.run(state))
            self.assertEqual(state.read_text(), "")
            self.assertEqual(len(notes), 3)


# -- the host -----------------------------------------------------------------


class HostTests(unittest.TestCase):
    """Backend SELECTION and the absent-backend path, with the platform and the
    `which` lookup injected. The Linux backends are not exercised on a Linux host here."""

    ARGV = ["python3", "ralph.py", "run", "--queue", "a"]

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
                delivered, said_ = self.said(lambda: host.notify("OPERATOR — loop down", "b is down"))
                self.assertFalse(delivered)
                self.assertEqual(calls, [])
                self.assertIn(tool, said_)
                self.assertIn("OPERATOR — loop down: b is down", said_)

    def test_linux_installs_and_starts_a_systemd_run_job(self):
        with tempfile.TemporaryDirectory() as home:
            host, calls = self.host("linux", ("systemd-run", "systemctl"), home)
            spec = host.install_job("dev.ralph.x-a", self.ARGV, "/w", "/w/log.txt")
            self.assertTrue(spec.is_file())
            host.start_job("dev.ralph.x-a")
            start = calls[-1]
            self.assertEqual(start[:4], ["systemd-run", "--user", "--unit", "dev.ralph.x-a"])
            self.assertIn("--working-directory=/w", start)
            self.assertEqual(start[-5:], self.ARGV)
            host.install_job("dev.ralphwatch.x-a", ["python3", "ralph.py", "watch"], "/w",
                             "/w/watch.log", interval=120)
            host.start_job("dev.ralphwatch.x-a")
            self.assertIn("--on-unit-active=120", calls[-1])

    def test_linux_keep_alive_restarts_on_failure_and_only_then(self):
        # The supervisor is retired: the host restarts a loop that crashed, and a
        # loop that exits 0 (done or stopped) stays down.
        with tempfile.TemporaryDirectory() as home:
            host, calls = self.host("linux", ("systemd-run", "systemctl"), home)
            spec = host.install_job("dev.ralph.x-a", self.ARGV, "/w", "/w/log.txt",
                                    keep_alive=True)
            self.assertIs(json.loads(spec.read_text())["keep_alive"], True)
            host.start_job("dev.ralph.x-a")
            start = calls[-1]
            at = start.index("Restart=on-failure")
            self.assertEqual((start[at - 1], start[at + 1:at + 3]), ("-p", ["-p", "RestartSec=60"]))
            self.assertEqual(start[-5:], self.ARGV)       # the policy sits before the command
            host.install_job("dev.ralph.x-b", self.ARGV, "/w", "/w/log.txt")
            host.start_job("dev.ralph.x-b")
            self.assertIs(json.loads(host.job_file("dev.ralph.x-b").read_text())["keep_alive"],
                          False)
            self.assertNotIn("Restart=on-failure", calls[-1])

    def test_linux_a_job_spec_written_before_keep_alive_starts_without_it(self):
        with tempfile.TemporaryDirectory() as home:
            host, calls = self.host("linux", ("systemd-run", "systemctl"), home)
            spec = host.job_file("dev.ralph.x-a")
            spec.parent.mkdir(parents=True)
            spec.write_text(json.dumps({"argv": self.ARGV, "workdir": "/w", "log": "/w/log.txt",
                                        "interval": None}))
            host.start_job("dev.ralph.x-a")
            self.assertEqual(calls[-1][-5:], self.ARGV)
            self.assertNotIn("Restart=on-failure", calls[-1])

    def test_mac_keeps_its_job_out_of_launch_agents_and_starts_it_by_path(self):
        # launchd re-runs every plist in ~/Library/LaunchAgents at each login, so a
        # job written there outlived its queue: on 2026-10-05 the finished svrngs
        # e0-u3 loops were still loaded at boot. Like Linux's transient unit, the
        # Mac job now runs until it exits or the login session ends.
        with tempfile.TemporaryDirectory() as home:
            host, calls = self.host("darwin", ("launchctl",), home)
            plist = host.install_job("dev.ralph.x-a", self.ARGV, "/w", "/w/log.txt")
            self.assertEqual(plist, pathlib.Path(home) / ".config" / "ralph" / "jobs" / "dev.ralph.x-a.plist")
            self.assertFalse((pathlib.Path(home) / "Library" / "LaunchAgents").exists())
            self.assertEqual(plistlib.loads(plist.read_bytes())["ProgramArguments"], self.ARGV)
            host.start_job("dev.ralph.x-a")
            self.assertEqual(calls[-1], ["launchctl", "bootstrap", f"gui/{os.getuid()}", str(plist)])

    def test_mac_keep_alive_restarts_on_a_failed_exit_throttled(self):
        with tempfile.TemporaryDirectory() as home:
            host, _ = self.host("darwin", ("launchctl",), home)
            job = plistlib.loads(host.install_job("dev.ralph.x-a", self.ARGV, "/w", "/w/log.txt",
                                                  keep_alive=True).read_bytes())
            self.assertEqual(job["KeepAlive"], {"SuccessfulExit": False})
            self.assertEqual(job["ThrottleInterval"], 60)
            self.assertIs(job["RunAtLoad"], True)
            self.assertEqual(job["ProgramArguments"], self.ARGV)
            plain = plistlib.loads(host.install_job("dev.ralph.x-b", self.ARGV, "/w",
                                                    "/w/log.txt").read_bytes())
            self.assertNotIn("KeepAlive", plain)
            self.assertNotIn("ThrottleInterval", plain)

    def test_the_module_install_job_passes_keep_alive_through(self):
        with tempfile.TemporaryDirectory() as home:
            host, _ = self.host("darwin", ("launchctl",), home)
            with mock.patch.object(ralph, "host", return_value=host):
                plist = ralph.install_job("dev.ralph.x-a", self.ARGV, "/w", "/w/log.txt",
                                          keep_alive=True)
            self.assertIn("KeepAlive", plistlib.loads(plist.read_bytes()))

    def test_mac_moves_the_login_agents_ralph_wrote_out_of_launch_agents(self):
        # Every plist ralph wrote before that is still a login agent. The first job
        # call moves each into the jobs dir: a loaded job keeps running, `start`
        # still finds it, no login runs it again, and nothing else is touched.
        with tempfile.TemporaryDirectory() as home:
            agents = pathlib.Path(home) / "Library" / "LaunchAgents"
            jobs = pathlib.Path(home) / ".config" / "ralph" / "jobs"
            agents.mkdir(parents=True)
            jobs.mkdir(parents=True)
            for name in ("dev.ralph.x-a", "dev.ralphwatch.x-a", "com.svrnmesh.daemon"):
                (agents / f"{name}.plist").write_text(name)
            (jobs / "dev.ralph.x-b.plist").write_text("the job")
            (agents / "dev.ralph.x-b.plist").write_text("a stale copy")
            host, _ = self.host("darwin", ("launchctl",), home)
            _, said_ = self.said(lambda: host.job_running("dev.ralph.x-a"))
            self.assertEqual([p.name for p in agents.iterdir()], ["com.svrnmesh.daemon.plist"])
            self.assertEqual((jobs / "dev.ralph.x-a.plist").read_text(), "dev.ralph.x-a")
            self.assertEqual((jobs / "dev.ralphwatch.x-a.plist").read_text(), "dev.ralphwatch.x-a")
            self.assertEqual((jobs / "dev.ralph.x-b.plist").read_text(), "the job")
            self.assertIn("dev.ralphwatch.x-a", said_)
            _, said_ = self.said(lambda: host.job_running("dev.ralph.x-a"))
            self.assertEqual(said_, "")

    def test_start_finds_a_job_that_was_installed_as_a_login_agent(self):
        with tempfile.TemporaryDirectory() as tmp:
            home, workdir = pathlib.Path(tmp) / "home", pathlib.Path(tmp) / "w"
            workdir.mkdir()
            (home / "Library" / "LaunchAgents").mkdir(parents=True)
            (home / "Library" / "LaunchAgents" / "dev.ralph.w-a.plist").write_text("<plist/>")
            host, calls = self.host("darwin", ("launchctl",), home)
            with mock.patch.object(ralph, "host", return_value=host):
                rc, _, err = quiet_main(["start", "--workdir", str(workdir), "--label", "a"])
            self.assertEqual(rc, 0, err)
            self.assertEqual(calls[-1], ["launchctl", "bootstrap", f"gui/{os.getuid()}",
                                         str(home / ".config" / "ralph" / "jobs" / "dev.ralph.w-a.plist")])

    def test_start_leaves_a_running_job_alone(self):
        # Starting a running job booted it out, then failed to bootstrap it:
        # the loop and its session died mid-unit (svrngs u5, 2026-10-06T17:31Z).
        with tempfile.TemporaryDirectory() as tmp:
            home, workdir = pathlib.Path(tmp) / "home", pathlib.Path(tmp) / "w"
            workdir.mkdir()
            jobs = home / ".config" / "ralph" / "jobs"
            jobs.mkdir(parents=True)
            (jobs / "dev.ralph.w-a.plist").write_text("<plist/>")
            host, calls = self.host("darwin", ("launchctl",), home)
            with mock.patch.object(ralph, "host", return_value=host), \
                    mock.patch.object(host, "job_running", return_value=True):
                rc, out, err = quiet_main(["start", "--workdir", str(workdir), "--label", "a"])
            self.assertEqual(rc, 0, err)
            self.assertEqual(calls, [])
            self.assertIn("already running", out + err)

    def test_an_absent_job_backend_refuses_to_start_and_says_it_cannot_see(self):
        with tempfile.TemporaryDirectory() as home:
            for platform, tool in (("linux", "systemd-run"), ("darwin", "launchctl"),
                                   ("win32", "no job backend")):
                host, calls = self.host(platform, (), home)
                with self.assertRaisesRegex(ralph.HostError, tool):
                    host.start_job("dev.ralph.x-a")
                running, said_ = self.said(lambda: host.job_running("dev.ralph.x-a"))
                self.assertFalse(running)
                self.assertIn("cannot tell whether dev.ralph.x-a is running", said_)
                self.assertEqual(calls, [])

    def test_start_reports_an_absent_backend_as_exit_two(self):
        with tempfile.TemporaryDirectory() as tmp:
            host, _ = self.host("linux", (), tmp)
            with mock.patch.object(ralph, "host", return_value=host):
                rc, _, err = quiet_main(["start", "--workdir", tmp])
            self.assertEqual(rc, 2)
            self.assertIn("start:", err)


# -- the probe and the error tail ---------------------------------------------


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

    def test_a_declared_worker_bin_probes_a_bare_id_through_itself(self):
        # The claude shim names models bare; the pool's dispatch probe refused
        # `claude-opus-5-5` and halted phase-c before its first wave.
        with tempfile.TemporaryDirectory() as tmp:
            paths = ralph.Paths(pathlib.Path(tmp),
                                manifest=mock.Mock(worker_bin=shutil.which("echo")))
            self.assertEqual(ralph.probe_model("claude-opus-5-5", paths), (True, ""))
            paths.manifest.worker_bin = shutil.which("false")
            ok, cause = ralph.probe_model("claude-opus-5-5", paths)
            self.assertFalse(ok)
            self.assertIn("exit 1", cause)

    def test_a_refused_model_never_reaches_the_client(self):
        # From RosterProbeTests: a refusal is the cause a halt names, and no
        # session (here, the client writing a file) is spent on it.
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, ".opencode/opencode.json",
                  '{"provider": {"mesh": {"options": {"baseURL": "http://127.0.0.1:9741/v1"}}}}')
            ran = pathlib.Path(tmp) / "ran"
            client = write(tmp, "client.sh", f'#!/bin/sh\ntouch "{ran}"\n')
            client.chmod(0o755)
            paths = ralph.Paths(pathlib.Path(tmp))
            with mock.patch.dict(os.environ, {"RALPH_OPENCODE_BIN": str(client)}):
                self.assertIn("never targets localhost", ralph.probe_refusal("mesh/fast", paths))
                self.assertEqual(ralph.probe_refusal("prov/x", paths), "")
                ok, cause = ralph.probe_model("mesh/fast", paths)
            self.assertFalse(ok)
            self.assertTrue(cause.startswith("mesh/fast: routes at http://127.0.0.1:9741/v1"))
            self.assertFalse(ran.exists())

    def test_a_roster_is_tried_in_declared_order_and_a_single_value_is_a_roster_of_one(self):
        # "Single-model values behave exactly as today (the probe still runs)."
        self.assertEqual(ralph.parse_roster("prov/dead,prov/alive"), ["prov/dead", "prov/alive"])
        self.assertEqual(ralph.parse_roster("prov/solo"), ["prov/solo"])


class ErrorTailTests(unittest.TestCase):
    """A strikeout halt carries the failing session's last error-shaped
    transcript line (quota / permission path / provider error / crash tail),
    so the halt text alone is diagnostic (from HaltTailTests)."""

    LANE = ("working...\n"
            "provider: Usage limit reached for 5 hour\n"
            "Error: Unexpected server error (500)\n")

    def test_the_last_error_shaped_line_is_the_tail(self):
        self.assertEqual(ralph.error_tail(self.LANE), "Error: Unexpected server error (500)")
        self.assertEqual(ralph.error_tail("working...\nall good\n"), "")
        self.assertEqual(ralph.error_tail(None), "")
        self.assertEqual(len(ralph.error_tail("fatal: " + "x" * 400)), 200)

    def test_a_halt_carries_the_transcripts_last_error_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            lane = write(tmp, "target/ralph/lane-dm-a.out", self.LANE)
            review = write(tmp, "target/ralph/review.out",
                           "analyzing...\napi error: 429 quota exhausted\n")
            quiet = write(tmp, "target/ralph/quiet.out", "working...\ndone\n")
            self.assertEqual(ralph.halt_tail_suffix(lane),
                             " — last error: Error: Unexpected server error (500)")
            self.assertEqual(ralph.halt_tail_suffix(review),
                             " — last error: api error: 429 quota exhausted")
            self.assertEqual(ralph.halt_tail_suffix(quiet), "")
            self.assertEqual(ralph.halt_tail_suffix(pathlib.Path(tmp) / "missing.out"), "")


class CargoJobsShareTests(unittest.TestCase):
    def share(self, lanes, avail_gb, cores=32):
        """lib/cargo-jobs.sh's split with the machine's probes stubbed."""
        r = subprocess.run(
            ["bash", "-c", 'source "$0"; stub_gb="$2" stub_cores="$3"; '
             'cargo_jobs_available_gb() { echo "$stub_gb"; }; '
             'cargo_jobs_cores() { echo "$stub_cores"; }; cargo_jobs_share "$1"; '
             'echo "$CARGO_JOBS_SHARE"', str(ralph.CARGO_JOBS_LIB), str(lanes), str(avail_gb),
             str(cores)], capture_output=True, text=True, check=True)
        return int(r.stdout.split()[-1])

    def test_one_budget_is_split_across_the_lanes_with_a_floor(self):
        # (2) and (4): 4 GB a job, 2 jobs a lane at least, half the cores at most.
        self.assertEqual(self.share(3, 41), 3)     # 10 jobs by memory, three lanes
        self.assertEqual(self.share(3, 100), 5)    # 16 by the cores, three lanes
        self.assertEqual(self.share(3, 24), 2)     # the floor exactly
        self.assertEqual(self.share(3, 23), 0)     # under it: no wave
        self.assertEqual(self.share(1, 8), 2)
        self.assertEqual(self.share(2, 64, cores=4), 2)   # core-capped, never under the floor


# -- the lanes ----------------------------------------------------------------


class LaneRootTests(unittest.TestCase):
    def test_lanes_live_beside_the_main_tree_not_under_it(self):
        # Under the main tree cargo merges the main tree's .cargo/config.toml
        # into every lane's, doubling target.rustflags, and every crates.io
        # unit's identity changes with it.
        with tempfile.TemporaryDirectory() as tmp:
            workdir = pathlib.Path(tmp) / "repo"
            workdir.mkdir()
            root = ralph.lane_root_for(workdir)
            self.assertEqual(root, workdir.resolve().parent / "repo-lanes")
            self.assertNotIn(workdir.resolve(), root.parents)


class LaneFixture:
    def repo(self, tmp, rows="- [ ] dm-a — depends []\n"):
        root = pathlib.Path(tmp).resolve()
        git_repo(root)
        write(root, "ralph/STATE.md", rows)
        write(root, "seed.txt", "seed")
        # The lane root is inside the temp tree: never committed, nor is a target/.
        write(root, ".git/info/exclude", ".ralph/\ntarget/\n")
        commit_all(root)
        return root

    def lanes(self, root, *, base="main", free=lambda: 80, floor=ralph.DISK_FLOOR_GB, **paths):
        p = ralph.Paths(root, **paths)
        return ralph.Lanes(p, lane_root=in_tree_lanes(p), base_branch=base,
                           disk_free_gb=free, disk_floor_gb=floor)

    def prepare(self, lanes, unit, jobs=None):
        (wt, notes, env), out = said(lanes.prepare, unit, jobs)
        return wt, notes, env, out


class LanesTests(LaneFixture, unittest.TestCase):
    """Lanes: a lane's worktree, its cloned target, the base merged into it,
    its evidence and its removal (formerly Pool methods, PoolTests/PoolQueueTests)."""

    def test_a_new_lane_is_a_worktree_on_its_own_branch_from_the_base(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            lanes = self.lanes(root)
            wt, notes, env, _ = self.prepare(lanes, "dm-a")
            self.assertEqual(wt, root / ".ralph" / "wt" / "dm-a")
            self.assertEqual(git(wt, "rev-parse", "--abbrev-ref", "HEAD").stdout.strip(),
                             "ralph/dm-a")
            self.assertEqual(git(wt, "rev-parse", "HEAD").stdout, git(root, "rev-parse", "HEAD").stdout)
            self.assertEqual(notes, [])
            self.assertEqual(env, {"SVRN_CARGO_LOCK_DIR":
                                   f"/tmp/svrn-cargo-lock.{os.getuid()}.lane-dm-a"})
            self.assertFalse((wt / ralph.LANE_JOBS_FILE).exists())

    def test_a_branch_left_without_its_worktree_is_checked_out_again_with_its_work(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            subprocess.run(["git", "-C", str(root), "branch", "ralph/dm-a"], check=True)
            side = root / ".ralph" / "side"
            subprocess.run(["git", "-C", str(root), "worktree", "add", "-q", str(side),
                            "ralph/dm-a"], check=True)
            write(side, "dm-a.txt", "the lane's work")
            commit_all(side, "dm-a: work")
            subprocess.run(["git", "-C", str(root), "worktree", "remove", str(side)], check=True)
            wt, _, _, _ = self.prepare(self.lanes(root), "dm-a")
            self.assertEqual((wt / "dm-a.txt").read_text(), "the lane's work")

    def test_a_worktree_git_cannot_make_is_a_spawn_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            with self.assertRaisesRegex(ralph.SpawnError, "git worktree add for dm-a: .*ghost"):
                said(self.lanes(root, base="ghost").prepare, "dm-a", None)

    def test_a_lanes_cargo_share_reaches_its_env_and_its_file(self):
        # `toolbox run` forwards no env, so a lane's checks read the share from a file.
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            wt, _, env, _ = self.prepare(self.lanes(root), "dm-a", jobs=4)
            self.assertEqual((env["SOVEREIGN_LINT_JOBS"], env["SOVEREIGN_TEST_JOBS"]), ("4", "4"))
            self.assertEqual((wt / ralph.LANE_JOBS_FILE).read_text(),
                             "SOVEREIGN_LINT_JOBS=4\nSOVEREIGN_TEST_JOBS=4\n")

    def test_resumed_lane_is_refreshed_onto_the_base(self):
        # A lane is kept across sessions, so a fix that lands on the base never
        # reaches it unless it is brought in (dm-daemon-api-edge burned three
        # waves on a stale opencode.json, 2026-09-17).
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            seed = git(root, "rev-parse", "HEAD").stdout.strip()
            # A harness fix lands on the base AFTER the lane worktree exists.
            write(root, "harness.txt", "fixed")
            commit_all(root, "fix")
            wt = root / ".ralph" / "wt" / "dm-a"
            subprocess.run(["git", "-C", str(root), "worktree", "add", "-q", "-b",
                            "ralph/dm-a", str(wt), seed], check=True)
            _, notes, _, out = self.prepare(self.lanes(root), "dm-a")
            self.assertEqual((wt / "harness.txt").read_text(), "fixed")
            self.assertEqual(notes, [])
            self.assertIn("lane dm-a: refreshed onto main", out)

    def test_a_resumed_lane_with_work_of_its_own_merges_the_base_in(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            lanes = self.lanes(root)
            wt, _, _, _ = self.prepare(lanes, "dm-a")
            write(wt, "dm-a.txt", "lane work")
            commit_all(wt, "dm-a: work")
            write(root, "base.txt", "the base moved")
            commit_all(root, "base moved")
            again, notes, _, out = self.prepare(lanes, "dm-a")
            self.assertEqual(again, wt)
            self.assertEqual(((wt / "dm-a.txt").read_text(), (wt / "base.txt").read_text()),
                             ("lane work", "the base moved"))
            self.assertEqual(notes, [])
            self.assertIn("lane dm-a: merged main in", out)

    def test_a_conflict_with_the_base_goes_back_to_the_lane_as_a_merge_in_progress(self):
        # A settle-time conflict halted the whole pool 8 times (2026-09-17 →
        # 10-04); the base had moved under the lane, which is the lane's to
        # resolve — its resume merges the base in and its session resolves it.
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            write(root, "shared.txt", "seed\n")
            commit_all(root, "shared")
            lanes = self.lanes(root)
            wt, _, _, _ = self.prepare(lanes, "dm-a")
            write(wt, "shared.txt", "the lane's\n")
            commit_all(wt, "dm-a: shared")
            write(root, "shared.txt", "the base's\n")
            commit_all(root, "base: shared")
            _, notes, _, out = self.prepare(lanes, "dm-a")
            self.assertIn("lane dm-a: conflicts with main — the session resolves it", out)
            self.assertEqual(len(notes), 1)
            self.assertIn("MERGE IN PROGRESS", notes[0])
            self.assertEqual(git(wt, "rev-parse", "-q", "--verify", "MERGE_HEAD").returncode, 0)

    def test_lane_worktree_gets_host_pointer_dirs(self):
        # A row's `read: O8` names `.sovereign/features/<id>/order.md`, which
        # is gitignored (`.gitignore:44`), so `git worktree add` never brings
        # it and the lane cannot execute its row (dm-vocab-compile-fail-test,
        # 2026-09-17, three waves). The lanes provision the per-host pointers.
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            write(root, ".gitignore", ".sovereign/features/\n")
            write(root, ".sovereign/features/dm-a/order.md", "the order")
            commit_all(root, "pointers")
            wt, _, _, _ = self.prepare(self.lanes(root), "dm-a")
            self.assertEqual((wt / ".sovereign/features/dm-a/order.md").read_text(), "the order")
            # A copy, not a symlink: the ignore pattern is directory-only, so a
            # symlink would be committed by the lane's `git add -A`.
            self.assertFalse((wt / ".sovereign/features").is_symlink())
            self.assertEqual(git(wt, "status", "--porcelain").stdout, "")

    def test_a_new_lane_starts_from_a_clone_of_the_main_target(self):
        # (1): the lane's target holds the main tree's artifacts, its evidence
        # dir is not the main tree's, and every tracked file is newer than
        # every cloned artifact, so cargo rebuilds the workspace crates once.
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            write(root, "target/debug/deps/libserde.rlib", "warm")
            write(root, "target/ralph/q/lint.log", "the main tree's log")
            lanes = self.lanes(root)
            # /tmp is no btrfs, so a plain copy; and the main tree finishes a build
            # between the lane's checkout and its clone (the race the touch closes).
            lanes.CLONE_TARGET = ("sh", "-c", 'sleep 0.05; touch "$0/debug/deps/libserde.rlib"; '
                                  'cp -a "$0" "$1"')
            wt, _, _, out = self.prepare(lanes, "q-a")
            t = wt / "target"
            self.assertEqual((t / "debug/deps/libserde.rlib").read_text(), "warm")
            self.assertFalse((t / "ralph/q/lint.log").exists())
            self.assertGreaterEqual((wt / "seed.txt").stat().st_mtime,
                                    (t / "debug/deps/libserde.rlib").stat().st_mtime)
            self.assertIn("target cloned from", out)

    def test_a_target_that_cannot_be_cloned_is_named(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            write(root, "target/debug/deps/libserde.rlib", "warm")
            lanes = self.lanes(root)
            lanes.CLONE_TARGET = ("false",)
            wt, _, _, out = self.prepare(lanes, "q-a")
            self.assertIn("target NOT cloned", out)
            self.assertFalse((wt / "target").exists())

    @unittest.skipUnless(sys.platform == "darwin", "the APFS clone; Linux's is btrfs-only")
    def test_the_default_clone_runs_on_macos(self):
        # macOS cp has no --reflink: the btrfs default failed "illegal option"
        # and every ersilia lane built from an empty target (2026-10-02). The
        # default itself, not a stand-in, clones a target here.
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            write(root, "target/debug/deps/libserde.rlib", "warm")
            wt, _, _, out = self.prepare(self.lanes(root), "q-a")
            self.assertNotIn("target NOT cloned", out)
            self.assertIn("target cloned from", out)
            self.assertEqual((wt / "target/debug/deps/libserde.rlib").read_text(), "warm")

    def test_a_read_only_dir_in_the_cloned_target_neither_leaks_the_lane_nor_its_evidence(self):
        # The main tree's target/ralph/phase-b/ship/esc/seed is dr-xr-xr-x; the
        # clone copies it into the lane, where it stopped the evidence reset
        # and `git worktree remove --force` alike (2026-10-02).
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp, "- [ ] q-a — depends []\n")
            write(root, "target/debug/ro/lib.rlib", "warm")
            write(root, "target/ralph/phase-b/ship/esc/seed/config.toml", "seed")
            for d in ("target/debug/ro", "target/ralph/phase-b/ship/esc/seed"):
                os.chmod(root / d, 0o555)
            lanes = self.lanes(root, log_dir="target/ralph/q")
            lanes.CLONE_TARGET = ("cp", "-a")
            wt, _, _, _ = self.prepare(lanes, "q-a", jobs=2)
            write(wt, "target/ralph/q/lint.log", "exit=0 the lane's lint\n")
            self.assertTrue(said(lanes.keep_evidence, "q-a", wt)[0])
            said(lanes.remove_lane, "q-a", wt)
            self.assertFalse(wt.exists())
            kept = root / "target/ralph/q/q-a"
            self.assertEqual(sorted(p.relative_to(kept).as_posix() for p in kept.rglob("*")),
                             ["lane.env", "q", "q/lint.log"])

    def test_a_lanes_evidence_outlives_its_worktree(self):
        # (12): `git worktree remove --force` takes the lane's target/ with it.
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp, "- [ ] q-a — depends []\n")
            lanes = self.lanes(root, log_dir="target/ralph/q")
            wt, _, _, _ = self.prepare(lanes, "q-a")
            write(wt, "target/ralph/q/lint.log", "exit=0 the lane's lint\n")
            self.assertTrue(said(lanes.keep_evidence, "q-a", wt)[0])
            _, out = said(lanes.remove_lane, "q-a", wt)
            self.assertFalse(wt.exists())
            self.assertEqual(out, "")                      # git removed it whole
            self.assertEqual((root / "target/ralph/q/q-a/q/lint.log").read_text(),
                             "exit=0 the lane's lint\n")

    def test_evidence_that_cannot_be_copied_says_so_and_a_lane_with_none_is_kept_trivially(self):
        # False tells the caller to keep the worktree rather than lose what a commit cites.
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp, "- [ ] q-a — depends []\n")
            lanes = self.lanes(root, log_dir="target/ralph/q")
            wt, _, _, _ = self.prepare(lanes, "q-a")
            self.assertTrue(said(lanes.keep_evidence, "q-a", wt)[0])     # nothing to keep
            write(wt, "target/ralph/q/lint.log", "exit=0\n")
            write(root, "target/ralph/q/q-a", "a file where the evidence dir goes")
            kept, out = said(lanes.keep_evidence, "q-a", wt)
            self.assertFalse(kept)
            self.assertIn("evidence copy to", out)

    def test_remove_tree_clears_write_protection(self):
        with tempfile.TemporaryDirectory() as tmp:
            ro = write(tmp, "t/a/b/file", "x").parent
            os.chmod(ro, 0o555)
            os.chmod(ro.parent, 0o555)
            self.assertIsNone(ralph.remove_tree(pathlib.Path(tmp) / "t"))
            self.assertFalse((pathlib.Path(tmp) / "t").exists())
            self.assertIsNone(ralph.remove_tree(pathlib.Path(tmp) / "never-was"))


class ReclaimDiskTests(LaneFixture, unittest.TestCase):
    """Under the disk floor the lanes free cargo output from lanes that cannot
    need it now. A floor with no remedy sat the ersilia pool idle for 66 ticks
    while two idle lanes held 15GB of build output (2026-10-04)."""

    def test_idle_lanes_build_output_goes_and_a_busy_lanes_never_does(self):
        # Never a lane whose unit is active or about to start: its session or
        # background run may be executing those binaries. The rest of target/ stays.
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            lane_root = root / ".ralph" / "wt"
            write(lane_root, "q-idle/target/debug/big", "x")
            write(lane_root, "q-idle/target/release/big", "x")
            write(lane_root, "q-idle/target/ralph/evidence.log", "kept")
            write(lane_root, "q-run/target/release/ersilia", "in use")
            # A file-protocol leftover is not loop state: it protects nothing.
            write(lane_root, "q-stale/ralph/waiting", "ralph/never.done\n")
            write(lane_root, "q-stale/target/debug/big", "x")
            lanes = self.lanes(root, free=lambda: 3)
            free, out = said(lanes.reclaim_disk, {"q-run"}, 3)
            self.assertEqual(free, 3)
            self.assertFalse((lane_root / "q-idle/target/debug").exists())
            self.assertFalse((lane_root / "q-idle/target/release").exists())
            self.assertTrue((lane_root / "q-idle/target/ralph/evidence.log").exists())
            self.assertTrue((lane_root / "q-run/target/release/ersilia").exists())
            self.assertFalse((lane_root / "q-stale/target/debug").exists())
            self.assertIn("reclaimed", out)

    def test_the_least_recently_built_goes_first_and_it_stops_over_the_floor(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            lane_root = root / ".ralph" / "wt"
            write(lane_root, "q-old/target/debug/big", "x")
            write(lane_root, "q-new/target/debug/big", "x")
            now = time.time()
            os.utime(lane_root / "q-old/target/debug", (now - 100, now - 100))
            os.utime(lane_root / "q-new/target/debug", (now, now))
            lanes = self.lanes(root, free=lambda: 80)
            free, _ = said(lanes.reclaim_disk, set(), 3)
            self.assertEqual(free, 80)
            self.assertFalse((lane_root / "q-old/target/debug").exists())
            self.assertTrue((lane_root / "q-new/target/debug/big").exists())

    def test_over_the_floor_or_with_no_lanes_nothing_is_touched(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            lanes = self.lanes(root, free=lambda: self.fail("read the disk with nothing to do"))
            self.assertEqual(lanes.reclaim_disk(set(), 2), 2)        # no lane root yet
            write(root / ".ralph" / "wt", "q-idle/target/debug/big", "x")
            self.assertEqual(lanes.reclaim_disk(set(), ralph.DISK_FLOOR_GB), ralph.DISK_FLOOR_GB)
            self.assertTrue((root / ".ralph/wt/q-idle/target/debug/big").exists())


class DecisionRenumberTests(LaneFixture, unittest.TestCase):
    """Lanes in one wave each mint `<campaign>-<max+1>` from the same tree, so
    the second merge was an add/add conflict (PoolQueueTests (3) and (10))."""

    def decisions_repo(self, tmp):
        root = self.repo(tmp, "- [ ] q-a — depends []\n- [ ] q-b — depends []\n")
        install_script(root, "ralph-decisions.py")
        for part in ("_header.md", "_archive-ledger.md", "_flags.md", "_archive-appendices.md"):
            write(root, f"ralph/decisions/{part}", f"{part}\n")
        subprocess.run([sys.executable, "scripts/ralph-decisions.py", "--write"], cwd=root,
                       check=True, capture_output=True)
        commit_all(root, "decisions")
        return root

    def mint(self, wt, cite=False):
        """What a lane does, as PROMPT's pool-lane section says: the entry file
        only, minted in its own tree; `cite` also names the id in a file of its own."""
        if cite:
            write(wt, f"notes/{wt.name}.md", "Decision: ralph/decisions/q-1.md (q-1; not q-10)\n")
        subprocess.run([sys.executable, "scripts/ralph-decisions.py", "new", "q",
                        "--subject", wt.name, "--who", "worker", "--date", "2026-10-01"],
                       cwd=wt, check=True, capture_output=True)
        commit_all(wt, wt.name)

    def wave(self, root, cite=False):
        lanes = self.lanes(root)
        for unit in ("q-a", "q-b"):
            wt, _, _, _ = self.prepare(lanes, unit)
            self.mint(wt, cite)
        return lanes

    def land(self, lanes, unit, root):
        """As Loop._land_lane lands a lane: renumber, merge, regenerate, mark."""
        wt, branch = lanes.lane_root / unit, f"ralph/{unit}"
        refused, _ = said(lanes.renumber_decisions, unit, wt, branch)
        self.assertIsNone(refused)
        r = lanes.git("merge", "--no-ff", "-m", f"merge {unit}", branch)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIsNone(said(lanes.regenerate_decisions, unit)[0])
        ralph.Queue(root / "ralph/STATE.md").mark_done(unit)
        lanes.git("add", "--", "ralph/STATE.md")
        self.assertEqual(lanes.git("commit", "-q", "-m", f"ralph: {unit} done").returncode, 0)

    def test_two_lanes_minting_one_id_both_merge_and_the_ledger_is_current(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.decisions_repo(tmp)
            lanes = self.wave(root)
            for unit in ("q-a", "q-b"):
                self.land(lanes, unit, root)
            names = sorted(p.name for p in (root / "ralph/decisions").glob("q-*.md"))
            self.assertEqual(names, ["q-1.md", "q-2.md"])
            subjects = {(root / "ralph/decisions" / n).read_text().split(" · ")[2] for n in names}
            self.assertEqual(subjects, {"q-a", "q-b"})
            check = subprocess.run([sys.executable, "scripts/ralph-decisions.py", "--check"],
                                   cwd=root, capture_output=True, text=True)
            self.assertEqual(check.returncode, 0, check.stderr)
            self.assertEqual(git(root, "status", "--porcelain", "--untracked-files=no").stdout, "")

    def test_a_renumbered_lanes_citations_follow_its_entry(self):
        # Each lane cites the id it minted in a tracked file of its own. After
        # the merges each file names its own entry, a longer id that shares the
        # prefix is untouched, and the renumbered entry records the old id its
        # lane's commit bodies still carry.
        with tempfile.TemporaryDirectory() as tmp:
            root = self.decisions_repo(tmp)
            lanes = self.wave(root, cite=True)
            for unit in ("q-a", "q-b"):
                self.land(lanes, unit, root)
            decisions = root / "ralph/decisions"
            by_subject = {(decisions / f"{i}.md").read_text().split(" · ")[2]: i
                          for i in ("q-1", "q-2")}
            for unit, eid in by_subject.items():
                self.assertEqual((root / f"notes/{unit}.md").read_text(),
                                 f"Decision: ralph/decisions/{eid}.md ({eid}; not q-10)\n")
            self.assertIn("Minted as `q-1` in its lane", (decisions / "q-2.md").read_text())
            self.assertNotIn("Minted as", (decisions / "q-1.md").read_text())
            log = git(root, "log", "--format=%s").stdout
            self.assertIn("decision ids renumbered at merge (pool): q-1 → q-2", log)

    def test_without_the_renumber_the_second_merge_conflicts(self):
        # The failing input the renumber exists for (its PLANT, kept as a test).
        with tempfile.TemporaryDirectory() as tmp:
            root = self.decisions_repo(tmp)
            lanes = self.wave(root)
            self.land(lanes, "q-a", root)
            r = lanes.git("merge", "--no-ff", "-m", "merge q-b", "ralph/q-b")
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("ralph/decisions/q-1.md", r.stdout + r.stderr)
            lanes.git("merge", "--abort")


# -- the shell scripts ----------------------------------------------------------


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
    """`ralph-check.sh <name>` runs what the worker's OWN queue.toml declares.
    The script asks `scripts/ralph.py check-argv`, the loop under test."""

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

    def test_a_pool_lanes_share_reaches_its_checks_without_the_env(self):
        # `toolbox run` forwards no env, so the share is read from the lane's file.
        with tempfile.TemporaryDirectory() as tmp:
            self.fixture(tmp)
            write(tmp, "ralph/next/a/queue.toml",
                  '[checks]\njobs = ["sh", "-c", "echo $SOVEREIGN_LINT_JOBS/$SOVEREIGN_TEST_JOBS"]\n')
            write(tmp, ralph.LANE_JOBS_FILE, "SOVEREIGN_LINT_JOBS=3\nSOVEREIGN_TEST_JOBS=3\n")
            with mock.patch.dict(os.environ, {}, clear=False):
                os.environ.pop("SOVEREIGN_LINT_JOBS", None)
                os.environ.pop("SOVEREIGN_TEST_JOBS", None)
                r = self.check(tmp, "a", "jobs")
                self.assertIn("exit=0\n3/3\n", r.stdout)
                os.environ["SOVEREIGN_LINT_JOBS"] = "1"        # an explicit value wins
                r = self.check(tmp, "a", "jobs")
                self.assertIn("exit=0\n1/3\n", r.stdout)

    def test_a_refused_manifest_fails_the_check_by_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.fixture(tmp)
            write(tmp, "ralph/next/b/queue.toml", "[checks]\nlint = ['true']\n")
            r = self.check(tmp, "b", "hello")
            self.assertEqual(r.returncode, 2)
            self.assertIn("ralph/next/b/queue.toml", r.stderr)


class ShimTests(unittest.TestCase):
    def test_a_dropped_variant_is_said_once(self):
        # The empty prompt makes the shim exit 2 BEFORE it reaches `claude`: no session.
        # `thinking`/`fast` are opencode variants, not claude effort levels.
        r = subprocess.run([str(SCRIPTS / "ralph-claude-shim.sh"), "run", "--model", "m",
                            "--variant", "thinking", "--variant", "fast", ""],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)
        self.assertIn("empty prompt", r.stderr)
        self.assertEqual(r.stderr.count("--variant"), 1, r.stderr)
        self.assertIn("thinking", r.stderr)

    def test_an_effort_level_variant_reaches_claude_as_effort(self):
        # A stub `claude` on PATH prints its argv, so this sees the call the shim makes.
        with tempfile.TemporaryDirectory() as tmp:
            stub = pathlib.Path(tmp) / "claude"
            stub.write_text('#!/bin/sh\necho "argv: $*" >&2\n')
            stub.chmod(0o755)
            env = {**os.environ, "PATH": f"{tmp}:{os.environ['PATH']}",
                   "RALPH_PERMISSION_BRIDGE": "0"}
            r = subprocess.run([str(SCRIPTS / "ralph-claude-shim.sh"), "run", "--model", "m",
                                "--variant", "medium", "do the unit"],
                               capture_output=True, text=True, env=env)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("--effort medium", r.stderr)
        self.assertIn("effort=medium", r.stderr)
        self.assertNotIn("dropping --variant", r.stderr)


if __name__ == "__main__":
    unittest.main()
