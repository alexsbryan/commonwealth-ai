#!/usr/bin/env python3
"""hpr-cost — how many places must change to add ONE flag, summed over the five
frozen specimens of the `hot-path-reuse` campaign.

WHAT THE NUMBER IS. The distinct places an author must type in to add ONE flag,
summed over the five frozen specimens. THE PLACES ARE COUNTED FROM THE SOURCE,
never assumed. The order this instrument was written against said "hand-rolled
= 2, derived = 1"; hpr-1 then measured the three converted specimens BY HAND and
got 4, 3 and 2. A constant of 2 would have scored `router_fit_cmd.rs` as a 2->1
win when the truth is 4->1 — understating the result by half, and mis-scoring
every future file. That is the same class of error (an instrument measuring
something adjacent to its thesis) that this campaign exists to stop, so the
constant is gone and `count_sites` does the work.

The place kinds, each somewhere an author types when adding one flag:

  struct field         the declaration. Always present.
  match arm            the flag-dispatch arm. Hand-rolled surfaces only.
  per-field local      `let mut <field>` accumulators in the parse function.
  constructor literal  an `S { field, .. }` naming the fields one by one.
  Default impl         a HAND-WRITTEN `impl Default for S`. A `#[derive(Default)]`
                       needs no edit and is therefore not a place.

Measured at HEAD 63c72af8 this gives router_fit 4 · main 4 · vault_report 2 ·
all 3 · notes_retrieval 3 = 16, so the toml's floor of 10 (5 x 2) is low and the
seat corrects it from this reading. A converted surface counts 1: the field.

Floor and target are READ FROM `quality/campaigns/hot-path-reuse.toml`, never
restated here — one decider, one name (§10.6).

WHY THIS QUANTITY AND NOT LINES. The campaign's first formulation of
`hpr-cheaper` counted TOTAL LINES of the flag surface. The pilot conversion of
`vault_report.rs` moved it 53 -> 48 — five lines — because a 43-line parse loop
was replaced by ~18 lines of `#[arg(long)]` attributes. The line count was
measuring verbosity, which is adjacent to the thesis and not the thesis. This
quantity is ungameable by verbosity: attribute style cannot move it, and the
only way to reach 1 is to make the declaration the single source.

DETECTION, and what it refuses to do. Comments and doc comments are stripped
before anything is matched (a file that merely *mentions* `#[derive(clap::Parser)]`
in a comment is not a converted file — `vault_report.rs` has two such comments
today, and an unstripped scan counts them). Then:

  derived     a `#[derive(..Parser..)]` attached to a struct declaration -> 1
  hand-rolled >= 2 match arms whose pattern is a bare long-flag literal
              (`"--foo" =>`, `"--foo" | "-f" =>`) -> 2
  BOTH        `mixed` — NOT scored. Which surface an added flag lands on is
              undetermined, so the honest verdict is could-not-judge.
  NEITHER     `unknown` — NOT scored.

`mixed` and `unknown` are ABSENCE, and absence is reported, never defaulted
(ARCH §18.3, §18.2). Such a file is named on stderr and the script exits 3,
printing no value. It is not scored 1, it is not scored 2, and it is not
averaged away.

PRECONDITION (else exit 3, ARCH §7/§18.2): at least one specimen is converted
and compiles.
  - "converted" is checked structurally: at least one specimen classifies
    `derived`.
  - "compiles" is checked by PROXY ONLY and the proxy is named in the output:
    the crate owning each derived specimen declares a `clap` dependency, without
    which the derive cannot build. This script does NOT run cargo — the bar's
    timeout is 30s. The real compile proof is the workspace lint gate
    (`./scripts/sovereign-lint.sh --human --full`), which compiles every
    specimen; do not read this proxy as a build.

GATE ZERO — `scripts/hpr-cost.py --gate-zero`, and it lives here on purpose.
A control run once and written up in a report rots; a control that ships inside
the instrument is re-runnable by whoever next doubts the number (principle 10:
make it structural, not remembered). It runs both:

  POSITIVE  three independent before/after pairs with DIFFERENT pre-registered
            answers, which is what makes this a control rather than a
            coincidence. hpr-1 hand-counted HEAD as router_fit 4, all 3,
            vault_report 2 (`FIXTURE_HEAD_COST`) before this counter existed.
            Each specimen is scored at HEAD — it must equal its fixture — and
            then swapped back to HEAD in an otherwise-current tree, where the
            total must rise by exactly `fixture - 1`. Deltas of 3, 2 and 1 from
            one counter cannot be produced by a constant. A specimen already
            derived at HEAD is skipped and NAMED.
  NEGATIVE  score a scratch copy of all five specimens carrying a rename
            (`Opts`/`Args` -> `CmdOptions`/`CmdArgs`), a reformat (derive
            attributes exploded across lines, every indent doubled) and
            adversarial comments that quote BOTH shapes (`#[derive(clap::Parser)]`
            in a `//` comment, `"--fake" =>` in a `//` and a `/* */` comment).
            Predicted: the value does not move at all.

The negative control is not decoration. `vault_report.rs` carries two comments
that quote `#[derive(clap::Parser)]`, and a scan that did not strip comments
would have reported it converted before the pilot ever ran.

GATE ZERO FLAGS. `--swap`, `--rev` and `--root` are the plumbing `--gate-zero`
drives, exposed so a control can be reproduced by hand. They are NOT part of the
bar's instrument line (`scripts/hpr-cost.py --json`), and any run that uses one
stamps `"gate_zero": true` in its JSON so a control value can never be mistaken
for a bar value.

  --swap PATH=REV   read just this specimen at REV, everything else from the
                    working tree. This is the isolated positive control: the
                    pilot is UNCOMMITTED, so there is no commit pair to diff,
                    and a whole-tree `--rev HEAD` comparison would also pick up
                    any conversion a concurrent worker landed. Swapping ONE
                    specimen back to HEAD isolates exactly one conversion.
  --rev REV         read every specimen at REV.
  --root DIR        read every specimen from DIR/<repo-relative-path>. This is
                    the negative control: renamed/reformatted copies in a
                    scratch tree, scored by this same code, with no repo file
                    touched.

Exit codes (co-lineage instrument contract): 0 value valid · 2 usage ·
3 precondition unmet or a specimen's shape is absent (named on stderr, NO value
printed) · 4 environment (git unavailable, specimen unreadable).
`--gate-zero` exits 0 only when BOTH controls pass, 1 when either fails, and 3
when a control cannot be CONSTRUCTED (which is a could-not-judge, not a pass).
"""
from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The five specimens, FROZEN at HEAD 63c72af8 by quality/campaigns/hot-path-reuse.toml.
# Do not add to this list — the denominator is the bar's floor (5 x 2 = 10) and a
# sixth specimen would silently redefine it.
SPECIMENS = (
    "sovereign/crates/sovereign-cli-llm/src/router_fit_cmd.rs",
    "sovereign/crates/sovereign-cli/src/main.rs",
    "sovereign/crates/sovereign-cli-llm/src/bench_cmd/vault_report.rs",
    "sovereign/crates/sovereign-cli-llm/src/bench_cmd/all.rs",
    "sovereign/crates/sovereign-cli/src/notes_retrieval_cmd.rs",
)

