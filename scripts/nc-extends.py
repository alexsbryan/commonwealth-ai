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


# ─── The tool axis: PER-TOOL impls, not the one shared adapter ──────────────
#
# RE-SPECIFIED 2026-08-21 by nc-18, on the seat's ruling. The old `axis_tool`
# was a bare `rg("impl Tool for")` passing only at ZERO, with no allowance for
# the target shape — so the ONE generic adapter that every declared tool routes
# through counted against the axis it exists to close.
#
# WHY THE ALLOWANCE IS NOT A LOWERED BAR. `axis_intent` below already has the
# identical exemption and documents it in its own code: `sites = len(scattered)
# + max(0, len(tables) - 1)`, "One table is the target shape; every extra one is
# a second decider." Two axes answering the same question ("is adding one of
# these a DATA change?") under two different rules is two deciders for one
# question (ARCH principle 8). This makes them one rule.
#
# WHY ZERO WAS UNREACHABLE BY CONSTRUCTION, which is the same defect the
# `studio/` line in SKIP corrects: the `Tool` trait cannot be deleted. `studio/`
# carries 30 more `impl Tool for` on the SAME `sovereign_contracts::traits::Tool`
# and is excluded from this axis by campaign scope, while
# `sovereign-tools/Cargo.toml` takes a NON-optional dep on a studio crate — so
# the registry must keep accepting `dyn Tool`, and something must adapt a
# manifest row to it. The one route to a literal zero (registry holds
# `Arc<DeclaredTool>`, studio bridged in) was refused ON THE MERITS: it buys one
# deleted impl by creating a SECOND way to be a tool, plus a synthesised-manifest
# bridge. That is principle 8 backwards, and contorting code to move a grep is
# what this campaign exists to stop.
#
# BOTH SPECS ON THE UNCHANGED TREE at `51178383` (ARCH §18.6 — a judge change
# reported only in the direction it was meant to fix is the smell):
#     old spec (every `impl Tool for`)      82   FAIL
#     new spec (per-tool impls only)        81   FAIL
# The re-spec does not flip the axis and does not make it reachable without the
# work: 81 bespoke impls still have to go.
#
# WHAT IS EXEMPT, deliberately narrow (the seat's condition 4 — the exemption
# must not become the new hatch): an impl whose `descriptor()` names NO string
# literal and whose block reads a manifest it HOLDS. Identity from data, not
# baked into the block. The 55 per-tool impls that call
# `tool_manifest::require("some_literal_id")` are manifest-BACKED but not
# generic — their id is in the block, which is what makes them per-tool — and
# they get nothing. A hand-rolled generic adapter with a `descriptor` field and
# no manifest gets nothing either.
#
# AND AT MOST ONE, exactly as at most one `IntentRow` table is free. A tree of
# hand-written tools that merely share a base type still counts every impl but
# one, so the spec cannot pass the tree the ruling's kill bar names.

#: A `descriptor()` that names its own id is a PER-TOOL impl, however it reads
#: the manifest. This is the anti-hatch: to be exempt a block must actually be
#: generic over a held manifest, which IS the target shape.
_ID_LITERAL = re.compile(r'"[^"]*"')
#: Identity taken from a manifest the value HOLDS.
_HELD_MANIFEST = re.compile(r"\bself\.manifest\b")


def _scan_tool_impls(lines):
    """Every `impl Tool for` block in `lines`, classified. Pure — no I/O.

    Split out so `--self-test` can drive it on planted snippets: a classifier
    only ever exercised on the live tree is one nobody has watched fail
    (ARCH §18.1).
    """
    found, i = [], 0
    while i < len(lines):
        head = code_part(lines[i])
        m = re.search(r"impl Tool for (\w+)", head)
        if not m:
            i += 1
            continue
        depth, j, opened, body = 0, i, False, []
        while j < len(lines):
            c = code_part(lines[j])
            for ch in c:
                if ch == "{":
                    depth += 1
                    opened = True
                elif ch == "}":
                    depth -= 1
            body.append(c)
            if opened and depth <= 0:
                break
            j += 1
        # `descriptor()`'s own body, where the identity decision is visible.
        desc, d, o, seen = [], 0, False, False
        for c in body:
            if not seen and re.search(r"\bfn\s+descriptor\s*\(", c):
                seen = True
            if seen:
                for ch in c:
                    if ch == "{":
                        d += 1
                        o = True
                    elif ch == "}":
                        d -= 1
                desc.append(c)
                if o and d <= 0:
                    break
        desc = "\n".join(desc)
        found.append({
            "line": i + 1,
            "ty": m.group(1),
            "generic": seen and not _ID_LITERAL.search(desc),
            "held_manifest": bool(_HELD_MANIFEST.search("\n".join(body))),
        })
        i = j + 1
    return found


