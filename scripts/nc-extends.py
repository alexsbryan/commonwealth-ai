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


# ─── The intent axis: POLICY sites, not mentions ────────────────────────────
#
# RE-SPECIFIED 2026-08-20 by nc-14. The old `axis_intent` counted FILES
# containing `Intent::` and passed only at ZERO — meaning no Rust anywhere may
# name an intent variant. nc-14's order says the opposite and is right:
# "matching on a CLOSED enum is what enums are FOR ... pattern matches stay."
# Driving the old number to zero means erasing variant naming from the
# codebase: worse architecture bought to flip a number.
#
# WHAT IS COUNTED NOW: a per-intent POLICY block — one `match` over an `Intent`
# that derives a per-variant ATTRIBUTE. Adding a 14th intent forces an edit at
# every one of them, which is exactly the bar's one question ("to add one more
# of this thing, how many Rust sites must change?").
#
# WHAT IS REFUSED: construction (`let i = Intent::CodeQuery`), guards
# (`matches!(intent, Intent::ComparisonQuery)`), and HANDLER DISPATCH
# (`match intent { Intent::CodeQuery => self.handle_code_query(..).await, .. }`)
# — control flow over a closed set, which the order protects by name.
#
# THE GAMING VECTOR THIS SPEC CLOSES, stated because the obvious alternative
# has it: spec the axis on EXHAUSTIVENESS (count matches with no `_` arm) and
# anyone can zero it by adding `_ =>` fallback arms — trading a compile error
# for a silent default, which is ARCH principle 6 backwards and the precise
# disease this order names. Three live sites already have that shape and are
# still counted here: `speed_for_retrieval_intent`'s `_ => Speed::Slow`,
# `resolve_output_budget`'s `_ => 700`, and `operation_of`'s `_ => None`.
#
# BOTH HALVES ON THE UNCHANGED TREE (ARCH §18.6 — a judge change reported only
# in the direction it was meant to fix is the smell):
#     old spec (files naming `Intent::`)   33   FAIL
#     new spec (per-intent policy blocks)  13   FAIL
# The re-spec does not flip the axis, and does not make it easier to pass.

_ARM_HEAD = re.compile(r"^\s*(?:\|\s*)?(?:Self|Intent|[\w:]+::Intent)::\w")
_VARIANT = re.compile(r"(?:Self|Intent|[\w:]+::Intent)::(\w+)")
_MATCH_KW = re.compile(r"\bmatch\b")

#: Below this many DISTINCT variants named in arm heads, a `match` is a guard
#: or a special case, not a table. Mirrors ARCH §2.1's own smell threshold
#: ("a `match` on string ids with more than 3 arms"). Counted as VARIANTS and
#: not as arm lines, so `A | B | C | D => ..` on one line is four, not one.
_TABLE_ARITY = 3

#: The row type of the one intent table. A block whose arms build this IS the
#: table; a SECOND one is a second decider (ARCH principle 8) and is counted.
_ROW_TYPE = "IntentRow"


def _intent_policy_blocks():
    """Every `match` over an `Intent`, classified.

    Returns dicts with `variants` (distinct variant names in arm-head
    position), `dispatch` (an arm body awaits — control flow, not a table),
    and `builds_row` (the arms construct the intent table's row type).

    Line-based rather than a real parser, deliberately: the bar must run in
    seconds off a clean checkout with no toolchain, and every judgement it
    makes is visible in one screen of code. Its two known limits both
    OVER-count, which for a bar that passes only at zero is the safe
    direction — it can never manufacture a pass.
    """
    found = []
    for path in subprocess.run(
            ["git", "grep", "-lI", "-e", "Intent::", "--", "*.rs"],
            capture_output=True, text=True).stdout.splitlines():
        if any(s in path for s in SKIP):
            continue
        with open(path, encoding="utf-8", errors="replace") as fh:
            lines = fh.read().splitlines()
        for block in _scan_blocks(lines):
            found.append(dict(block, file=path))
    return found


def _scan_blocks(lines):
    """The judgement itself, over a list of source lines. Pure — no I/O.

    Split out from [`_intent_policy_blocks`] so `--self-test` can drive it on
    planted snippets. A classifier only ever exercised on the live tree is a
    classifier nobody has watched fail (ARCH §18.1).
    """
    # `Self::Foo` is an Intent arm only inside `impl Intent`.
    self_is_intent = any("impl Intent" in line for line in lines)
    found, i = [], 0
    while i < len(lines):
        head = code_part(lines[i])
        if _MATCH_KW.search(head) and head.rstrip().endswith("{"):
            depth, j, variants, bodies, opened = 0, i, [], [], False
            while j < len(lines):
                line = code_part(lines[j])
                outer = depth
                for ch in line:
                    if ch == "{":
                        depth += 1
                        opened = True
                    elif ch == "}":
                        depth -= 1
                if j > i:
                    bodies.append(line)
                    # Arm heads sit one level inside the match block.
                    if outer == 1 and _ARM_HEAD.match(line):
                        is_self = line.lstrip().lstrip("| ").startswith("Self::")
                        if self_is_intent or not is_self:
                            variants += _VARIANT.findall(line.split("=>")[0])
                if opened and depth <= 0:
                    break
                j += 1
            if variants:
                found.append({
                    "line": i + 1,
                    "variants": sorted(set(variants)),
                    "dispatch": any(".await" in b for b in bodies),
                    "builds_row": any(_ROW_TYPE in b for b in bodies),
                })
                i = j
        i += 1
    return found