# PRE-REGISTERED GROUND TRUTH (§18.1 — the falsifier exists before the data).
# hpr-1 counted these BY HAND at HEAD 63c72af8, before this counter was written,
# and the seat relayed them as a correction to an earlier estimate of "2 for
# every hand-rolled file". A counter that cannot reproduce 4 / 3 / 2 here is
# guessing, not counting, and gate zero fails rather than shipping a number.
FIXTURE_HEAD_COST = {
    "sovereign/crates/sovereign-cli-llm/src/router_fit_cmd.rs": 4,
    "sovereign/crates/sovereign-cli-llm/src/bench_cmd/all.rs": 3,
    "sovereign/crates/sovereign-cli-llm/src/bench_cmd/vault_report.rs": 2,
}
DERIVED_COST = 1        # the converted form: a struct field, and nothing else

# A match arm whose pattern is one or more bare long-flag literals.
# `Some("--help") =>` (subcommand dispatch) deliberately does NOT match: the
# pattern must start at the arm, not be wrapped in a constructor.
FLAG_ARM = re.compile(
    r'^[ \t]*"--[A-Za-z0-9][A-Za-z0-9_-]*"'
    r'(?:\s*\|\s*"-{1,2}[A-Za-z0-9][A-Za-z0-9_-]*")*'
    r'\s*(?:if\s[^=]*)?=>', re.M)
