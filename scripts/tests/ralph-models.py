#!/usr/bin/env python3
"""ralph-models.sh: set/show the per-host models.env without losing keys."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


HELPER = Path(__file__).resolve().parents[1] / "ralph-models.sh"


class ModelsFile(unittest.TestCase):
    def helper(self, root, *args):
        return subprocess.run(
            ["bash", str(HELPER), "--workdir", str(root), "--no-restart", *args],
            env=os.environ, text=True, capture_output=True, timeout=10)

    def test_set_preserves_unspecified_keys_and_show(self):
        with tempfile.TemporaryDirectory(prefix="ralph-models-") as tmp:
            root = Path(tmp)
            r = self.helper(root, "--model", "a/worker", "--review-model", "b/reviewer",
                            "--variant", "high")
            self.assertEqual(r.returncode, 0, r.stderr)
            text = (root / "ralph/models.env").read_text()
            self.assertIn("MODEL=a/worker", text)
            self.assertIn("REVIEW_MODEL=b/reviewer", text)
            self.assertIn("VARIANT=high", text)

            r = self.helper(root, "--model", "c/worker")
            self.assertEqual(r.returncode, 0, r.stderr)
            text = (root / "ralph/models.env").read_text()
            self.assertIn("MODEL=c/worker", text)
            self.assertIn("REVIEW_MODEL=b/reviewer", text)
            self.assertIn("VARIANT=high", text)

            shown = self.helper(root)
            self.assertIn("MODEL=c/worker", shown.stdout)
            self.assertIn("REVIEW_MODEL=b/reviewer", shown.stdout)


if __name__ == "__main__":
    unittest.main()
