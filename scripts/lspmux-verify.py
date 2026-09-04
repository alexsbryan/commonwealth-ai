#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Prove the shared rust-analyzer on THIS host — both shim arms, one server.

`scripts/tests/lspmux-shim.sh` pins the shim's dispatch against stub binaries
and runs in pre-push.  This script is the other half: it drives the REAL
rust-analyzer and a REAL lspmux server, and answers the two questions the
stubs cannot.

  1. Does an invocation WITH arguments still reach rust-analyzer's own CLI?
     The daemon's SCIP exporter spawns `rust-analyzer scip . --config-path …`
     through a plain PATH lookup, and an export that produces nothing wipes
     the code-intel graph to zero symbols.  This runs before the shim is ever
     put on PATH.

  2. Does an invocation WITHOUT arguments give two independent clients ONE
     shared server?  That is the entire point of the exercise.

Exit 0 only if every check passes.  Run it any time:

    scripts/lspmux-verify.py                       # default shim + this repo
    scripts/lspmux-verify.py --shim ~/.local/lspmux-shim/rust-analyzer
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

# rust-analyzer answers `initialize` before it has loaded the workspace, so
# this is a handshake budget, not an indexing budget.  Generous because the
# lspmux server may be cold-starting the analyzer underneath us.
HANDSHAKE_TIMEOUT_S = 90


class Report:
    def __init__(self) -> None:
        self.failures = 0

    def ok(self, label: str, detail: str = "") -> None:
        print(f"  ok   {label}" + (f" — {detail}" if detail else ""))

    def fail(self, label: str, detail: str) -> None:
        print(f"  FAIL {label}\n       {detail}")
        self.failures += 1

    def check(self, cond: bool, label: str, detail: str) -> bool:
        (self.ok if cond else self.fail)(label, detail)
        return cond


# ── LSP over stdio, just enough of it ──────────────────────────────────────


class LspClient:
    """One `rust-analyzer` invocation with no arguments — i.e. one LSP client.

    Through the shim this is a `lspmux client`, so several of these should
    land on a single language server.
    """

    def __init__(self, shim: Path, root: Path, name: str) -> None:
        self.name = name
        self.proc = subprocess.Popen(
            [str(shim)],
            cwd=str(root),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.root = root

    def _send(self, obj: dict) -> None:
        body = json.dumps(obj).encode()
        assert self.proc.stdin is not None
        self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body))
        self.proc.stdin.write(body)
        self.proc.stdin.flush()

    def _read(self) -> dict | None:
        out = self.proc.stdout
        assert out is not None
        headers: dict[str, str] = {}
        while True:
            line = out.readline()
            if not line:
                return None
            line = line.strip()
            if not line:
                break
            key, _, val = line.decode("utf-8", "replace").partition(":")
            headers[key.strip().lower()] = val.strip()
        length = int(headers.get("content-length", 0))
        if length == 0:
            return None
        return json.loads(out.read(length))

    def initialize(self) -> dict:
        """Send `initialize`, return its result. Raises on timeout."""
        self._send(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "processId": os.getpid(),
                    "rootUri": self.root.as_uri(),
                    # lspmux keys instances on the workspace root, so naming it
                    # explicitly is what makes two clients share one server
                    # even when they were started from different directories.
                    "workspaceFolders": [
                        {"uri": self.root.as_uri(), "name": self.root.name}
                    ],
                    "capabilities": {},
                },
            }
        )
        result: dict = {}
        error: list[BaseException] = []

        def pump() -> None:
            try:
                while True:
                    msg = self._read()
                    if msg is None:
                        raise RuntimeError("server closed the connection")
                    if msg.get("id") == 1:
                        result.update(msg)
                        return
            except BaseException as exc:  # noqa: BLE001 — reported to caller
                error.append(exc)

        thread = threading.Thread(target=pump, daemon=True)
        thread.start()
        thread.join(HANDSHAKE_TIMEOUT_S)
        if thread.is_alive():
            raise TimeoutError(f"no initialize response in {HANDSHAKE_TIMEOUT_S}s")
        if error:
            raise error[0]
        return result

    def initialized(self) -> None:
        """Send the `initialized` notification.

        Not ceremony: lspmux attaches a client to the shared instance only
        after it sees this (client.rs, `add_client` sits below the wait for
        it).  A verification that stops at the `initialize` response counts
        zero clients on an instance that is in fact serving it.
        """
        self._send({"jsonrpc": "2.0", "method": "initialized", "params": {}})

    def close(self) -> None:
        try:
            self._send({"jsonrpc": "2.0", "id": 2, "method": "shutdown"})
            self._send({"jsonrpc": "2.0", "method": "exit"})
        except (BrokenPipeError, OSError):
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()

    def stderr_tail(self) -> str:
        if self.proc.stderr is None:
            return ""
        try:
            return self.proc.stderr.read1(4096).decode("utf-8", "replace")
        except Exception:  # noqa: BLE001
            return ""