MIN_FLAG_ARMS = 2                            # one `"--help" =>` arm is not a parse loop
DERIVE_AT = re.compile(r"#\[\s*derive\s*\(")
PARSER_TOKEN = re.compile(r"(?:^|[^A-Za-z0-9_:])(?:clap::)?Parser(?![A-Za-z0-9_])")
STRUCT_DECL = re.compile(r"\A(?:pub\s*(?:\([^)]*\)\s*)?)?struct\s+([A-Za-z_][A-Za-z0-9_]*)")
STRUCT_ANY = re.compile(r"\bstruct\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^>{]*>)?\s*\{")
FIELD_DECL = re.compile(r"^\s*(?:pub\s*(?:\([^)]*\)\s*)?)?([a-z_][A-Za-z0-9_]*)\s*:", re.M)
TOP_FN = re.compile(r"^(?:pub\s*(?:\([^)]*\)\s*)?)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)", re.M)
CLAP_DEP = re.compile(r"^\s*clap\s*(?:\.[A-Za-z_-]+)?\s*=", re.M)


CAMPAIGN_TOML = REPO / "quality" / "campaigns" / "hot-path-reuse.toml"
BAR_ID = "hpr-cheaper"


class Absent(Exception):
    """A shape or an input that must be reported, never defaulted (§18.3)."""


def thresholds() -> tuple[float | None, float | None]:
    """floor/target from the campaign file. Restating them here would be a
    second definition of one threshold (§10.6) and would go stale the moment
    the seat corrects the floor this instrument is about to falsify."""
    try:
        doc = tomllib.loads(CAMPAIGN_TOML.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError):
        return None, None
    for b in doc.get("bar", []):
        if b.get("id") == BAR_ID:
            return b.get("floor"), b.get("target")
    return None, None


# --------------------------------------------------------------------------
# Rust lexing — only as much as the two shapes need
# --------------------------------------------------------------------------


def strip_comments(src: str) -> tuple[str, list[bool]]:
    """-> (text with comments blanked to spaces, per-char 'inside a string' mask).

    Line count and every column are preserved, so reported line numbers are the
    file's own. Comments are blanked rather than removed because a comment that
    quotes the treatment (`// the flag surface became #[derive(clap::Parser)]`)
    must not read as the treatment — two such comments exist in `vault_report.rs`
    right now, and counting them would report a converted file that is not.
    """
    out: list[str] = []
    in_str: list[bool] = []
    i, n = 0, len(src)
    depth = 0                                # block-comment nesting (Rust nests)

    def emit(text: str, is_string: bool) -> None:
        out.append(text)
        in_str.extend([is_string] * len(text))

    while i < n:
        c = src[i]
        if depth:                            # inside /* ... */
            if src.startswith("/*", i):
                depth += 1
                emit("  ", False)
                i += 2
            elif src.startswith("*/", i):
                depth -= 1
                emit("  ", False)
                i += 2
            else:
                emit("\n" if c == "\n" else " ", False)
                i += 1
            continue
        if src.startswith("//", i):          # line comment, incl. /// and //!
            j = src.find("\n", i)
            j = n if j < 0 else j
            emit(" " * (j - i), False)
            i = j
            continue
        if src.startswith("/*", i):
            depth = 1
            emit("  ", False)
            i += 2
            continue
        if c == "r" and (m := re.match(r'r(#*)"', src[i:])):
            close = '"' + m.group(1)         # raw string: r"..", r#".."#
            j = src.find(close, i + len(m.group(0)))
            j = n if j < 0 else j + len(close)
            emit(src[i:j], True)
            i = j
            continue
        if c == '"':
            j = i + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == '"':
                    j += 1
                    break
                j += 1
            emit(src[i:j], True)
            i = j
            continue
        if c == "'" and (m := re.match(r"'(?:\\.|[^\\'])'", src[i:])):
            emit(m.group(0), True)           # char literal, not a lifetime
            i += len(m.group(0))
            continue
        emit(c, False)
        i += 1
    return "".join(out), in_str


def _skip_bracketed(text: str, mask: list[bool], i: int, open_c: str, close_c: str) -> int:
    """Index just past a balanced bracket run starting at text[i] == open_c."""
    depth = 0
    while i < len(text):
        if not mask[i]:
            if text[i] == open_c:
                depth += 1
            elif text[i] == close_c:
                depth -= 1
                if depth == 0:
                    return i + 1
        i += 1
    return len(text)


