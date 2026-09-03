#!/usr/bin/env python3
"""Close ledger records: write `landed` naming the test that was WATCHED red.

    scripts/test-close.py GR-05=<junit classname>::<test path> IN-10=...
    scripts/test-close.py --block IN-01="needs real GGUF weights; no fixture"
    scripts/test-close.py --retarget GR-05=<file>::<symbol>

`landed` is the ONLY field scripts/test-queue.py reads to drop an item from the
queue, so this script is the burndown's single write path. It is admissible for
a mutation actually watched red on the test the record names — a `landed`
written from reading the tree is the opinion-versus-mutation trap
TEST_LEDGER.md §Closure names, wearing the burndown's own clothes.

Three refusals, because the loop that calls this is unattended:
  - an id in neither registry
  - a record that already carries `landed` (no silent overwrite)
  - a test name with no `fn <name>` anywhere in the tree, which is what a
    fabricated or mistyped close looks like. --force names the substitution.
"""
import os, re, subprocess, sys

SPECS = "quality/conformance-specs.toml"
BACKLOG = "quality/tests/backlog.toml"


def key_segments(key):
    """The individually checkable parts of a `landed` value.

    Both registries already spell a multi-test close as
    `a_test (+ ::another_test)`, and some name a playwright spec or a vitest
    file instead of a Rust fn. Splitting here means the guard validates the
    convention that is in use rather than forcing a worse one — the first
    version of this script refused `A (+ ::B)` outright, having parsed `B)` as
    the test name.
    """
    parts = re.split(r"[(),+]|\s+\+\s+", key)
    out = []
    for raw in parts:
        seg = raw.strip().strip(":").strip()
        if not seg or seg.startswith("@") or seg in ("*", "playwright", "vitest"):
            continue
        out.append(seg.split("::")[-1].strip() if "::" in seg else seg)
    return [s for s in out if s]


def test_exists(key):
    """Every checkable segment must name something really in the tree.

    A Rust segment must have a `fn <name>(`; a file-shaped one must be a
    tracked file. A key with no checkable segment at all is refused — an
    unrecognisable `landed` is exactly what a fabricated close looks like.
    """
    checked = False
    for seg in key_segments(key):
        if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", seg):
            # POSIX ERE: git grep -E has no \s. This guard silently refused
            # every legitimate close until that was caught (ARCH §18.4 —
            # validate the instrument before the result).
            # --untracked, because in the burndown loop the test file is
            # ALWAYS new: a plain `git grep` searches the index only and
            # refused every close on a test written this session.
            r = subprocess.run(
                ["git", "grep", "-lE", "--untracked",
                 rf"fn[[:space:]]+{re.escape(seg)}[[:space:]]*\("],
                capture_output=True, text=True)
            if r.returncode != 0 or not r.stdout.strip():
                return False
            checked = True
        elif "/" in seg and "." in seg:
            if not os.path.exists(seg):
                return False
            checked = True
    return checked


def close_spec(rid, test):
    blocks = open(SPECS).read().split("[[spec]]")
    out, hit = [blocks[0]], False
    for b in blocks[1:]:
        m = re.search(r'^id = "([^"]+)"$', b, re.M)
        if m and m.group(1) == rid:
            if "\nlanded = " in b:
                sys.exit(f"{rid}: already landed")
            b = b.replace('status = "exists-untagged"', 'status = "landed"', 1)
            b = b.rstrip("\n") + f"\nlanded = '''{test}'''\n\n"
            hit = True
        out.append(b)
    if not hit:
        return False
    open(SPECS, "w").write("[[spec]]".join(out))
    return True


def close_backlog(rid, test):
    t = open(BACKLOG).read()
    key = f'id = "{rid}"'
    if key not in t:
        return False
    start = t.index(key)
    nxt = t.find("[[test]]", start)
    end = nxt if nxt != -1 else len(t)
    blk = t[start:end]
    if "\nlanded = " in blk:
        sys.exit(f"{rid}: already landed")
    open(BACKLOG, "w").write(
        t[:start] + blk.rstrip("\n") + f"\nlanded = {toml_basic(test)}\n\n" + t[end:])
    return True


