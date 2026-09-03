#!/usr/bin/env python3
"""Commit-message citation integrity: a cited hash must still be in history.

# The failure this catches

A commit body cites a sibling commit by short hash. Someone then amends or
rebases that sibling — for formatting, for a message fix, for anything — and
its hash changes. The citation now points at a commit that is not in the
branch, and NOTHING notices: the old object is still in the object database
until gc, so `git cat-file -e` happily confirms it exists.

That happened on 2026-09-03 in this repo, twice. Amending `fix(atlas)` for
rustfmt changed its hash, and three later commits plus a SYSTEM_OVERVIEW
paragraph went on citing the pre-amend one. Fixing THAT by rewriting the
messages changed those commits' hashes in turn, so the fix created a fresh
round of dangling citations pointing at the intermediate generation. Reaching a
fixpoint took three passes, and each one was caught by hand.

ARCH §11.1 is "cite, don't recall" — a citation that no longer resolves is the
same defect as one that was never true.

# Why ancestry, and not existence

`git cat-file -e <hash>` is the obvious check and it is the WRONG one: it
passes for exactly the commits this is meant to catch, because a rewritten
commit's object survives until gc. The question is not "does this object
exist" but "is this commit in the history I am publishing", which is
`git merge-base --is-ancestor`.

# Why an unresolvable token is a warning and not a failure

Note ids in this repo are 8 hex characters — the same shape as a short git
hash (`note 81feaf78`). A gate that fails on every one of those would cry wolf
on ordinary commit bodies, and a gate with a false-positive rate nobody has
measured is how people learn to reach for `--no-verify` (AGENTS.md, on the
advisory ratchets). So the rule is narrow and has essentially no false
positives:

    FAIL  the token resolves to a commit object, and that commit is NOT an
          ancestor of HEAD  — i.e. it was certainly meant as a commit
          reference, and it is certainly dangling
    WARN  the token resolves to nothing — probably a note id, possibly a
          typo; reported so a human can look, never enough to block

Usage:
    scripts/citation-check.py [<range>]     default: origin/main..HEAD
    scripts/citation-check.py --self-test   prove the checker can fail
"""

from __future__ import annotations

import re
import subprocess
import sys
import tempfile
from pathlib import Path

# 7-40 hex chars, at least one digit. The digit requirement drops English
# words that are accidentally hex ("deadbeef", "effaced"); a real short hash
# with no digit at all is possible but runs about 1 in 700 at 9 characters,
# and the cost of missing one is a stale citation rather than a wrong build.
TOKEN = re.compile(r"\b(?=[0-9a-f]*[0-9])[0-9a-f]{7,40}\b")

# `note 81feaf78` / `notes `81feaf78`` — note ids share the hash shape, and
# they are not git objects. Excluded by the word in front of them.
NOTE_PREFIXED = re.compile(r"\bnotes?\s+`?([0-9a-f]{7,40})\b", re.IGNORECASE)


def git(*args: str, cwd: Path | None = None) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=True
    ).stdout


def is_commit(h: str, cwd: Path | None = None) -> bool:
    return (
        subprocess.run(
            ["git", "cat-file", "-e", f"{h}^{{commit}}"],
            cwd=cwd,
            capture_output=True,
        ).returncode
        == 0
    )


def in_history(h: str, tip: str = "HEAD", cwd: Path | None = None) -> bool:
    return (
        subprocess.run(
            ["git", "merge-base", "--is-ancestor", h, tip],
            cwd=cwd,
            capture_output=True,
        ).returncode
        == 0
    )


def check(rev_range: str, cwd: Path | None = None) -> tuple[list, list]:
    """Returns (failures, warnings) as (commit, token) pairs."""
    revs = git("rev-list", rev_range, cwd=cwd).split()
    failures, warnings = [], []
    for rev in revs:
        body = git("show", "-s", "--format=%B", rev, cwd=cwd)
        excluded = set(NOTE_PREFIXED.findall(body))
        for tok in set(TOKEN.findall(body)):
            if tok in excluded or any(tok.startswith(e) or e.startswith(tok) for e in excluded):
                continue
            if rev.startswith(tok):  # a commit citing itself is impossible; skip self-prefix
                continue
            if not is_commit(tok, cwd=cwd):
                warnings.append((rev, tok))
            elif not in_history(tok, "HEAD", cwd=cwd):
                failures.append((rev, tok))
    return failures, warnings