# --------------------------------------------------------------------------
# the two shapes
# --------------------------------------------------------------------------


def find_derived(text: str, mask: list[bool]) -> tuple[str, int] | None:
    """-> (struct name, 1-based line) for a `#[derive(..Parser..)]` that is
    attached to a struct declaration, or None."""
    for m in DERIVE_AT.finditer(text):
        if mask[m.start()]:
            continue
        end = _skip_bracketed(text, mask, m.start() + 1, "[", "]")
        if not PARSER_TOKEN.search(text[m.start():end]):
            continue
        # Skip any further attributes (`#[command(...)]`, `#[arg(...)]`) and
        # blank space, then require a struct declaration. A `Parser` derive on
        # an enum is a subcommand table, not a flag surface.
        j = end
        while j < len(text):
            while j < len(text) and text[j].isspace():
                j += 1
            if j < len(text) and text[j] == "#" and not mask[j]:
                j = _skip_bracketed(text, mask, j + 1, "[", "]")
                continue
            break
        sm = STRUCT_DECL.match(text[j:j + 200])
        if sm:
            return sm.group(1), text.count("\n", 0, m.start()) + 1
    return None


def find_flag_arms(text: str) -> list[dict]:
    """Match arms that dispatch on a long flag, with their offsets."""
    return [{"offset": m.start(), "line": text.count("\n", 0, m.start()) + 1}
            for m in FLAG_ARM.finditer(text)]


def struct_decls(text: str, mask: list[bool]) -> dict[str, dict]:
    """name -> {line, fields}. Field names are what makes a per-field local or a
    field-listing literal recognisable as a PLACE rather than as any old code."""
    out: dict[str, dict] = {}
    for m in STRUCT_ANY.finditer(text):
        if mask[m.start()]:
            continue
        brace = text.index("{", m.start())
        end = _skip_bracketed(text, mask, brace, "{", "}")
        body = text[brace + 1:end - 1]
        fields = [f for f in FIELD_DECL.findall(body)]
        out.setdefault(m.group(1), {"line": text.count("\n", 0, m.start()) + 1,
                                    "fields": fields, "start": m.start(),
                                    "end": end})
    return out


def top_level_fns(text: str, mask: list[bool]) -> list[dict]:
    """Column-0 functions only: a `fn parse(...)` nested in `mod tests` is not
    the command's parse path and must not be mistaken for it."""
    out = []
    for m in TOP_FN.finditer(text):
        if mask[m.start()] or (m.start() and text[m.start() - 1] not in "\n"):
            continue
        try:
            lp = text.index("(", m.end())
        except ValueError:
            continue
        rp = _skip_bracketed(text, mask, lp, "(", ")")
        brace = text.find("{", rp)
        if brace < 0:
            continue
        body_end = _skip_bracketed(text, mask, brace, "{", "}")
        out.append({"name": m.group(1), "sig": text[m.start():brace],
                    "start": m.start(), "body": (brace, body_end)})
    return out


def _returns(sig: str, name: str) -> bool:
    """Does this signature RETURN the struct? `fn f(o: &Opts) -> Bench` does
    not, and counting it would attach the parse sites to the wrong function."""
    _, arrow, ret = sig.partition("->")
    return bool(arrow) and re.search(r"\b" + re.escape(name) + r"\b", ret) is not None


