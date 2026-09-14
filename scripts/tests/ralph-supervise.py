#!/usr/bin/env python3
"""End-to-end supervisor stops and resolution, without model calls."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SUPERVISOR = Path(__file__).resolve().parents[1] / "ralph-supervise.sh"


class Supervision(unittest.TestCase):
    def run_case(self, mode):
        with tempfile.TemporaryDirectory(prefix="ralph-supervise-") as tmp:
            root = Path(tmp)
            (root / "ralph").mkdir()
            (root / "bin").mkdir()
            row = "HUMAN-design" if mode == "human" else "dm-build"
            (root / "ralph/STATE.md").write_text(f"- [ ] {row} — depends []\n")
            if mode in ("human", "operator", "resolve", "unresolved"):
                (root / "ralph/STOP").touch()
            if mode != "operator":
                (root / "ralph/NEEDS_HUMAN.md").write_text("fixture blocker\n")
            worker = root / "bin/opencode"
            worker.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, pathlib, sys\n"
                "assert not pathlib.Path('ralph/STOP').exists(), 'resolver inherited STOP'\n"
                "with open('calls.jsonl', 'a') as f: f.write(json.dumps(sys.argv[1:])+'\\n')\n"
                "if os.environ['CASE'] == 'resolve':\n"
                " pathlib.Path('ralph/NEEDS_HUMAN.md').unlink()\n"
                " pathlib.Path('resolved').touch()\n"
                "if os.environ['CASE'] == 'interrupt': pathlib.Path('ralph/STOP').touch()\n"
            )
            worker.chmod(0o755)
            inner = root / "bin/campaign"
            inner.write_text(
                "#!/bin/bash\n"
                "[ -f resolved ] && { touch ralph/DONE; exit 0; }\n"
                "exit 2\n"
            )
            inner.chmod(0o755)
            sleeper = root / "bin/sleep"
            sleeper.write_text("#!/bin/sh\nexec /bin/sleep 0.05\n")
            sleeper.chmod(0o755)
            env = dict(os.environ, HOME=tmp, TMPDIR=tmp, CASE=mode,
                       PATH=f"{root / 'bin'}:{os.environ['PATH']}",
                       RALPH_OPENCODE_BIN=str(worker), RALPH_RESOLVE_MAX="2")
            subprocess.run(["git", "init", "-q", tmp], check=True, env=env)
            result = subprocess.run(
                ["bash", str(SUPERVISOR), "--workdir", tmp, "--no-notify", "--",
                 str(inner), "--review-model", "strong/reviewer", "--variant", "high"],
                env=env, text=True, capture_output=True, timeout=10)
            calls = root / "calls.jsonl"
            return result, [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else [], (root / "ralph/DONE").exists(), (root / "ralph/STOP").exists()

    def test_blocker_resolves_and_resumes_with_review_model_and_effort(self):
        result, calls, done, _ = self.run_case("resolve")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(done)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][:5], ["run", "--model", "strong/reviewer", "--variant", "high"])

    def test_human_approval_never_dispatched_to_resolver(self):
        result, calls, done, stopped = self.run_case("human")
        self.assertEqual(calls, [], result.stdout)
        self.assertFalse(done)
        self.assertTrue(stopped)
        self.assertIn("HUMAN-design", result.stdout)

    def test_operator_stop_is_preserved(self):
        result, calls, done, stopped = self.run_case("operator")
        self.assertEqual(calls, [], result.stdout)
        self.assertFalse(done)
        self.assertTrue(stopped)
        self.assertIn("operator STOP", result.stdout)

    def test_unresolved_blocker_has_bounded_attempts(self):
        result, calls, done, _ = self.run_case("unresolved")
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertEqual(len(calls), 2)
        self.assertFalse(done)
        self.assertIn("leaving it to the operator", result.stdout)

    def test_operator_stop_during_resolution_is_preserved(self):
        result, calls, done, stopped = self.run_case("interrupt")
        self.assertEqual(len(calls), 1)
        self.assertFalse(done)
        self.assertTrue(stopped)
        self.assertIn("operator STOP during resolution", result.stdout)


if __name__ == "__main__":
    unittest.main()
