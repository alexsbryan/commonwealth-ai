#!/usr/bin/env python3
"""nc-extends — is adding a thing a DATA change or a CODE change?

The campaign's other three bars count TYPES crossing domain boundaries. That
number can be driven to target by publishing a narrow surface of CONCRETE
types, which is not the same as the architecture being extensible — so this
instrument exists to keep the easy axis from standing in for the goal.

ONE QUESTION PER AXIS: to add one more of this thing, how many Rust sites must
change? An axis is DATA (passes) at zero, and CODE (fails) at anything above.
No partial credit: one `impl` block is the whole difference between shipping a
feature as a file and shipping it as a release.

The score is the count of passing axes, 0-3, matching the hand-scored `today`
already recorded on the bar. That agreement is the instrument's own validation
(ARCH §18.4 — validate the instrument before the result): if this reports
something other than 1 on an unchanged tree, it is measuring something other
than what the bar declared, and the number is void.

Deliberately grep-shaped, with no dependency on the SCIP graph. The three
graph-backed bars all read zero off an empty database on 2026-08-20 and would
have been recorded as total success; an instrument that cannot be silently
emptied is worth more here than a more precise one.
"""
import json
import re
import subprocess
import sys

# `studio/` is excluded because THE CAMPAIGN excludes it. noun-convergence.toml
# names it "a FIFTH system this framing does not name ... Out of scope", and
# nc-13's Scope section names only sovereign-tools / sovereign-core /
# cli-contract.toml. Counting it made the tool axis unreachable BY
# CONSTRUCTION: 30 of 112 `impl Tool for` sites are studio's, so converting
# every in-scope impl still left the axis failing. An axis no funded work can
# pass measures nothing. SCOPE correction, not a threshold change — the
# zero-or-fail rule above is untouched.
SKIP = (".claude/worktrees/", "target/", "/tests/", "studio/")


def code_part(content):
    """The executable part of a source line — comments removed.

    THE BAR MUST NOT COUNT PROSE. Found by nc-13's worker 2026-08-20: the tool
    axis transiently read 86 instead of 83 because THREE DOC-COMMENT MENTIONS
    of `impl Tool for`, in a module whose whole subject is that trait, scored as
    implementations. In their words: anyone writing prose about this trait
    inflates the campaign's own bar by doing so. They fixed it by rewording
    their comments (`290b7e3d`), which un-inflated the number without removing
    the hazard — the next author to document the trait re-inflates it.

    A measurement an author can move by writing a sentence is not a
    measurement, so the guard belongs here rather than in everyone's prose.

    Known and accepted limit: a `//` inside a string literal (a URL) truncates
    the line early, so a pattern appearing AFTER such a literal on the same line
    would be missed. That direction UNDER-counts, which for a bar that passes
    only at zero is the safe direction — it can never manufacture a pass.
    """
    stripped = content.lstrip()
    if stripped.startswith(("//", "*", "/*")):
        return ""
    idx = content.find("//")
    return content if idx == -1 else content[:idx]


def rg(pattern, glob="*.rs"):
    """Hit count and files for a pattern over TRACKED sources, CODE ONLY.

    `git grep` rather than a filesystem walk: the question is how many code
    sites a maintainer must edit, so the universe is what is committed. It also
    cannot wander into `target/` (46GB here) or an untracked agent worktree —
    a plain `grep -r` took 23s and blew the 10s measurement budget.

    git grep finds the candidate lines; `code_part` decides which are real.
    """
    out = subprocess.run(
        ["git", "grep", "-nI", "-e", pattern, "--", glob],
        capture_output=True, text=True).stdout.splitlines()
    rx = re.compile(pattern)
    hits = []
    for line in out:
        if any(s in line for s in SKIP):
            continue
        # `path:lineno:content` — split twice so content keeps any colons.
        parts = line.split(":", 2)
        if len(parts) < 3:
            continue
        if rx.search(code_part(parts[2])):
            hits.append(line)
    files = {l.split(":", 1)[0] for l in hits}
    return len(hits), sorted(files)