def count_sites(text: str, mask: list[bool], shape: str,
                arms: list[dict]) -> tuple[str | None, list[str]]:
    """-> (flag-surface struct name, the distinct PLACES adding one flag touches).

    NOT A CONSTANT. The order this instrument was written against asserted
    "hand-rolled = 2"; hpr-1 measured 4, 3 and 2 across three specimens. A
    constant that fits one file understates a 4->1 conversion by half, which is
    the same class of error — an instrument measuring something adjacent to its
    thesis — that this whole campaign exists to stop. So the places are counted
    from the source, every time.

    The place kinds, each of which is somewhere an author must type when adding
    one flag:
      struct field        the declaration itself. Always present.
      match arm           the flag-dispatch arm. Hand-rolled surfaces only.
      per-field local     `let mut <field>` accumulators in the parse function.
      constructor literal a `S { field, .. }` that names the fields one by one.
      Default impl        a HAND-WRITTEN `impl Default for S` (a `#[derive(
                          Default)]` needs no edit and is not a place).
    """
    decls = struct_decls(text, mask)
    fns = top_level_fns(text, mask)
    name = None
    if shape == "derived":
        d = find_derived(text, mask)
        name = d[0] if d else None
    elif arms:
        first = arms[0]["offset"]
        holders = [f for f in fns if f["body"][0] <= first < f["body"][1]]
        if holders:
            sig = holders[-1]["sig"]
            for cand in decls:
                if _returns(sig, cand):
                    name = cand
                    break
    if name is None or name not in decls:
        return None, []

    fields = set(decls[name]["fields"])
    sites = ["struct field"]
    if shape == "hand-rolled":
        sites.append("match arm")

    parse_fn = next((f for f in fns if _returns(f["sig"], name)), None)
    if parse_fn:
        body = text[parse_fn["body"][0]:parse_fn["body"][1]]
        locals_ = [v for v in re.findall(r"\blet\s+mut\s+([a-z_][A-Za-z0-9_]*)", body)
                   if v in fields]
        if len(locals_) >= 2:
            sites.append(f"per-field local ({len(locals_)} in {parse_fn['name']})")
        for m in re.finditer(r"\b" + re.escape(name) + r"\s*\{", body):
            lit = body[m.end():_skip_bracketed(body, [False] * len(body),
                                               m.end() - 1, "{", "}")]
            named = [f for f in fields
                     if re.search(r"(?:^|[{,\s])" + re.escape(f) + r"\s*[,:}]", lit)]
            if len(named) >= 2:
                sites.append(f"constructor literal ({len(named)} fields "
                             f"in {parse_fn['name']})")
                break

    if re.search(r"\bimpl\s+Default\s+for\s+" + re.escape(name) + r"\b", text):
        sites.append("hand-written impl Default")
    return name, sites


def classify(src: str, strip: bool = True) -> dict:
    """`strip=False` exists ONLY for gate zero's vacuity check: it shows what
    the same detector reports when comments are left in, which is how we know
    the comment-stripping in the negative control is load-bearing rather than
    decorative (§18.1 — name the input that makes it red, then watch it)."""
    text, mask = strip_comments(src) if strip else (src, [False] * len(src))
    derived = find_derived(text, mask)
    arms = find_flag_arms(text)
    handrolled = len(arms) >= MIN_FLAG_ARMS
    if derived and handrolled:
        shape = "mixed"
    elif derived:
        shape = "derived"
    elif handrolled:
        shape = "hand-rolled"
    else:
        shape = "unknown"

    struct, sites = (count_sites(text, mask, shape, arms)
                     if shape in ("derived", "hand-rolled") else (None, []))
    return {
        "shape": shape,
        "cost": len(sites) if sites else None,
        "sites": sites,
        "struct": struct,
        "derive_line": derived[1] if derived else None,
        "flag_arms": len(arms),
        "flag_arm_lines": [a["line"] for a in arms[:12]],
    }


# --------------------------------------------------------------------------
# specimen sources
# --------------------------------------------------------------------------


def _git(*argv: str) -> str:
    proc = subprocess.run(["git", *argv], cwd=REPO, capture_output=True, text=True)
    if proc.returncode != 0:
        raise Absent(f"git {' '.join(argv)} exited {proc.returncode}: {proc.stderr.strip()}")
    return proc.stdout


def read_specimen(rel: str, rev: str | None, root: Path | None) -> tuple[str, str]:
    """-> (source text, provenance label)."""
    if root is not None:
        p = root / rel
        if not p.is_file():
            raise Absent(f"{rel}: not present under --root {root}")
        return p.read_text(encoding="utf-8", errors="replace"), f"root:{root}"
    if rev is not None:
        return _git("show", f"{rev}:{rel}"), f"rev:{rev}"
    p = REPO / rel
    if not p.is_file():
        raise Absent(f"{rel}: specimen missing from the working tree")
    return p.read_text(encoding="utf-8", errors="replace"), "worktree"


def owning_crate(rel: str) -> Path | None:
    d = (REPO / rel).parent
    while d != REPO and d != d.parent:
        if (d / "Cargo.toml").is_file():
            return d
        d = d.parent
    return None


