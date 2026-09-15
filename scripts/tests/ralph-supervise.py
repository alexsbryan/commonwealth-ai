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
    def run_case(self, mode, *, models_file=None,
                 inner_flags=("--review-model", "strong/reviewer", "--variant", "high")):
        with tempfile.TemporaryDirectory(prefix="ralph-supervise-") as tmp:
            root = Path(tmp)
            (root / "ralph").mkdir()
            (root / "bin").mkdir()
            row = "HUMAN-design" if mode == "human" else "dm-build"
            (root / "ralph/STATE.md").write_text(f"- [ ] {row} — depends []\n")
            if mode == "haltstop":
                (root / "ralph/STOP").write_text("halt: fixture\n")
            else:
                if mode in ("human", "operator", "resolve", "unresolved", "noop", "junk"):
                    (root / "ralph/STOP").touch()
                if mode != "operator":
                    (root / "ralph/NEEDS_HUMAN.md").write_text("fixture blocker\n")
            if models_file is not None:
                (root / "ralph/models.env").write_text(models_file)
            worker = root / "bin/opencode"
            worker.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, pathlib, sys\n"
                "assert not pathlib.Path('ralph/STOP').exists(), 'resolver inherited STOP'\n"
                "with open('calls.jsonl', 'a') as f: f.write(json.dumps(sys.argv[1:])+'\\n')\n"
                "if os.environ['CASE'] == 'resolve':\n"
                " pathlib.Path('ralph/NEEDS_HUMAN.md').unlink()\n"
                " pathlib.Path('resolved').touch()\n"
                "if os.environ['CASE'] == 'haltstop':\n"
                " pathlib.Path('ralph/NEEDS_HUMAN.md').unlink(missing_ok=True)\n"
                " pathlib.Path('resolved').touch()\n"
                "if os.environ['CASE'] == 'junk':\n"
                " open('junk', 'a').write('x')\n"
                " os.system('git add junk && git commit -q -m junk')\n"
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
            subprocess.run(["git", "-C", tmp, "config", "user.email", "t@t"], check=True, env=env)
            subprocess.run(["git", "-C", tmp, "config", "user.name", "t"], check=True, env=env)
            result = subprocess.run(
                ["bash", str(SUPERVISOR), "--workdir", tmp, "--no-notify", "--",
                 str(inner), *inner_flags],
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

    def test_operator_stop_during_resolution_is_preserved(self):
        result, calls, done, stopped = self.run_case("interrupt")
        self.assertEqual(len(calls), 1)
        self.assertFalse(done)
        self.assertTrue(stopped)
        self.assertIn("operator STOP during resolution", result.stdout)

    def test_noop_resolution_escalates_immediately(self):
        result, calls, done, _ = self.run_case("noop")
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertEqual(len(calls), 1)
        self.assertFalse(done)
        self.assertIn("changed nothing", result.stdout)

    def test_junk_commits_do_not_reset_the_bound(self):
        result, calls, done, _ = self.run_case("junk")
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertEqual(len(calls), 2)
        self.assertFalse(done)
        self.assertIn("did not clear it", result.stdout)

    def test_halt_stop_without_package_is_not_an_operator_stop(self):
        result, calls, done, _ = self.run_case("haltstop")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(calls), 1)
        self.assertTrue(done)

    def test_models_file_supplies_resolver_when_inner_has_none(self):
        result, calls, done, _ = self.run_case(
            "resolve", models_file="REVIEW_MODEL=file/reviewer\nVARIANT=medium\n",
            inner_flags=())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(done)
        self.assertEqual(calls[0][:5], ["run", "--model", "file/reviewer", "--variant", "medium"])


if __name__ == "__main__":
    unittest.main()
