#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Idle CPU of one process tree, read from /proc (phase-b pb-rails-idle-*).

The bars of phase-b-31 read "a 5-minute idle window after a 60 s settle, CPU
from /proc/<pid>/stat utime+stime summed over the process tree". This is the
one instrument both halves (cw-rails, the stock binary) take it with.

    idle-cpu.py --out DIR --label L [--settle 60] [--window 300] -- CMD...
    idle-cpu.py --out DIR --label L --pid PID      # an already-running tree

Each reading appends one JSON line to DIR/readings.jsonl, and a label already
there is skipped, so a session that ends mid-row resumes without re-taking it.
A tree that dies inside the window is recorded as `died`, never as a number.
"""
import argparse, json, os, signal, subprocess, sys, time

TCK = os.sysconf("SC_CLK_TCK")


def stat(path):
    # comm may hold spaces; the fields after the last ')' are positional.
    raw = open(path).read()
    rest = raw[raw.rindex(")") + 2:].split()
    return {"ppid": int(rest[1]), "ticks": int(rest[11]) + int(rest[12])}


def tree(root):
    kids = {}
    for p in os.listdir("/proc"):
        if p.isdigit():
            try:
                kids.setdefault(stat(f"/proc/{p}/stat")["ppid"], []).append(int(p))
            except (OSError, ValueError):
                pass
    out, todo = [], [root]
    while todo:
        p = todo.pop()
        out.append(p)
        todo.extend(kids.get(p, []))
    return out


def snapshot(root):
    """Tree ticks, plus per-thread ticks keyed (pid, tid, comm)."""
    total, threads = 0, {}
    for p in tree(root):
        try:
            total += stat(f"/proc/{p}/stat")["ticks"]
            for t in os.listdir(f"/proc/{p}/task"):
                comm = open(f"/proc/{p}/task/{t}/comm").read().strip()
                threads[(p, int(t), comm)] = stat(f"/proc/{p}/task/{t}/stat")["ticks"]
        except OSError:
            pass
    return total, threads


def alive(pid):
    try:
        return stat(f"/proc/{pid}/stat") is not None and open(f"/proc/{pid}/status").read().find("State:\tZ") < 0
    except OSError:
        return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--label", required=True)
    ap.add_argument("--settle", type=float, default=60)
    ap.add_argument("--window", type=float, default=300)
    ap.add_argument("--pid", type=int)
    ap.add_argument("cmd", nargs="*")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    ledger = os.path.join(a.out, "readings.jsonl")
    if os.path.exists(ledger):
        for line in open(ledger):
            if json.loads(line).get("label") == a.label:
                print(f"idle-cpu: {a.label} already read, skipping: {line.strip()}")
                return 0
    child = None
    if a.pid:
        pid = a.pid
    else:
        if not a.cmd:
            ap.error("give --pid or a command after --")
        log = open(os.path.join(a.out, f"{a.label}.proc.log"), "w")
        child = subprocess.Popen(a.cmd, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        pid = child.pid
    print(f"idle-cpu: {a.label} pid={pid} settle={a.settle}s window={a.window}s", flush=True)
    time.sleep(a.settle)
    rec = {"label": a.label, "pid": pid, "settle_s": a.settle, "window_s": a.window,
           "cmd": a.cmd or open(f"/proc/{pid}/cmdline").read().replace("\0", " ").strip()}
    if not alive(pid):
        rec["verdict"] = "died"
    else:
        t0, th0 = snapshot(pid)
        w0 = time.monotonic()
        time.sleep(a.window)
        t1, th1 = snapshot(pid)
        wall = time.monotonic() - w0
        if not alive(pid):
            rec["verdict"] = "died"
        else:
            hot = sorted(((th1[k] - th0.get(k, 0), k) for k in th1), reverse=True)[:5]
            rec.update(verdict="read", wall_s=round(wall, 1), ticks=t1 - t0,
                       pct_core=round(100.0 * (t1 - t0) / TCK / wall, 3),
                       hot_threads=[{"tid": k[1], "comm": k[2], "ticks": d} for d, k in hot])
    if child:
        os.killpg(child.pid, signal.SIGTERM)
        try:
            child.wait(10)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
    with open(ledger, "a") as f:
        f.write(json.dumps(rec) + "\n")
    print(json.dumps(rec))
    return 0 if rec["verdict"] == "read" else 1


if __name__ == "__main__":
    sys.exit(main())