# --------------------------------------------------------------------------
# scoring — one decider, shared by the bar path and by gate zero (§10.6)
# --------------------------------------------------------------------------


def score(rev: str | None = None, root: Path | None = None,
          swaps: dict[str, str] | None = None) -> tuple[list[dict], list[str]]:
    """-> (rows, problems). A non-empty `problems` means NO value may be
    reported: either a specimen's shape is absent, or the precondition is
    unmet. Both are exit 3 for the caller."""
    swaps = swaps or {}
    rows = []
    for rel in SPECIMENS:
        src, prov = read_specimen(rel, swaps.get(rel, rev),
                                  None if rel in swaps else root)
        row = classify(src)
        row["path"] = rel
        row["source"] = prov
        rows.append(row)

    problems: list[str] = []
    for r in rows:
        if r["cost"] is None:
            problems.append(
                f"{r['path']}: shape is {r['shape']!r} — matches "
                + ("BOTH the derived and the hand-rolled shape"
                   if r["shape"] == "mixed" else "NEITHER shape")
                + f" (derive={r['struct']}, flag arms={r['flag_arms']}). NOT scored.")

    derived = [r for r in rows if r["shape"] == "derived"]
    if not derived:
        problems.append("PRECONDITION UNMET: no specimen is converted — none of "
                        "the five carries a `#[derive(..Parser..)]` flag surface, "
                        "so there is no treatment to measure.")
    for r in derived:
        # The "compiles" half of the precondition, by PROXY and named as one.
        # Under --root there is no crate to inspect; the proxy is skipped and
        # said to be skipped rather than assumed satisfied.
        if root is not None:
            continue
        crate = owning_crate(r["path"])
        if crate is None or not CLAP_DEP.search(
                (crate / "Cargo.toml").read_text(encoding="utf-8", errors="replace")):
            problems.append(f"PRECONDITION UNMET: {r['path']} is derived but its "
                            f"crate declares no `clap` dependency — the derive "
                            f"cannot compile.")
    return rows, problems


def total(rows: list[dict]) -> float:
    return float(sum(r["cost"] for r in rows))


# --------------------------------------------------------------------------
# gate zero
# --------------------------------------------------------------------------


def _mutate(src: str) -> str:
    """A rename + a reformat + comments that quote BOTH shapes. Nothing here
    changes how many places an added flag touches, so nothing here may move the
    value."""
    out = re.sub(r"\bOpts\b", "CmdOptions", src)
    out = re.sub(r"\bArgs\b", "CmdArgs", out)
    out = re.sub(r"#\[derive\(([^()]*)\)\]",
                 lambda m: "#[derive(\n    "
                           + ",\n    ".join(p.strip() for p in m.group(1).split(","))
                           + ",\n)]", out)
    out = "\n".join(re.sub(r"^(\s+)", lambda m: m.group(1) * 2, ln)
                    for ln in out.splitlines())
    return (
        "// gate zero, negative control. The next three lines quote both shapes\n"
        "// in COMMENTS and must therefore change nothing:\n"
        "//   #[derive(clap::Parser, Debug)] struct Opts { }\n"
        '//   "--fake" => { unreachable!() }\n'
        '/*  "--other" | "-o" => {}   and   #[derive(Parser)]  */\n'
        + out + "\n\n// trailing reformat\n")


