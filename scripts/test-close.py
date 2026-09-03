#!/usr/bin/env python3
"""Close ledger records: write `landed` naming the test that was WATCHED red.

    scripts/test-close.py GR-05=<junit classname>::<test path> IN-10=...

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
import re, subprocess, sys

SPECS = "quality/conformance-specs.toml"
BACKLOG = "quality/tests/backlog.toml"


def test_exists(key):
    """The junit key's last segment must be a test fn that is really there."""
    name = key.split("::")[-1].strip()
    if not name:
        return False
    # POSIX ERE: git grep -E has no \s. This guard silently refused every
    # legitimate close until that was caught (ARCH §18.4 — validate the
    # instrument before the result).
    r = subprocess.run(["git", "grep", "-lE",
                        rf"fn[[:space:]]+{re.escape(name)}[[:space:]]*\("],
                       capture_output=True, text=True)
    return r.returncode == 0 and bool(r.stdout.strip())


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
    open(BACKLOG, "w").write(t[:start] + blk.rstrip("\n") + f'\nlanded = "{test}"\n\n' + t[end:])
    return True


def main(argv):
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
