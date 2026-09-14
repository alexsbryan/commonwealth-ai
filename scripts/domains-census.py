#!/usr/bin/env python3
"""domains-census — the instrument for the domains campaign.

Every bar in quality/campaigns/domains.toml names this script as its instrument.
It reads the bounded-context registry quality/DOMAINS.toml (DATA — the contexts,
the words each owns, the module tags, the per-type dispositions) and counts what
the registry says, one subcommand per bar. No crate name, word or path pattern
is a constant here: a rule that needs one goes into the registry as a row
(ARCH principle 9, and this order's "Seams").

GREP-SHAPED ON PURPOSE. Every axis greps tracked `*.rs` through `git grep` and
strips comments with `code_part` (copied from scripts/nc-extends.py — copied,
not re-derived). The three graph-backed bars all read zero on an emptied index
on 2026-08-20, so no axis reads the SCIP index or the daemon; no axis shells
cargo except `liftable`, which runs the two lift scripts.

MEASUREMENT AND JUDGEMENT HAVE DISTINCT OUTPUTS.
  - A successful measurement (`--json`) ends with ONE compact
    `json.dumps({"value": N, "commit": "<sha>"})` line, consumed by
    `scripts/co-lineage.py::_parse_value`; no verdict follows it. A failed
    measurement emits no fabricated value: exit 3 (artifact absent) or 4
    (could-not-judge — a coverage hole), never a zero.
  - `--self-test` and `predicate` are judging runs; they end with
    `scripts/lib/judgement.py`'s four-verdict emitter, whose reason is the half
    a reader acts on.

THE SELF-TEST PLANTS A POSITIVE AND A NEGATIVE CONTROL PER AXIS. A positive
fixture must be caught; a negative fixture (the word in a doc comment, a `use`
line, a string literal, a variable named `peer_foo`) must be refused. An axis
that cannot be given a negative control a plausible refactor would trip is
telemetry and is dropped, not shipped blind (domains-3-instrument, "Not worth
continuing if"). This rung lands the runner and the registry load; each axis
registers itself, with its controls, as its subcommand lands.

Exit codes: 0 value valid (or self-test green), 3 artifact absent,
4 could-not-judge (coverage hole), other = error.
"""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The four-verdict line a judging run ends with (`scripts/lib/judgement.py`).
sys.path.insert(0, str(REPO / "scripts" / "lib"))
from judgement import emit as emit_judgement  # noqa: E402

DT = REPO / "quality" / "DOMAINS.toml"

EXIT_OK = 0
EXIT_ARTIFACT_ABSENT = 3
EXIT_COULD_NOT_JUDGE = 4

_REGISTRY: dict | None = None


def registry(path: Path = DT) -> dict:
    """quality/DOMAINS.toml as a dict. Loaded once; the file is DATA."""
    global _REGISTRY
    if _REGISTRY is None:
        with open(path, "rb") as f:
            _REGISTRY = tomllib.load(f)
    return _REGISTRY


def code_part(content: str) -> str:
    """The executable part of a source line — comments removed.

    THE BAR MUST NOT COUNT PROSE. Found by nc-13's worker 2026-08-20: the tool
    axis transiently read 86 instead of 83 because THREE DOC-COMMENT MENTIONS
    of `impl Tool for`, in a module whose whole subject is that trait, scored as
    implementations. A measurement an author can move by writing a sentence is
    not a measurement, so the guard lives here rather than in everyone's prose.

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


def git_head() -> str | None:
    """HEAD at measure time, for the measurement line's `commit`."""
    r = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO,
                       capture_output=True, text=True)
    return r.stdout.strip() if r.returncode == 0 else None


def emit_measurement(value) -> None:
    """The one line `co-lineage.py::_parse_value` reads. Nothing after it.

    A caller that cannot take a value does NOT call this: it exits 3 or 4 with
    no line, because a fabricated zero is indistinguishable from a real one
    (ARCH principle 6).
    """
    print(json.dumps({"value": value, "commit": git_head()}))


# ── The axes ────────────────────────────────────────────────────────────────
#
# One entry per subcommand that is a bar instrument. Each carries a `detect`
# over a root directory and a planted `positive` / `negative` fixture written
# into a temp dir by the self-test. `detect` returns something truthy iff it
# finds a hit, so the same function serves the tree (root=REPO) and a fixture
# (root=tempdir) — the self-test is the axis's own function, not a re-derivation
# of it. No axis is registered yet; the runner below is wired to take them.
AXES: list[dict] = []

SUBCOMMANDS: dict[str, callable] = {}


def subcommand(name: str):
    """Register a measurement subcommand. `fn(args) -> int`."""
    def deco(fn):
        SUBCOMMANDS[name] = fn
        return fn
    return deco


def self_test() -> int:
    """Plant a positive and a negative control per axis; report caught/refused.

    The positive must be caught (a real definition scores) and the negative
    refused (prose, a `use`, a string, a differently-named variable do not).
    An axis whose controls do not behave is the axis's bug, and this exits 1 —
    a self-test that cannot fail is not a self-test (ARCH principle 5).
    """
    print("domains-census --self-test — a planted positive and negative per axis\n")
    if not AXES:
        print("  (no axes registered yet: the runner is wired and will plant a "
              "positive and a negative control as each subcommand lands)")
        emit_judgement(
            "domains-census", "could-not-judge",
            "no axes registered yet — the self-test runner is wired but has no "
            "planted control to judge, so it makes no claim")
        return EXIT_OK

    total = caught = refused = 0
    failed: list[str] = []
    for axis in AXES:
        total += 1
        with tempfile.TemporaryDirectory(prefix=f"domains-census-{axis['id']}-pos-") as tp, \
             tempfile.TemporaryDirectory(prefix=f"domains-census-{axis['id']}-neg-") as tn:
            axis["positive"](Path(tp))
            c = bool(axis["detect"](Path(tp)))
            axis["negative"](Path(tn))
            r = not bool(axis["detect"](Path(tn)))
        caught += c
        refused += r
        ok = c and r
        if not ok:
            failed.append(axis["id"])
        print(f"  {axis['id']:<22} caught={str(c):<5} refused={str(r):<5} "
              f"{'ok' if ok else 'FAIL'}")

    print(f"\n  {total} axes · {caught}/{total} positives caught · "
          f"{refused}/{total} negatives refused")
    if failed:
        emit_judgement("domains-census", "failed",
                       f"planted controls misbehaved for: {', '.join(failed)}")
        return 1
    emit_judgement(
        "domains-census", "passed",
        f"all {total} axes caught their planted positive control and refused "
        f"their planted negative")
    return EXIT_OK


def main(argv: list[str]) -> int:
    args = list(argv)
    if "--self-test" in args:
        return self_test()
    if not args or args[0].startswith("-"):
        print("usage: domains-census.py <subcommand> [--json]", file=sys.stderr)
        print(f"subcommands: {', '.join(sorted(SUBCOMMANDS)) or '(none registered yet)'}",
              file=sys.stderr)
        return 2
    name = args.pop(0)
    fn = SUBCOMMANDS.get(name)
    if fn is None:
        print(f"domains-census: unknown subcommand {name!r}", file=sys.stderr)
        return 2
    return fn(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
