#!/usr/bin/env python3
"""scripts/ralph-permission-bridge.py, driven as claude drives it: a stdio
JSON-RPC process with one `approve` tool, run with the env ralph.py's
session_env and the shim give a worker session.

Every case points the loop's control dir at a temporary directory, so no
case writes into this checkout's ralph/, where a live loop may be waiting.

    python3 scripts/tests/ralph_permission_bridge.py
"""
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import threading
import time
import unittest

BRIDGE = pathlib.Path(__file__).resolve().parent.parent / "ralph-permission-bridge.py"


def call(env, answer=None, request_name="PERMISSION_REQUEST.md", seen=None):
    """One `approve` call for `echo hi`. When `answer` is given, it is written
    to the answer file beside the request as soon as the request appears;
    `seen` collects the request's text. Returns the permission result, or
    None when the bridge died without answering."""
    ctl = pathlib.Path(env["RALPH_WORKDIR"]) / env["RALPH_CONTROL_DIR"]

    def answerer():
        request = ctl / request_name
        deadline = time.time() + 10
        while time.time() < deadline:
            if request.exists():
                if seen is not None:
                    seen.append(request.read_text())
                stem = request_name.replace("PERMISSION_REQUEST", "PERMISSION_ANSWER")
                (ctl / stem.removesuffix(".md")).write_text(answer + "\n")
                return
            time.sleep(0.05)

    if answer is not None:
        threading.Thread(target=answerer, daemon=True).start()
    lines = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
         "params": {"name": "approve",
                    "arguments": {"tool_name": "Bash", "input": {"command": "echo hi"}}}},
    ]
    proc = subprocess.run([sys.executable, str(BRIDGE)], input="".join(json.dumps(m) + "\n" for m in lines),
                          capture_output=True, text=True, timeout=30, env={**os.environ, **env})
    for line in proc.stdout.splitlines():
        msg = json.loads(line)
        if msg.get("id") == 2:
            return json.loads(msg["result"]["content"][0]["text"])
    return None


class PermissionBridgeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.workdir = pathlib.Path(self.tmp.name) / "other-repo"
        self.ctl = self.workdir / "ralph" / "next" / "q" / "ctl"
        self.ctl.mkdir(parents=True)
        self.settings = self.workdir / "ralph" / "claude-settings.json"
        self.settings.write_text(json.dumps({"permissions": {"allow": []}}))
        self.env = {"RALPH_WORKDIR": str(self.workdir), "RALPH_CONTROL_DIR": "ralph/next/q/ctl",
                    "RALPH_CLAUDE_SETTINGS": str(self.settings), "RALPH_SESSION_CWD": str(self.workdir),
                    "RALPH_QUEUE": "q", "RALPH_PERMISSION_WAIT_SECS": "10"}

    def tearDown(self):
        self.tmp.cleanup()

    def test_a_loop_in_another_repo_is_asked_in_its_own_control_dir(self):
        # Before 2026-10-06 the bridge raised on SETTINGS.relative_to(HERE)
        # here and died, and the worker saw "Connection closed".
        seen = []
        result = call(self.env, answer="allow", seen=seen)
        self.assertEqual(result, {"behavior": "allow", "updatedInput": {"command": "echo hi"}})
        self.assertIn(str(self.ctl / "PERMISSION_ANSWER"), seen[0])
        self.assertIn("allow Bash", (self.ctl / "log-permissions.txt").read_text())

    def test_always_appends_to_the_loops_own_settings(self):
        call(self.env, answer="always")
        self.assertIn("Bash(echo hi)", json.loads(self.settings.read_text())["permissions"]["allow"])

    def test_a_pool_lane_gets_files_of_its_own(self):
        env = {**self.env, "RALPH_SESSION_CWD": str(pathlib.Path(self.tmp.name) / "lane-a")}
        result = call(env, answer="deny not this one", request_name="PERMISSION_REQUEST-lane-a.md")
        self.assertEqual(result["behavior"], "deny")
        self.assertIn("not this one", result["message"])

    def test_empty_settings_is_the_default_never_the_cwd(self):
        # ralph.py's session_env sets RALPH_CLAUDE_SETTINGS empty for a loop
        # whose manifest names none; Path("") would be the cwd.
        seen = []
        call({**self.env, "RALPH_CLAUDE_SETTINGS": ""}, answer="deny", seen=seen)
        self.assertIn("ralph/claude-settings.json", seen[0])

    def test_no_answer_is_a_deny_that_says_so(self):
        result = call({**self.env, "RALPH_PERMISSION_WAIT_SECS": "1"})
        self.assertEqual(result["behavior"], "deny")
        self.assertIn("not answered", result["message"])

    def test_a_bridge_error_is_a_deny_naming_it_not_a_dead_server(self):
        (self.workdir / "ralph" / "next" / "q").chmod(0o500)
        self.ctl.chmod(0o500)
        try:
            result = call(self.env)
        finally:
            self.ctl.chmod(0o700)
            (self.workdir / "ralph" / "next" / "q").chmod(0o700)
        self.assertEqual(result["behavior"], "deny")
        self.assertIn("permission bridge failed", result["message"])


if __name__ == "__main__":
    unittest.main()
