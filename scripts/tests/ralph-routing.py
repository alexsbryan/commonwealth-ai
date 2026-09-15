#!/usr/bin/env python3
"""Exercise serial routing through the real loop with an argv-recording worker."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest


LOOP = Path(__file__).resolve().parents[1] / "ralph-loop.sh"


class Routing(unittest.TestCase):
    def run_queue(self, rows, *, models=False, supervised=False, models_file=None,
                  review_flag="strong/reviewer", extra_args=(), waiting_age=None):
        with tempfile.TemporaryDirectory(prefix="ralph-routing-") as tmp:
            root = Path(tmp)
            (root / "ralph").mkdir()
            (root / "bin").mkdir()
            (root / "ralph/STATE.md").write_text(rows)
            (root / "ralph/PROMPT.md").write_text("Execute the selected unit.")
            if models_file is not None:
                (root / "ralph/models.env").write_text(models_file)
            if waiting_age is not None:
                w = root / "ralph/waiting"
                w.write_text("never.done\n")
                os.utime(w, (time.time() - waiting_age, time.time() - waiting_age))
            worker = root / "bin/opencode"
            worker.write_text(
                "#!/usr/bin/env python3\n"
                "import json, pathlib, sys\n"
                "pathlib.Path('argv.json').write_text(json.dumps(sys.argv[1:]))\n"
                "pathlib.Path('ralph/DONE').touch()\n"
            )
            worker.chmod(0o755)
            sleeper = root / "bin/sleep"
            sleeper.write_text("#!/bin/sh\nexec /bin/sleep 0.05\n")
            sleeper.chmod(0o755)
            env = dict(os.environ, HOME=tmp, PATH=f"{root / 'bin'}:{os.environ['PATH']}",
                       RALPH_OPENCODE_BIN=str(worker))
            subprocess.run(["git", "init", "-q", tmp], check=True, env=env)
            cmd = ["bash", str(LOOP), "--workdir", tmp, "--max-iter", "2"]
            if review_flag:
                cmd += ["--review-model", review_flag]
            if models:
                cmd += ["--model", "worker/terra", "--variant", "high"]
            if supervised:
                cmd += ["--supervise"]
            cmd += list(extra_args)
            result = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=10)
            args = root / "argv.json"
            return result, json.loads(args.read_text()) if args.exists() else None

    def test_blocked_human_does_not_hide_ready_review(self):
        result, args = self.run_queue(
            "- [ ] HUMAN-later — depends [dm-later]\n"
            "- [ ] REVIEW-ready — depends []\n"
            "- [ ] dm-later — depends [REVIEW-ready]\n"
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("strong/reviewer", args)

    def test_blocked_review_does_not_steal_worker(self):
        _, args = self.run_queue(
            "- [ ] REVIEW-blocked — depends [dm-ready]\n"
            "- [ ] dm-ready — depends []\n", models=True)
        self.assertEqual(args[:5], ["run", "--model", "worker/terra", "--variant", "high"])

    def test_review_preserves_high_effort(self):
        _, args = self.run_queue("- [ ] REVIEW-ready — depends []\n", models=True)
        self.assertEqual(args[:5], ["run", "--model", "strong/reviewer", "--variant", "high"])

    def test_in_progress_takes_precedence(self):
        _, args = self.run_queue(
            "- [ ] dm-ready — depends []\n"
            "- [~] REVIEW-resume — depends [unfinished]\n")
        self.assertIn("strong/reviewer", args)

    def test_all_dependencies_required(self):
        _, args = self.run_queue(
            "- [x] dm-done abc123 — depends []\n"
            "- [ ] REVIEW-blocked — depends [dm-done, dm-next]\n"
            "- [ ] dm-next — depends [dm-done]\n", models=True)
        self.assertIn("worker/terra", args)

    def test_ready_human_stops_before_worker(self):
        result, args = self.run_queue(
            "- [ ] HUMAN-design-review — depends [] — operator approval\n"
            "- [ ] dm-later — depends [HUMAN-design-review]\n")
        self.assertIsNone(args)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("HUMAN-design-review", result.stdout)

    def test_no_ready_row_stops_before_worker(self):
        result, args = self.run_queue("- [ ] REVIEW-blocked — depends [missing]\n")
        self.assertIsNone(args)
        self.assertNotEqual(result.returncode, 0)

    def test_supervise_flag_forwards_models_and_preserves_human_stop(self):
        result, args = self.run_queue("- [ ] HUMAN-design — depends []\n",
                                      models=True, supervised=True)
        self.assertIsNone(args)
        self.assertIn("supervisor resolution: model strong/reviewer, variant high", result.stdout)
        self.assertIn("operator approval required", result.stdout)


    def test_models_file_supplies_defaults(self):
        _, args = self.run_queue(
            "- [ ] dm-ready — depends []\n", review_flag=None,
            models_file="MODEL=file/worker\nREVIEW_MODEL=file/reviewer\nVARIANT=medium\n")
        self.assertEqual(args[:5], ["run", "--model", "file/worker", "--variant", "medium"])

    def test_flags_override_models_file(self):
        _, args = self.run_queue("- [ ] dm-ready — depends []\n", models=True,
                                 models_file="MODEL=file/worker\nVARIANT=medium\n")
        self.assertEqual(args[:5], ["run", "--model", "worker/terra", "--variant", "high"])

    def test_review_row_reads_models_file(self):
        _, args = self.run_queue(
            "- [ ] REVIEW-ready — depends []\n", review_flag=None,
            models_file="MODEL=file/worker\nREVIEW_MODEL=file/reviewer\nVARIANT=medium\n")
        self.assertEqual(args[:5], ["run", "--model", "file/reviewer", "--variant", "medium"])


    def test_stale_waiting_marker_halts(self):
        result, args = self.run_queue(
            "- [ ] dm-ready — depends []\n", waiting_age=7200,
            extra_args=("--marker-timeout", "60"))
        self.assertIsNone(args)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("HALT", result.stdout)
        self.assertIn("never.done", result.stdout)

    def test_heartbeat_writes_timestamp_and_context(self):
        with tempfile.TemporaryDirectory(prefix="ralph-hb-") as tmp:
            hb = f"{tmp}/hb"
            r = subprocess.run(
                ["bash", "-c",
                 '. scripts/ralph-lib.sh; HEARTBEAT_FILE="$1" heartbeat "iteration 7"; cat "$1"',
                 "_", hb],
                cwd=str(LOOP.parent.parent), env=dict(os.environ),
                text=True, capture_output=True, timeout=10)
            self.assertRegex(r.stdout, r"^\d+ iteration 7\n$")


if __name__ == "__main__":
    unittest.main()
