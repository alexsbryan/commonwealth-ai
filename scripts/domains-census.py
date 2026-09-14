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
import os
import re
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


# ── peer-outside — dm-peer-outside-fabric ───────────────────────────────────
#
# No crate outside the commonwealth package may define a type whose name
# carries the word `Peer`. The sweep is ANYWHERE in the name, never a prefix:
# a prefix bar is passed by renaming `PeerFoo` to `MeshPeerFoo`, which moves no
# coupling (campaigns/domains.toml:135-139). `--anywhere` is therefore the only
# mode; a `--prefix` mode is not implemented.
#
# TWO LITERALS THE REGISTRY CANNOT SUPPLY, and why they are not the constant
# the Seams forbid. The word `Peer` is the axis's own subject — Fabric owns
# `Member`, so no `[[context]].owns` row carries `Peer`, and deriving it from
# the `[[noun]]` rows would break the moment the bar hits target and the rows
# are renamed away. The package prefix is the bar's own scope ("outside the
# commonwealth package", campaigns/domains.toml:132). Every word the
# `word-owners` axis sweeps comes from the registry; this axis is the Peer bar,
# so its word is its definition.
_PEER_WORD = "Peer"
_PEER_DEF = re.compile(
    r"^\s*pub(?:\(crate\))?\s+(?:struct|enum|type|trait)\s+(\w+)")
_COMMONWEALTH_PREFIX = "commonwealth/crates/"
_WALK_SKIP = frozenset({"target", ".git", "node_modules"})


def _rs_files(root: Path) -> list[Path]:
    """Every tracked `*.rs` under `root`; a filesystem walk for a fixture.

    `git ls-files` is the tree path: the universe is what is committed, and it
    cannot wander into `target/`. A temp-dir fixture is not a work tree, so it
    is walked with the heavy directories pruned. One enumerator, so the axis
    the self-test drives is the axis that runs on the tree.
    """
    root = Path(root)
    if (root / ".git").exists():
        r = subprocess.run(["git", "ls-files", "--", "*.rs"],
                           cwd=root, capture_output=True, text=True)
        if r.returncode == 0:
            return [root / p for p in r.stdout.splitlines() if p]
    out: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in _WALK_SKIP]
        out.extend(Path(dirpath) / fn for fn in filenames if fn.endswith(".rs"))
    return out


def _crate_dir(path: Path, root: Path) -> str:
    """The crate a file belongs to: its nearest ancestor with a Cargo.toml.

    Only ancestors AT or BELOW `root` are considered, so a stray manifest above
    a temp-dir fixture can never claim it.
    """
    root = Path(root)
    for d in (path.parent, *path.parents):
        if d != root and root not in d.parents:
            continue
        if (d / "Cargo.toml").exists():
            return d.relative_to(root).as_posix() if d != root else "."
    try:
        rel = path.relative_to(root).as_posix()
    except ValueError:
        return path.as_posix()
    return rel.split("/", 1)[0]


def peer_defs(root: Path) -> list[dict]:
    """`Peer`-named definitions outside the commonwealth package, keeps removed.

    One dict per definition (`file`, `line`, `name`, `crate`), so the self-test
    can ask it for truthiness and the subcommand can print the crate set. The
    registry's `[[noun]]` rows with `disposition = "decided:keep"` are
    subtracted BY NAME — the registry's own carve-out list (e.g. `PeerAnswer`,
    C9 egress custody), not an exception in this script.
    """
    root = Path(root)
    kept = {n["name"] for n in registry().get("noun", [])
            if n.get("disposition") == "decided:keep"}
    found: list[dict] = []
    for path in _rs_files(root):
        try:
            rel = path.relative_to(root).as_posix()
        except ValueError:
            rel = path.as_posix()
        if rel.startswith(_COMMONWEALTH_PREFIX):
            continue
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        for i, line in enumerate(lines):
            m = _PEER_DEF.match(code_part(line))
            if not m or _PEER_WORD not in m.group(1):
                continue
            if m.group(1) in kept:
                continue
            found.append({"file": rel, "line": i + 1, "name": m.group(1),
                          "crate": _crate_dir(path, root)})
    return found


def _peer_positive(root: Path) -> None:
    """A planted real definition: caught."""
    (root / "fixture").mkdir(parents=True, exist_ok=True)
    (root / "fixture" / "lib.rs").write_text(
        "pub struct FooPeerBar {\n    n: u32,\n}\n", encoding="utf-8")


def _peer_negative(root: Path) -> None:
    """The word present but not a definition: refused.

    The five shapes the row names (`use`, a commented-out definition, a doc
    comment, a string literal, a lower-case variable, an `impl`) plus a
    definition that does NOT carry the word — without that last line the
    negative cannot catch a broken word filter.
    """
    (root / "fixture").mkdir(parents=True, exist_ok=True)
    (root / "fixture" / "lib.rs").write_text(
        "use x::PeerFoo;\n"
        "// pub struct PeerFoo\n"
        "/// A doc comment naming `pub struct PeerFoo`.\n"
        "const NAME: &str = \"PeerFoo\";\n"
        "fn f() { let peer_foo = 1; }\n"
        "impl PeerFoo {}\n"
        "pub struct PlainThing;\n",
        encoding="utf-8")


AXES.append({
    "id": "peer-outside",
    "detect": peer_defs,
    "positive": _peer_positive,
    "negative": _peer_negative,
})


@subcommand("peer-outside")
def cmd_peer_outside(args: list[str]) -> int:
    """Print the count and the crate set, or the measurement line with --json."""
    found = peer_defs(REPO)
    if "--json" in args:
        emit_measurement(len(found))
        return EXIT_OK
    by_crate: dict[str, int] = {}
    for h in found:
        by_crate[h["crate"]] = by_crate.get(h["crate"], 0) + 1
    print("peer-outside — Peer* definitions outside commonwealth/crates/ "
          "(the word anywhere in the name; prefix mode refused)\n")
    for crate in sorted(by_crate):
        print(f"  {crate:<46} {by_crate[crate]:>3}")
    print(f"\n  value: {len(found)} in {len(by_crate)} crates")
    return EXIT_OK


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