def gate_zero(out=sys.stdout) -> int:
    p = lambda s="": print(s, file=out)  # noqa: E731
    p("\n  hpr-cost — GATE ZERO\n")

    base_rows, base_problems = score()
    if base_problems:
        for x in base_problems:
            print(f"hpr-cost: {x}", file=sys.stderr)
        print("hpr-cost: gate zero CANNOT BE CONSTRUCTED — the working tree does "
              "not produce a value to control against", file=sys.stderr)
        return 3
    base = total(base_rows)
    p(f"  baseline (working tree)                 value = {base:g}")

    # ---- POSITIVE: swap one converted specimen back to HEAD ---------------
    p("\n  POSITIVE — three before/after pairs with DIFFERENT pre-registered")
    p("  answers (hpr-1 hand-counted HEAD before this counter was written).")
    p("  A constant cannot produce deltas of 3, 2 and 1.\n")
    p(f"  {'specimen':<26} {'HEAD':>5} {'fix':>4} {'now':>4} {'d pred':>7} "
      f"{'d obs':>6}  result")
    converted = [r for r in base_rows if r["shape"] == "derived"]
    if not converted:
        print("hpr-cost: no converted specimen in the working tree — the positive "
              "control CANNOT BE CONSTRUCTED", file=sys.stderr)
        return 3
    pos_ok, pos_ran, skipped, unregistered = True, 0, [], []
    for r in converted:
        rel = r["path"]
        head_rows, head_problems = score(swaps={rel: "HEAD"})
        head_row = next(x for x in head_rows if x["path"] == rel)
        if head_row["shape"] == "derived":
            skipped.append(rel)
            continue
        fixture = FIXTURE_HEAD_COST.get(rel)
        if fixture is None:
            unregistered.append(f"{Path(rel).name} (HEAD cost {head_row['cost']}, "
                                f"no pre-registered answer)")
            continue
        if head_problems:
            p(f"  {Path(rel).name[:26]:<26} {'—':>5} {fixture:>4} {r['cost']:>4} "
              f"{'—':>7} {'—':>6}  COULD-NOT-JUDGE ({head_problems[0]})")
            pos_ok = False
            continue
        obs_total = total(head_rows)
        pred_delta = fixture - DERIVED_COST
        obs_delta = obs_total - base
        ok = (head_row["cost"] == fixture and r["cost"] == DERIVED_COST
              and abs(obs_delta - pred_delta) < 1e-9)
        pos_ok &= ok
        pos_ran += 1
        p(f"  {Path(rel).name[:26]:<26} {head_row['cost']:>5} {fixture:>4} "
          f"{r['cost']:>4} {pred_delta:>7} {obs_delta:>6g}  "
          f"{'PASS' if ok else 'FAIL'}")
    for rel in skipped:
        p(f"  {Path(rel).name[:26]:<26} {'—':>5} {'—':>4} {'—':>4} {'—':>7} "
          f"{'—':>6}  SKIPPED (already derived at HEAD)")
    for u in unregistered:
        p(f"  {u}  NOT SCORED — outside the pre-registered fixture, so it can")
        p("      confirm nothing; add it to FIXTURE_HEAD_COST with a hand count.")
    if pos_ran == 0:
        print("hpr-cost: no converted specimen has a pre-registered HEAD count — "
              "the positive control CANNOT BE CONSTRUCTED", file=sys.stderr)
        return 3

    # ---- NEGATIVE: rename + reformat + adversarial comments ---------------
    p("\n  NEGATIVE — rename + reformat + comments quoting both shapes.")
    p("  Applied to scratch copies; no repo file is touched.\n")
    scratch = Path(tempfile.mkdtemp(prefix="hpr-cost-gate0-"))
    try:
        for rel in SPECIMENS:
            dst = scratch / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            dst.write_text(_mutate((REPO / rel).read_text(encoding="utf-8",
                                                          errors="replace")),
                           encoding="utf-8")
        neg_rows, neg_problems = score(root=scratch)
        if neg_problems:
            p(f"  {'mutated copies of all five':<58} {base:>5g} {'—':>5}  FAIL "
              f"({neg_problems[0]})")
            neg_ok = False
        else:
            obs = total(neg_rows)
            neg_ok = abs(obs - base) < 1e-9
            p(f"  {'mutated copies of all five':<58} {base:>5g} {obs:>5g}  "
              f"{'PASS' if neg_ok else 'FAIL'}")
            for r in neg_rows:
                p(f"      {r['cost']}  {r['shape']:<12} {Path(r['path']).name}")

        # VACUITY CHECK (§18.1). A negative control that could not have gone red
        # proves nothing. Re-classify the same mutated files with comments LEFT
        # IN: the planted comments must flip at least one specimen's shape. If
        # they do not, the control never had teeth and its PASS is worthless.
        flipped = []
        for rel in SPECIMENS:
            mutated = (scratch / rel).read_text(encoding="utf-8", errors="replace")
            was = classify(mutated, strip=True)["shape"]
            now = classify(mutated, strip=False)["shape"]
            if was != now:
                flipped.append(f"{Path(rel).name} {was} -> {now}")
        p("\n  vacuity check — the same files with comments LEFT IN:")
        if flipped:
            for f in flipped:
                p(f"      {f}")
            p(f"  {len(flipped)} of {len(SPECIMENS)} specimens flip shape when the")
            p("  planted comments are read as code. The control has teeth.")
        else:
            p("      nothing flipped — THE NEGATIVE CONTROL IS VACUOUS, its PASS")
            p("      means nothing. Treat it as could-not-judge.")
            neg_ok = False
    finally:
        shutil.rmtree(scratch, ignore_errors=True)

    p(f"\n  POSITIVE {'PASSED' if pos_ok else 'FAILED'} ({pos_ran} isolated "
      f"conversion(s))   NEGATIVE {'PASSED' if neg_ok else 'FAILED'}\n")
    return 0 if (pos_ok and neg_ok) else 1


