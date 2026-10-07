#!/usr/bin/env python3
"""scripts/ralph-claude-shim.sh's MCP config: a worker gets the WORKDIR's
own .mcp.json, which names that repo's code corpus, and no repo's when its
workdir has none. A stand-in `claude` on PATH records the argv it was given.

    python3 scripts/tests/ralph_claude_shim.py
"""
import json
import os
import pathlib
import subprocess
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent.parent.parent
SHIM = HERE / "scripts" / "ralph-claude-shim.sh"

FAKE_CLAUDE = """#!/usr/bin/env python3
import json, os, sys
sys.stdin.read()
with open(os.environ["SHIM_ARGV_OUT"], "w") as f:
    json.dump(sys.argv[1:], f)
"""


class ShimMcpConfigTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.tmp.name)
        self.workdir = root / "other-repo"
        self.workdir.mkdir()
        bin_dir = root / "bin"
        bin_dir.mkdir()
        claude = bin_dir / "claude"
        claude.write_text(FAKE_CLAUDE)
        claude.chmod(0o755)
        self.settings = root / "settings.json"
        self.settings.write_text("{}")
        self.argv_out = root / "argv.json"
        self.env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}",
                    "RALPH_CLAUDE_SETTINGS": str(self.settings),
                    "SHIM_ARGV_OUT": str(self.argv_out)}

    def tearDown(self):
        self.tmp.cleanup()

    def argv(self):
        proc = subprocess.run(["bash", str(SHIM), "run", "do the thing"], cwd=self.workdir,
                              env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return json.loads(self.argv_out.read_text()), proc.stderr

    def mcp_configs(self, argv):
        return [argv[i + 1] for i, a in enumerate(argv) if a == "--mcp-config"]

    def test_a_workdir_with_its_own_config_hands_the_worker_that_config(self):
        (self.workdir / ".mcp.json").write_text(json.dumps({"mcpServers": {}}))
        argv, _ = self.argv()
        files = [c for c in self.mcp_configs(argv) if not c.startswith("{")]
        self.assertEqual([os.path.realpath(f) for f in files],
                         [os.path.realpath(self.workdir / ".mcp.json")], argv)
        self.assertIn("--strict-mcp-config", argv)

    def test_a_workdir_without_one_gets_no_repos_config_and_says_so(self):
        # FAILING INPUT: the shim's old line handed every worker this
        # checkout's .mcp.json, so commonwealth-ai's server reached zoracite.
        argv, stderr = self.argv()
        files = [c for c in self.mcp_configs(argv) if not c.startswith("{")]
        self.assertEqual(files, [], argv)
        self.assertIn("--strict-mcp-config", argv)
        self.assertIn("no .mcp.json", stderr)

    def test_the_permission_bridge_rides_beside_either(self):
        argv, _ = self.argv()
        inline = [c for c in self.mcp_configs(argv) if c.startswith("{")]
        self.assertEqual(len(inline), 1, argv)
        self.assertIn("ralph-permission-bridge.py", inline[0])


if __name__ == "__main__":
    unittest.main()