def self_test() -> int:
    """Build a repo whose history has a dangling citation, and prove we see it.

    A check with no failing input you can name is not a check (ARCH §18.1).
    This constructs the exact shape that fooled `cat-file -e`: a commit whose
    object still exists but which is no longer an ancestor.
    """
    with tempfile.TemporaryDirectory() as td:
        d = Path(td)
        git("init", "-q", "-b", "main", str(d))
        git("config", "user.email", "t@t", cwd=d)
        git("config", "user.name", "t", cwd=d)

        (d / "a.txt").write_text("a\n")
        git("add", ".", cwd=d)
        git("commit", "-qm", "first", cwd=d)
        first = git("rev-parse", "--short=9", "HEAD", cwd=d).strip()

        (d / "b.txt").write_text("b\n")
        git("add", ".", cwd=d)
        # Three token shapes in one body, each with a different correct verdict:
        #   {first}     a real ancestor          -> silent
        #   note 81fe.. declared as a note id    -> silent (excluded by the prefix)
        #   deadbee12   bare and unresolvable    -> WARN, never a failure
        git(
            "commit", "-qm",
            f"second\n\nbuilds on {first}, cites note 81feaf78, and mentions deadbee12",
            cwd=d,
        )
        second_msg = git("show", "-s", "--format=%B", "HEAD", cwd=d)

        # Clean state: the citation resolves.
        fails, warns = check("HEAD~1..HEAD", cwd=d)
        if fails:
            print(f"SELF-TEST FAILED: clean history reported {fails}")
            return 1
        if not any(t == "deadbee12" for _, t in warns):
            print(f"SELF-TEST FAILED: a bare unresolvable token should WARN (got {warns})")
            return 1
        if any(t.startswith("81feaf78") for _, t in warns):
            print("SELF-TEST FAILED: a note-prefixed id must be silent, not warned")
            return 1

        # Now amend `first` so its hash changes, and replay `second` on top.
        # The old object survives — which is exactly why `cat-file -e` is not
        # enough — but it is no longer an ancestor.
        git("reset", "-q", "--hard", "HEAD~1", cwd=d)
        git("commit", "-q", "--amend", "-m", "first (amended)", cwd=d)
        tree = git("rev-parse", "HEAD^{tree}", cwd=d).strip()
        Path(d / "msg").write_text(second_msg)
        new = subprocess.run(
            ["git", "commit-tree", tree, "-p", "HEAD", "-F", str(d / "msg")],
            cwd=d, capture_output=True, text=True, check=True,
        ).stdout.strip()
        git("update-ref", "refs/heads/main", new, cwd=d)
        git("reset", "-q", "--hard", "main", cwd=d)

        if not is_commit(first, cwd=d):
            print("SELF-TEST INCONCLUSIVE: the orphaned object was already gc'd")
            return 1

        fails, warns = check("HEAD~1..HEAD", cwd=d)
        if not any(t == first for _, t in fails):
            print(f"SELF-TEST FAILED: dangling citation {first} NOT caught (got {fails})")
            return 1
        if any(t.startswith("81feaf78") or t == "deadbee12" for _, t in fails):
            print("SELF-TEST FAILED: a note id or unresolvable token was reported as a FAILURE — the cry-wolf case")
            return 1

        print(
            f"self-test ok: dangling citation {first} caught as a FAILURE; "
            "note id silent; bare unresolvable token warned, not failed"
        )
        return 0


def main() -> int:
    args = sys.argv[1:]
    if args and args[0] == "--self-test":
        return self_test()
    rev_range = args[0] if args else "origin/main..HEAD"
    try:
        failures, warnings = check(rev_range)
    except subprocess.CalledProcessError as e:
        print(f"citation-check: cannot read {rev_range}: {e.stderr.strip()}")
        return 0  # no range to judge is not a failure — ARCH §18.2, never-ran
    for rev, tok in warnings:
        print(f"  warn  {rev[:9]} cites {tok} — resolves to nothing (a note id?)")
    for rev, tok in failures:
        print(f"  FAIL  {rev[:9]} cites {tok} — that commit is NOT in this history")
    if failures:
        print(
            f"\ncitation-check: {len(failures)} dangling citation(s) in {rev_range}.\n"
            "A cited commit was amended or rebased after it was cited, so the hash\n"
            "no longer names anything in this branch (ARCH §11.1 — cite, don't recall).\n"
            "Fix: rewrite the citing messages to the CURRENT hashes. Do it in one\n"
            "forward pass oldest-first, substituting each rewritten commit's new hash\n"
            "as you go — otherwise the fix changes hashes again and re-dangles them."
        )
        return 1
    print(f"citation-check: clean ({len(warnings)} note-shaped token(s) warned) in {rev_range}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