# --------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--rev", help="gate zero: read every specimen at this git rev")
    ap.add_argument("--root", help="gate zero: read every specimen from DIR/<repo-rel-path>")
    ap.add_argument("--swap", action="append", default=[], metavar="PATH=REV",
                    help="gate zero: read just PATH at REV (repeatable)")
    ap.add_argument("--gate-zero", action="store_true",
                    help="run both controls and report predicted vs observed")
    args = ap.parse_args()

    if args.gate_zero:
        if args.rev or args.root or args.swap:
            print("hpr-cost: --gate-zero drives --rev/--root/--swap itself",
                  file=sys.stderr)
            return 2
        try:
            return gate_zero()
        except Absent as exc:
            print(f"hpr-cost: {exc}", file=sys.stderr)
            return 3

    root = Path(args.root).resolve() if args.root else None
    swaps: dict[str, str] = {}
    for s in args.swap:
        if "=" not in s:
            print(f"hpr-cost: --swap needs PATH=REV, got {s!r}", file=sys.stderr)
            return 2
        path, rev = s.split("=", 1)
        if path not in SPECIMENS:
            print(f"hpr-cost: --swap path {path!r} is not one of the five frozen "
                  f"specimens", file=sys.stderr)
            return 2
        swaps[path] = rev
    is_control = bool(args.rev or root or swaps)

    try:
        rows, problems = score(rev=args.rev, root=root, swaps=swaps)
    except Absent as exc:
        print(f"hpr-cost: {exc}", file=sys.stderr)
        return 3
    except OSError as exc:
        print(f"hpr-cost: {exc}", file=sys.stderr)
        return 4

    if problems:
        for p in problems:
            print(f"hpr-cost: {p}", file=sys.stderr)
        print("hpr-cost: no value reported (exit 3 = could-not-judge / artifact-absent)",
              file=sys.stderr)
        return 3

    value = total(rows)

    commit, dirty = None, None
    if root is None:
        try:
            commit = _git("rev-parse", args.rev or "HEAD").strip()
            dirty = bool(_git("status", "--porcelain").strip())
        except Absent as exc:
            print(f"hpr-cost: {exc}", file=sys.stderr)
            return 4

    if is_control:
        print("hpr-cost: GATE-ZERO run — a control value, not a bar value "
              f"(rev={args.rev!r} root={args.root!r} swap={args.swap})", file=sys.stderr)

    if args.json:
        print(json.dumps({
            "value": value,
            "commit": commit,
            "dirty": dirty,
            "gate_zero": is_control,
            "precondition": "met: >=1 specimen derived; clap declared in its crate "
                            "(PROXY for 'compiles' — this script runs no cargo)",
            "specimens": rows,
        }))
        return 0

    floor, target = thresholds()
    print("\n  hpr-cost — places that must change to add one flag\n")
    for r in rows:
        print(f"  {r['cost']}  {r['shape']:<12} {Path(r['path']).name}")
        for site in r["sites"]:
            print(f"        - {site}")
    band = (f"floor {floor:g}  target {target:g}" if floor is not None
            else f"floor/target UNREADABLE from {CAMPAIGN_TOML.name}")
    print(f"\n  value {value:g}   {band}   lower is better")
    print(f"  commit {(commit or '(no commit — --root run)')[:12]}"
          + ("  DIRTY" if dirty else ""))
    print("\n  Every place listed is somewhere an author types to add one flag.")
    print("  Counted from the source, never assumed: hand-rolled surfaces here")
    print("  cost 2, 3 and 4 depending on how the parse function accumulates.")
    print("  Verbosity cannot move this number; only removing a place can.\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