def _classify(snippet):
    """`policy` / `dispatch` / `guard` / `none` for one planted snippet."""
    blocks = _scan_blocks(snippet.splitlines())
    if not blocks:
        return "none"
    b = blocks[0]
    if b["dispatch"]:
        return "dispatch"
    return "policy" if len(b["variants"]) >= _TABLE_ARITY else "guard"


def axis_intent():
    """Adding an intent: how many per-intent POLICY sites must change?

    Zero means every per-variant attribute is a COLUMN in one table, so a new
    intent is a ROW. The table itself does not count against the axis — but a
    SECOND table does, because two tables is two deciders (principle 8).
    """
    policy = [b for b in _intent_policy_blocks()
              if not b["dispatch"] and len(b["variants"]) >= _TABLE_ARITY]
    tables = [b for b in policy if b["builds_row"]]
    scattered = [b for b in policy if not b["builds_row"]]
    # One table is the target shape; every extra one is a second decider.
    sites = len(scattered) + max(0, len(tables) - 1)
    files = sorted({b["file"] for b in scattered + tables[1:]})
    if not scattered and len(tables) == 1:
        note = f"every per-intent attribute is a column in the one `{_ROW_TYPE}` table"
    else:
        note = (f"{len(scattered)} per-intent policy matches outside a "
                f"`{_ROW_TYPE}` table ({len(tables)} table(s) found)")
    return sites, files, note


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

    # ── The intent discriminator, on planted snippets (ARCH §18.1) ──────────
    #
    # WHAT IT MUST SEPARATE, and the failing input for each direction:
    #   POLICY   a per-variant ATTRIBUTE table. Counted.
    #   dispatch a per-variant HANDLER CALL. Refused — this is what a closed
    #            enum is FOR, and nc-14's order protects it by name.
    #   guard    fewer than `_TABLE_ARITY` variants. Refused.
    #   none     a construction or a `matches!` predicate. Refused.
    #
    # The `_ =>` case is the load-bearing one: it is the construct that would
    # pass an EXHAUSTIVENESS-based spec and must NOT pass this one, or the axis
    # can be zeroed by trading compile errors for silent defaults.
    cases = [
        ("policy", "attribute table",
         "match intent {\n"
         "    Intent::SimpleQuery => 400,\n"
         "    Intent::DeepQuery => 1200,\n"
         "    Intent::CodeQuery => 900,\n"
         "}\n"),
        ("policy", "attribute table WITH a `_` fallback arm — still counted",
         "match intent {\n"
         "    Intent::SimpleQuery => Speed::Fast,\n"
         "    Intent::DeepQuery => Speed::Slow,\n"
         "    Intent::ComparisonQuery => Speed::Fast,\n"
         "    _ => Speed::Slow,\n"
         "}\n"),
        ("policy", "one arm, four variants — variants are counted, not arms",
         "match intent {\n"
         "    Intent::SimpleQuery | Intent::KnowledgeQuery\n"
         "    | Intent::DeepQuery | Intent::CodeQuery => Some(Operation::Answer),\n"
         "    _ => None,\n"
         "}\n"),
        ("dispatch", "per-variant handler call",
         "match intent {\n"
         "    Intent::CodeQuery => self.handle_code_query(m).await,\n"
         "    Intent::ComplexTask => self.handle_complex_task(m).await,\n"
         "    Intent::ExpressiveQuery => self.handle_expressive_query(m).await,\n"
         "    _ => self.handle_simple(m).await,\n"
         "}\n"),
        ("guard", "two variants is a special case, not a table",
         "match intent {\n"
         "    Intent::KnowledgeQuery | Intent::SimpleQuery => Intent::DeepQuery,\n"
         "    other => other,\n"
         "}\n"),
        ("none", "construction site",
         "let intent = Intent::KnowledgeQuery;\n"),
        ("none", "predicate",
         "let wide = matches!(intent, Intent::ComparisonQuery | Intent::DeepQuery);\n"),
        ("none", "an array of variants in a test",
         "let intents = [Intent::DeepQuery, Intent::CodeQuery, Intent::SimpleQuery];\n"),
    ]
    for want, why, snippet in cases:
        got = _classify(snippet)
        if got != want:
            bad.append(f"{why}: want {want}, got {got}")

    # The table itself must be recognised, and a SECOND one must not be.
    table = ("match self {\n"
             "    Self::SimpleQuery => IntentRow { slug: \"simple_query\" },\n"
             "    Self::DeepQuery => IntentRow { slug: \"deep_query\" },\n"
             "    Self::CodeQuery => IntentRow { slug: \"code_query\" },\n"
             "}\n")
    blocks = _scan_blocks(("impl Intent {\n" + table + "}\n").splitlines())
    if not (blocks and blocks[0]["builds_row"]):
        bad.append("the `IntentRow` table was not recognised as the table")

    for line in bad:
        print(f"  FAIL  {line}")
    if bad:
        print(f"\nself-test: {len(bad)} failure(s) — the bar is not measuring "
              f"what it says it measures.")
        return 1
    print(f"self-test: pass — {len(counts_as_code)} code shapes counted, "
          f"{len(counts_as_prose)} prose shapes refused, "
          f"{len(cases)} intent shapes classified, table recognised.")
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