def _is_adapter(block):
    """THE exemption test. One implementation, and everything routes through it.

    The first draft of this re-spec had two — one here and one in the
    `--self-test` helper — so three deliberate breaks of the axis all passed
    the fixtures that were supposed to catch them. Two implementations of one
    threshold is ARCH principle 8, and it voided the check silently. Do not
    inline this predicate anywhere.
    """
    return block["generic"] and block["held_manifest"]


def _tool_axis_sites(total, adapters):
    """THE axis arithmetic: every block, minus ONE exemption.

    One implementation, shared by [`axis_tool`] and the fixtures, for the
    reason given on [`_is_adapter`].
    """
    return total - min(1, len(adapters))


def _tool_adapters(files):
    """The generic manifest-backed adapters across `files`."""
    out = []
    for path in files:
        with open(path, encoding="utf-8", errors="replace") as fh:
            lines = fh.read().splitlines()
        for b in _scan_tool_impls(lines):
            if _is_adapter(b):
                out.append(dict(b, file=path))
    return out


def axis_tool():
    """Adding a tool: does it need a new PER-TOOL `impl Tool for` block?

    Zero means the declared half of every tool is a manifest row and the
    executable half is an ordinary function — so a new tool is a row plus a
    handler. The one shared adapter does not count against the axis; a SECOND
    one does, because two adapters is two deciders (principle 8).
    """
    n, files = rg("impl Tool for")
    adapters = _tool_adapters(files)
    # One adapter is the target shape; every extra one is a second decider.
    sites = _tool_axis_sites(n, adapters)
    exempt = {(a["file"], a["line"]) for a in adapters[:1]}
    hot = sorted({f for f in files
                  if any(a["file"] == f for a in adapters[1:])
                  or not any(a["file"] == f for a in adapters)})
    if sites == 0:
        note = ("every tool is a manifest row plus a handler; one shared "
                "`DeclaredTool` adapter carries the trait")
    else:
        note = (f"{sites} per-tool trait impls, not rows "
                f"({len(adapters)} generic adapter(s) found)")
    _ = exempt
    return sites, hot, note


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
#     new spec (per-intent policy blocks)  14   FAIL
# The re-spec does not flip the axis, and does not make it easier to pass.
# (The new-spec figure read 13 for the first hour of its life: the scanner
# keyed on the `Intent::` prefix and could not see a file that glob-imports
# the variants. That was a FALSE PASS and is fixed below — see `_scan_blocks`.)

_ARM_HEAD = re.compile(r"^\s*(?:\|\s*)?(?:Self|Intent|[\w:]+::Intent)::\w")
#: `use crate::types::Intent::*;` — after this, arm heads are BARE variant
#: names and the qualified pattern above cannot see them. Found 2026-08-20 in
#: `runtime/authority_guard.rs`, whose `guard_story` is a thirteen-variant
#: policy table the axis was reading as absent. That is a FALSE PASS, the one
#: direction a zero-passes bar must never fail in, so the scanner learns the
#: variant names rather than trusting the prefix.
_GLOB_IMPORT = re.compile(r"\buse\s+[\w:]*Intent::\*\s*;")
_ENUM_DECL = re.compile(r"^\s*pub enum Intent\b")
_BARE_ARM = re.compile(r"^\s*(?:\|\s*)?([A-Z]\w*)\s*(?:[,{(|]|=>|$)")
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


def _intent_variants():
    """The `Intent` variant names, read off the enum declaration itself.

    Derived rather than hardcoded so a new variant is covered the day it
    lands — a bar carrying its own stale copy of the closed set is the same
    defect the rung it scores exists to remove.
    """
    names = set()
    for path in subprocess.run(
            ["git", "grep", "-lI", "-e", "pub enum Intent", "--", "*.rs"],
            capture_output=True, text=True).stdout.splitlines():
        if any(sk in path for sk in SKIP):
            continue
        with open(path, encoding="utf-8", errors="replace") as fh:
            lines = fh.read().splitlines()
        for i, line in enumerate(lines):
            if not _ENUM_DECL.match(code_part(line)):
                continue
            depth = 0
            for body in lines[i:]:
                c = code_part(body)
                before = depth
                depth += c.count("{") - c.count("}")
                if before == 1:
                    m = _BARE_ARM.match(c)
                    if m:
                        names.add(m.group(1))
                if before >= 1 and depth <= 0:
                    break
    return names


