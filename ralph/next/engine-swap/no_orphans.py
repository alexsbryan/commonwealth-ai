"""engine-swap's NO-ORPHANS check: no llama-server or ggml-rpc-server is running
without a sovereign-stock above it. A deployed daemon's own router (after
HUMAN-es-host-flip) is not an orphan; a server a SIGKILLed daemon left behind is.
Exit 1 names the pids. Watched failing on a stray process named llama-server
(2026-10-12). Linux reads /proc as it always did; a host without /proc (macOS)
reads one `ps` table instead, and a failed `ps` is refused (exit 2), never
read as no orphans."""
import os
import subprocess
import sys

SERVERS = ("llama-server", "ggml-rpc-server")


def stat(pid):
    """(command name, parent pid), or (None, 0) for a process that is gone."""
    try:
        s = open(f"/proc/{pid}/stat").read()
    except OSError:
        return None, 0
    return s[s.index("(") + 1:s.rindex(")")], int(s[s.rindex(")") + 2:].split()[1])


def under_a_daemon(pid):
    q = stat(pid)[1]
    while q > 1:
        name, parent = stat(q)
        if name == "sovereign-stock":
            return True
        q = parent
    return False


if os.path.isdir("/proc/self"):
    pids = list(filter(str.isdigit, os.listdir("/proc")))
else:
    ps = subprocess.run(["ps", "-axo", "pid=,ppid=,comm="], capture_output=True, text=True)
    if ps.returncode != 0:
        print(f"no-orphans: ps failed: {ps.stderr.strip()}")
        sys.exit(2)
    table = {}
    for line in ps.stdout.splitlines():
        pid, ppid, comm = line.split(None, 2)
        table[pid] = (os.path.basename(comm.strip()), int(ppid))

    def stat(pid):  # noqa: F811 — the ps table's answer to the /proc reader
        return table.get(str(pid), (None, 0))

    pids = list(table)

left = [p for p in pids if stat(p)[0] in SERVERS and not under_a_daemon(p)]
print("orphans", left or "none")
sys.exit(1 if left else 0)