def toml_basic(s):
    """A TOML basic string, escaped.

    Learned the hard way: a `blocked` reason quoting a design doc contains
    double quotes, and writing it raw produced a backlog.toml that tomllib
    could not parse — which took the whole burndown VIEW down with it, not just
    the one record. The single write path escapes; nothing downstream should
    have to.
    """
    out = s.replace("\\", "\\\\").replace('"', '\\"')
    for raw, esc in (("\n", "\\n"), ("\r", "\\r"), ("\t", "\\t")):
        out = out.replace(raw, esc)
    return '"' + out + '"'


def block_backlog(rid, reason):
    """Name why a record cannot be adjudicated here. It leaves the write queue
    and stays visible as `blocked` — the alternative is a file --next hands out
    every iteration forever, which is how an unattended loop spins."""
    t = open(BACKLOG).read()
    key = f'id = "{rid}"'
    if key not in t:
        sys.exit(f"{rid}: not in the backlog")
    start = t.index(key)
    nxt = t.find("[[test]]", start)
    end = nxt if nxt != -1 else len(t)
    blk = t[start:end]
    if "\nblocked = " in blk:
        sys.exit(f"{rid}: already blocked")
    if "\nlanded = " in blk:
        sys.exit(f"{rid}: already landed — closed records are not blocked")
    open(BACKLOG, "w").write(
        t[:start] + blk.rstrip("\n") + f"\nblocked = {toml_basic(reason)}\n\n" + t[end:])
    print(f"blocked {rid}: {reason}")


def retarget_backlog(rid, target):
    """Correct a record's `target` when the failure's real site is elsewhere.
    The queue's unit of work is a FILE, so a wrong target files the record
    against the wrong order and it is never reached from the right one."""
    t = open(BACKLOG).read()
    key = f'id = "{rid}"'
    if key not in t:
        sys.exit(f"{rid}: not in the backlog")
    start = t.index(key)
    nxt = t.find("[[test]]", start)
    end = nxt if nxt != -1 else len(t)
    blk = t[start:end]
    m = re.search(r'^target = "([^"]*)"$', blk, re.M)
    if not m:
        sys.exit(f"{rid}: no `target` line to correct")
    open(BACKLOG, "w").write(
        t[:start] + blk[:m.start(1)] + target + blk[m.end(1):] + t[end:])
    print(f"retarget {rid}: {m.group(1)} -> {target}")


def main(argv):
    if argv and argv[0] == "--block":
        for arg in argv[1:]:
            rid, reason = arg.split("=", 1)
            block_backlog(rid, reason)
        return
    if argv and argv[0] == "--retarget":
        for arg in argv[1:]:
            rid, target = arg.split("=", 1)
            retarget_backlog(rid, target)
        return
    force = "--force" in argv
    args = [a for a in argv if a != "--force"]
    if not args:
        sys.exit(__doc__)
    for arg in args:
        if "=" not in arg:
            sys.exit(f"{arg}: expected <ID>=<junit-key>")
        rid, test = arg.split("=", 1)
        if not test_exists(test):
            if not force:
                sys.exit(f"{rid}: no `fn {test.split('::')[-1]}(` in the tree — "
                         f"a close names a test that RAN. --force to override.")
            print(f"SUBSTITUTION {rid}: closing on {test}, which git grep cannot find.")
        if close_spec(rid, test):
            print(f"spec   {rid} -> {test}")
        elif close_backlog(rid, test):
            print(f"record {rid} -> {test}")
        else:
            sys.exit(f"{rid}: not found in either registry")


if __name__ == "__main__":
    main(sys.argv[1:])