def _intent_policy_blocks():
    """Every `match` over an `Intent`, classified.

    Returns dicts with `variants` (distinct variant names in arm-head
    position), `dispatch` (an arm body awaits — control flow, not a table),
    and `builds_row` (the arms construct the intent table's row type).

    Line-based rather than a real parser, deliberately: the bar must run in
    seconds off a clean checkout with no toolchain, and every judgement it
    makes is visible in one screen of code.

    THAT TRADE HAS ALREADY COST ONCE, so the limits are named rather than
    waved at. The first cut assumed every limit ran in the OVER-counting
    direction and therefore could never manufacture a pass. It could: keying
    on the `Intent::` prefix made a glob-importing file invisible, and the axis
    read PASSING with a live policy site in it. The remaining known limits —
    a file-wide rather than scope-wide glob check, and a `//` inside a string
    literal truncating a line — do over-count. That is an argument for
    `--self-test`, not for trusting the shape of the error.
    """
    found = []
    variants = _intent_variants()
    for path in subprocess.run(
            ["git", "grep", "-lI", "-e", "Intent::", "--", "*.rs"],
            capture_output=True, text=True).stdout.splitlines():
        if any(sk in path for sk in SKIP):
            continue
        with open(path, encoding="utf-8", errors="replace") as fh:
            lines = fh.read().splitlines()
        for block in _scan_blocks(lines, variants):
            found.append(dict(block, file=path))
    return found


def _scan_blocks(lines, variants=frozenset()):
    """The judgement itself, over a list of source lines. Pure — no I/O.

    Split out from [`_intent_policy_blocks`] so `--self-test` can drive it on
    planted snippets. A classifier only ever exercised on the live tree is a
    classifier nobody has watched fail (ARCH §18.1).
    """
    # `Self::Foo` is an Intent arm only inside `impl Intent`.
    self_is_intent = any("impl Intent" in line for line in lines)
    # `use ..Intent::*;` makes arm heads BARE variant names. Checked
    # file-wide rather than per-scope: that over-counts if one function globs
    # the variants and another matches an unrelated enum, and over-counting is
    # the only direction a bar that passes at zero may err in.
    globbed = bool(variants) and any(
        _GLOB_IMPORT.search(code_part(line)) for line in lines)
    found, i = [], 0
    while i < len(lines):
        head = code_part(lines[i])
        if _MATCH_KW.search(head) and head.rstrip().endswith("{"):
            depth, j, named, bodies, opened = 0, i, [], [], False
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
                            named += _VARIANT.findall(line.split("=>")[0])
                    elif outer == 1 and globbed and _BARE_ARM.match(line):
                        head = line.split("=>")[0]
                        named += [w for w in re.findall(r"\b([A-Z]\w*)", head)
                                  if w in variants]
                if opened and depth <= 0:
                    break
                j += 1
            if named:
                found.append({
                    "line": i + 1,
                    "variants": sorted(set(named)),
                    "dispatch": any(".await" in b for b in bodies),
                    "builds_row": any(_ROW_TYPE in b for b in bodies),
                })
                i = j
        i += 1
    return found


def _classify_tool(snippet):
    """`adapter` / `per-tool` / `none` for one planted `impl Tool for` block."""
    blocks = _scan_tool_impls(snippet.splitlines())
    if not blocks:
        return "none"
    return "adapter" if _is_adapter(blocks[0]) else "per-tool"


def _tool_sites(snippet):
    """The axis's own arithmetic over planted blocks — same two functions the
    live axis calls, so a break in either is visible here."""
    blocks = _scan_tool_impls(snippet.splitlines())
    return _tool_axis_sites(len(blocks), [b for b in blocks if _is_adapter(b)])