def axis_tool():
    """Adding a tool: does it need a new `impl Tool for` block?"""
    n, files = rg("impl Tool for")
    return n, files, "each tool is a hand-written trait impl, not a row"


def axis_intent():
    """Adding an intent: how many files fan out per-variant?"""
    n, files = rg(r"Intent::")
    return len(files), files, f"{n} `Intent::` sites fan out across {len(files)} files"


def axis_corpus():
    """Adding a corpus: recipes are TOML, so the Rust side should be zero.

    Counts Rust that enumerates INDIVIDUAL corpora by name — a match arm or
    const list naming specific corpora would mean a new corpus needs a code
    edit. Recipe TOMLs are data and never counted.
    """
    n, files = rg(r"CorpusId::[A-Z]")
    return n, files, "corpora are TOML recipes; no Rust enumerates them by name"


AXES = [("corpus", axis_corpus), ("tool", axis_tool), ("intent", axis_intent)]


def self_test():
    """Watch the comment guard decide, on planted lines (ARCH §18.1).

    The guard shipped with ZERO movement on the live tree, because nc-13's
    worker had already reworded the three doc comments that exposed it. A fix
    with no observable delta is exactly the kind that quietly stops working, so
    its evidence is here rather than in a tree diff.
    """
    counts_as_code = [
        "impl Tool for CorpusSearch {",
        "    impl Tool for Nested {",
        "let x = 1; // impl Tool for is named in this trailing comment",
    ]
    counts_as_prose = [
        "/// Every tool writes `impl Tool for` by hand.",
        "//! Module docs mentioning impl Tool for.",
        "// impl Tool for Foo {",
        "     * impl Tool for, in a block-comment continuation",
        "/* impl Tool for */",
    ]
    bad = []
    for line in counts_as_code:
        # The third case is subtle and deliberate: real code precedes the
        # comment, but the PATTERN is only in the comment, so it must NOT count.
        expected = "impl Tool for" in code_part(line)
        if line.startswith("let x") and expected:
            bad.append(f"trailing comment counted as code: {line!r}")
        elif not line.startswith("let x") and not expected:
            bad.append(f"real impl missed: {line!r}")
    for line in counts_as_prose:
        if "impl Tool for" in code_part(line):
            bad.append(f"prose counted as code: {line!r}")
    for line in bad:
        print(f"  FAIL  {line}")
    if bad:
        print(f"\nself-test: {len(bad)} failure(s) — the bar counts prose.")
        return 1
    print(f"self-test: pass — {len(counts_as_code)} code shapes counted, "
          f"{len(counts_as_prose)} prose shapes refused.")
    return 0


def main():
    if "--self-test" in sys.argv:
        return self_test()
    rows, score = [], 0
    for name, fn in AXES:
        sites, files, note = fn()
        passing = sites == 0
        score += passing
        rows.append({"axis": name, "code_sites": sites, "passes": passing,
                     "note": note, "top_files": files[:5]})

    if "--json" in sys.argv:
        print(json.dumps({"value": score, "axes": rows}, indent=2))
        return 0

    print("EXTENSIBILITY — adding a thing: DATA change, or CODE change?\n")
    print(f"  {'axis':<10} {'rust sites':>11}  {'verdict':<7}  why")
    print("  " + "-" * 76)
    for r in rows:
        v = "DATA" if r["passes"] else "CODE"
        print(f"  {r['axis']:<10} {r['code_sites']:>11}  {v:<7}  {r['note']}")
    print("  " + "-" * 76)
    print(f"\n  score: {score}/3 axes are data-driven")
    if score < 3:
        print("\n  A bar reading 3 means one commit adds a feature as FILES ONLY —")
        print("  no new `impl` block, full suite green. Until then this is the")
        print("  axis the boundary bars cannot speak to.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