# ── Which rust-analyzer processes are LSP servers? ─────────────────────────


def lsp_server_pids() -> list[int]:
    """PIDs of rust-analyzer processes running in LSP (stdio) mode.

    Batch invocations (`rust-analyzer scip …`) and the proc-macro helper are
    excluded: a full SCIP export is often in flight on this box and counting
    it would make the sharing check say whatever the daemon happened to be
    doing at the time.
    """
    pids = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            argv = (entry / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        argv = [a.decode("utf-8", "replace") for a in argv if a]
        if not argv:
            continue
        exe = os.path.basename(argv[0])
        if exe != "rust-analyzer":
            continue
        # A subcommand is a positional argument; LSP mode has none.
        if any(not a.startswith("-") for a in argv[1:]):
            continue
        pids.append(int(entry.name))
    return sorted(pids)


def lspmux_status(lspmux: str) -> dict:
    out = subprocess.run(
        [lspmux, "status", "--json"], capture_output=True, text=True, timeout=30
    )
    if out.returncode != 0:
        raise RuntimeError(f"lspmux status failed: {out.stderr.strip()}")
    return json.loads(out.stdout)


# ── The checks ─────────────────────────────────────────────────────────────


def check_passthrough(rep: Report, shim: Path) -> None:
    """Arm 2 — arguments reach the real rust-analyzer, untouched.

    Run FIRST and before the shim goes on PATH: this is the arm whose failure
    is silent and expensive.
    """
    print("Arguments pass through to the real rust-analyzer")

    out = subprocess.run([str(shim), "--version"], capture_output=True, text=True)
    rep.check(
        out.stdout.startswith("rust-analyzer "),
        "`--version` is answered by rust-analyzer",
        out.stdout.strip() or out.stderr.strip() or "(no output)",
    )

    # The SCIP subcommand specifically: `--version` is not a flag rust-analyzer's
    # scip parser knows, and its refusal is a sentence only rust-analyzer emits.
    # A shim that swallowed the subcommand could not produce it.
    out = subprocess.run([str(shim), "scip", "--version"], capture_output=True, text=True)
    combined = out.stdout + out.stderr
    rep.check(
        "unexpected flag" in combined and "--version" in combined,
        "`scip --version` reaches rust-analyzer's own scip parser",
        combined.strip().splitlines()[0] if combined.strip() else "(no output)",
    )

    out = subprocess.run([str(shim), "scip", "--help"], capture_output=True, text=True)
    combined = out.stdout + out.stderr
    rep.check(
        "LSP server for the Rust programming language" in combined,
        "`scip --help` prints rust-analyzer's own help",
        f"exit {out.returncode}",
    )


def check_sharing(rep: Report, shim: Path, root: Path, lspmux: str) -> None:
    """Arm 1 — two independent clients, one server."""
    print("Two clients, one server")

    before = set(lsp_server_pids())
    clients = [LspClient(shim, root, f"client-{i}") for i in (1, 2)]
    try:
        for client in clients:
            try:
                msg = client.initialize()
            except Exception as exc:  # noqa: BLE001
                rep.fail(
                    f"{client.name} completed the LSP handshake",
                    f"{exc}; stderr: {client.stderr_tail().strip()[:400]}",
                )
                return
            name = (
                msg.get("result", {}).get("serverInfo", {}).get("name", "")
            )
            rep.check(
                name == "rust-analyzer",
                f"{client.name} is talking to rust-analyzer",
                f"serverInfo.name = {name!r}",
            )
            client.initialized()

        # Attachment is a round-trip through the server, so poll rather than
        # assume; a bare sleep would either be flaky or slower than it needs
        # to be.
        status: dict = {}
        mine: list[dict] = []
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            status = lspmux_status(lspmux)
            mine = [
                inst
                for inst in status.get("instances", [])
                if Path(inst.get("workspace_root", "")).resolve() == root.resolve()
            ]
            if mine and len(mine[0].get("clients", [])) >= len(clients):
                break
            time.sleep(0.25)

        instances = status.get("instances", [])
        rep.check(
            len(mine) == 1,
            "lspmux holds exactly one instance for this workspace",
            f"{len(mine)} instance(s) for {root}; {len(instances)} total",
        )
        if mine:
            rep.check(
                len(mine[0].get("clients", [])) == 2,
                "both clients are attached to that one instance",
                f"{len(mine[0].get('clients', []))} client(s), server pid {mine[0].get('pid')}",
            )

        # Two assertions, because "one process" has two failure modes and
        # only one of them is a new process appearing.  An idle instance
        # lives on for `instance_timeout` (300s by default), so a second run
        # a minute later legitimately spawns nothing at all — that is the
        # feature working, not the check failing.
        lsp_pids = set(lsp_server_pids())
        spawned = sorted(lsp_pids - before)
        server_pid = mine[0].get("pid") if mine else None
        rep.check(
            server_pid in lsp_pids,
            "the shared instance is a live rust-analyzer in LSP mode",
            f"pid {server_pid}"
            + (" (spawned for this run)" if server_pid in spawned else " (warm, reused)"),
        )
        rep.check(
            len(spawned) <= 1,
            "two clients did not start two servers",
            f"new LSP-mode rust-analyzer pids: {spawned or 'none — both clients reused a warm server'}",
        )
    finally:
        for client in clients:
            client.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--shim",
        default=os.environ.get("LSPMUX_SHIM", str(Path.home() / ".local/lspmux-shim/rust-analyzer")),
        help="the shim to exercise (default: ~/.local/lspmux-shim/rust-analyzer)",
    )
    parser.add_argument(
        "--workspace",
        default=None,
        help="workspace root the clients open (default: this repo)",
    )
    parser.add_argument(
        "--passthrough-only",
        action="store_true",
        help="run only the argument pass-through checks (no lspmux server needed)",
    )
    args = parser.parse_args()

    shim = Path(args.shim).expanduser()
    if not shim.is_file() or not os.access(shim, os.X_OK):
        print(f"lspmux-verify: no executable shim at {shim}", file=sys.stderr)
        return 2

    root = Path(
        args.workspace
        or subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    ).resolve()

    rep = Report()
    check_passthrough(rep, shim)

    if not args.passthrough_only:
        lspmux = shutil.which("lspmux")
        if lspmux is None:
            rep.fail("lspmux is installed", "not found on PATH")
        else:
            try:
                lspmux_status(lspmux)
            except Exception as exc:  # noqa: BLE001
                rep.fail(
                    "the lspmux server is running",
                    f"{exc} — start it with: systemctl --user start lspmux",
                )
            else:
                check_sharing(rep, shim, root, lspmux)

    if rep.failures == 0:
        print("lspmux-verify: GREEN")
        return 0
    print(f"lspmux-verify: {rep.failures} FAILED")
    return 1


if __name__ == "__main__":
    sys.exit(main())