def _classify(snippet, variants=frozenset()):
    """`policy` / `dispatch` / `guard` / `none` for one planted snippet."""
    blocks = _scan_blocks(snippet.splitlines(), variants)
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

    # ── The glob import (ARCH §18.1, and this one was a live FALSE PASS) ────
    #
    # `use crate::types::Intent::*;` makes every arm head a BARE variant name,
    # invisible to a scanner that keys on the `Intent::` prefix. Found
    # 2026-08-20 in `runtime/authority_guard.rs`, whose `guard_story` is a
    # thirteen-variant policy table the axis was reading as ABSENT — it
    # reported the intent axis PASSING while a policy site was live. That is
    # the one direction a bar passing only at zero must never fail in, so the
    # scanner learns the variant names off the enum declaration and this
    # fixture is what proves it still does.
    globbed = ("use crate::types::Intent::*;\n"
               "match intent {\n"
               "    KnowledgeQuery | ComparisonQuery => GuardStory::Covered(\"kq\"),\n"
               "    DeepQuery | SimpleQuery => GuardStory::Covered(\"deep\"),\n"
               "    CodeQuery => GuardStory::NoOpByConstruction(\"code corpora\"),\n"
               "}\n")
    if _classify(globbed, _intent_variants()) != "policy":
        bad.append("a glob-imported policy table read as absent — FALSE PASS")
    # Without the variant set it is invisible: that is the bug, restated as a
    # fixture, so a refactor that stops threading the names through fails here.
    if _classify(globbed) != "none":
        bad.append("the glob fixture no longer needs the variant set to be seen")

    # ── The tool axis's one-adapter allowance (ARCH §18.1, §18.6) ──────────
    #
    # The exemption is the whole risk of the 2026-08-21 re-spec: if it can be
    # claimed by a hand-written tool it becomes the hatch that voids the axis.
    # Both directions get a named failing input, because a check with no
    # failing input you can name is not a check.
    ADAPTER = ("impl Tool for DeclaredTool {\n"
               "    fn descriptor(&self) -> ToolDescriptor {\n"
               "        self.manifest.to_descriptor()\n"
               "    }\n"
               "}\n")
    tool_cases = [
        ("adapter", "generic, identity from a HELD manifest — the target shape",
         ADAPTER),
        ("per-tool", "manifest-BACKED but names its own id: the 55-impl shape, "
                     "which the allowance must NOT cover",
         "impl Tool for GetLintOutputTool {\n"
         "    fn descriptor(&self) -> ToolDescriptor {\n"
         "        tool_manifest::require(\"get_lint_output\").to_descriptor()\n"
         "    }\n"
         "}\n"),
        ("per-tool", "HOLDS a manifest but still names its own id — isolates "
                     "the anti-hatch, which the no-manifest case masks",
         "impl Tool for GetLintOutputTool {\n"
         "    fn descriptor(&self) -> ToolDescriptor {\n"
         "        self.manifest.to_descriptor_for(\"get_lint_output\")\n"
         "    }\n"
         "}\n"),
        ("per-tool", "hand-rolled generic adapter with NO manifest — the seat's "
                     "condition 4: no manifest, no exemption",
         "impl Tool for GenericTool {\n"
         "    fn descriptor(&self) -> ToolDescriptor {\n"
         "        self.descriptor.clone()\n"
         "    }\n"
         "}\n"),
        ("per-tool", "a descriptor literal — the pre-nc-13 shape",
         "impl Tool for ExtractTool {\n"
         "    fn descriptor(&self) -> ToolDescriptor {\n"
         "        ToolDescriptor { id: \"extract\".into(), name: \"Extract\".into() }\n"
         "    }\n"
         "}\n"),
    ]
    for want, why, snippet in tool_cases:
        got = _classify_tool(snippet)
        if got != want:
            bad.append(f"tool axis — {why}: want {want}, got {got}")

    # ONE exemption, not one per adapter. A second adapter is a second decider
    # and counts, exactly as a second `IntentRow` table does.
    two = ADAPTER + ADAPTER.replace("DeclaredTool", "OtherDeclaredTool")
    if _tool_sites(two) != 1:
        bad.append(f"a SECOND adapter went unexempted: want 1 site, "
                   f"got {_tool_sites(two)}")

    # THE RULING'S KILL BAR, as a fixture: a tree where tools are still
    # hand-written but happen to share a base type must NOT pass. None of these
    # is generic, so none is exempt and every one counts.
    shared_base = "".join(
        f"impl Tool for {n}Tool {{\n"
        f"    fn descriptor(&self) -> ToolDescriptor {{\n"
        f"        self.base.descriptor(\"{n.lower()}\")\n"
        f"    }}\n"
        f"}}\n" for n in ("Alpha", "Beta", "Gamma"))
    if _tool_sites(shared_base) != 3:
        bad.append(f"hand-written tools sharing a base type were exempted: "
                   f"want 3 sites, got {_tool_sites(shared_base)}")

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
          f"{len(cases)} intent shapes classified, table recognised, "
          f"{len(tool_cases)} tool shapes classified, one-adapter allowance "
          f"held against a second adapter and a shared base type.")
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
