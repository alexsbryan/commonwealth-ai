"""engine-swap's NO-ORPHANS check: no llama-server or ggml-rpc-server is running
without a sovereign-stock above it. A deployed daemon's own router (after
HUMAN-es-host-flip) is not an orphan; a server a SIGKILLed daemon left behind is.
Exit 1 names the pids. Watched failing on a stray process named llama-server
(2026-10-12)."""
import os
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


left = [p for p in filter(str.isdigit, os.listdir("/proc"))
        if stat(p)[0] in SERVERS and not under_a_daemon(p)]
print("orphans", left or "none")
sys.exit(1 if left else 0)
