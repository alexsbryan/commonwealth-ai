#!/usr/bin/env python3
"""ralph-watch.sh: nag once per condition, catch a stopped job."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


WATCH = Path(__file__).resolve().parents[1] / "ralph-watch.sh"


class Watch(unittest.TestCase):
    def run_watch(self, root, *, running=True):
        bin_dir = root / "bin"
        bin_dir.mkdir(exist_ok=True)
        stub = bin_dir / "launchctl"
        stub.write_text("#!/bin/sh\n" + ("echo 'state = running'\n" if running else "exit 1\n"))
        stub.chmod(0o755)
        env = dict(os.environ, HOME=str(root), RALPH_WATCH_DRY="1",
                   RALPH_WATCH_LAUNCHCTL=str(stub))
        return subprocess.run(
            ["bash", str(WATCH), "--workdir", str(root), "--label", "t"],
            env=env, text=True, capture_output=True, timeout=10)

    def test_needs_human_notifies_once_per_content(self):
        with tempfile.TemporaryDirectory(prefix="ralph-watch-") as tmp:
            root = Path(tmp)
            (root / "ralph").mkdir()
            (root / "ralph/NEEDS_HUMAN.md").write_text("# unit-a\n")
            r1 = self.run_watch(root)
            self.assertIn("needs human", r1.stdout)
            r2 = self.run_watch(root)
            self.assertEqual(r2.stdout.strip(), "")
            (root / "ralph/NEEDS_HUMAN.md").write_text("# unit-b\n")
            r3 = self.run_watch(root)
            self.assertIn("needs human", r3.stdout)
            self.assertIn("unit-b", r3.stdout)

    def test_stopped_job_notifies(self):
        with tempfile.TemporaryDirectory(prefix="ralph-watch-") as tmp:
            root = Path(tmp)
            (root / "ralph").mkdir()
            r = self.run_watch(root, running=False)
            self.assertIn("loop down", r.stdout)

    def test_healthy_job_is_silent_and_clears_state(self):
        with tempfile.TemporaryDirectory(prefix="ralph-watch-") as tmp:
            root = Path(tmp)
            (root / "ralph").mkdir()
            (root / "ralph/NEEDS_HUMAN.md").write_text("# x\n")
            self.run_watch(root)
            (root / "ralph/NEEDS_HUMAN.md").unlink()
            r = self.run_watch(root, running=True)
            self.assertEqual(r.stdout.strip(), "")
            states = list((root / ".svrnmesh/ralph").glob("**/watch.state"))
            self.assertEqual(len(states), 1)
            self.assertEqual(states[0].read_text(), "")

    def test_done_or_stop_is_not_a_down_nag(self):
        with tempfile.TemporaryDirectory(prefix="ralph-watch-") as tmp:
            root = Path(tmp)
            (root / "ralph").mkdir()
            (root / "ralph/DONE").touch()
            r = self.run_watch(root, running=False)
            self.assertEqual(r.stdout.strip(), "")


if __name__ == "__main__":
    unittest.main()
