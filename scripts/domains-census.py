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

import datetime as _dt
import fnmatch
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import tomllib
from collections import Counter
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


def registry_for(root: Path) -> dict:
    """The registry for a scan root: the fixture's own, else the repo's.

    The self-test drives an axis against a temp dir, and an axis that reads
    `[[edge]]` rows must be judged by the registry the fixture planted, not by
    the repo's — otherwise the planted control and the file that judges it are
    different documents. A root with no `quality/DOMAINS.toml` falls back to
    the repo registry, so an axis can be driven against a bare source fixture.
    """
    root = Path(root)
    if root.resolve() == REPO.resolve():
        return registry()
    p = root / "quality" / "DOMAINS.toml"
    if p.exists():
        with open(p, "rb") as f:
            return tomllib.load(f)
    return registry()


def instrument(reg: dict) -> dict:
    """The census axes' subjects — `[instrument]`, DATA (ARCH 8, O3 "Seams").

    The script carries no crate name, word or path as a constant: the word each
    axis sweeps, the context whose vocabulary it reads and the ARCH_LAYERS
    package it scopes to are all rows here. A rule that needs a constant goes
    into the registry, never into this file.
    """
    return reg.get("instrument", {})


def context_by_id(reg: dict, cid: str) -> dict | None:
    """The `[[context]]` row named `cid`, or None."""
    for c in reg.get("context", []):
        if c.get("id") == cid:
            return c
    return None


def peer_word(reg: dict) -> str:
    """The word the peer-outside bar retires.

    Fabric owns `Member` (quality/DOMAINS.toml:136) — `Peer` is the legacy word
    the bar exists to remove, so no `[[context]].owns` row carries it and the
    axis's subject is declared as `[instrument].peer_word` instead.
    """
    return instrument(reg).get("peer_word", "")


def atom_context(reg: dict) -> dict | None:
    """The context whose word and roots the atom-outside axis reads."""
    return context_by_id(reg, instrument(reg).get("atom_context", ""))


def atom_word(reg: dict) -> str:
    """The atom axis's word: the head of the atom context's `owns` list.

    READ, never re-spelled (ARCH 8): Understanding's `owns` carries `Atom` as
    its primary word (quality/DOMAINS.toml:41), and the axis sweeps it.
    """
    c = atom_context(reg)
    owns = c.get("owns", []) if c else []
    return owns[0] if owns else ""


def atom_roots(reg: dict) -> list[str]:
    """The roots where the atom vocabulary is authored — `vocab_roots`."""
    c = atom_context(reg)
    return list(c.get("vocab_roots", [])) if c else []


def exempt_contexts(reg: dict) -> set[str]:
    """Context ids whose crates count against NO owner (`exempt = true`)."""
    return {c["id"] for c in reg.get("context", []) if c.get("exempt")}


def member_edge_types(reg: dict) -> set[str]:
    """The member types: the leaf of every `[[seam]].source_type`.

    READ, never re-spelled (ARCH 8): the canonical member type is the seam's
    `source_type` (quality/DOMAINS.toml:1388, `commonwealth_core::mesh::MemberRecord`).
    """
    out: set[str] = set()
    for s in reg.get("seam", []):
        src = s.get("source_type", "")
        if src:
            out.add(src.split("::")[-1].strip())
    return out


def _arch_package_crates(root: Path, package: str) -> list[str]:
    """The crates an ARCH_LAYERS `[[package]]` names, fixture-aware.

    A fixture with no `quality/ARCH_LAYERS.toml` falls back to the repo's,
    exactly as `arch_packages_for` does.
    """
    p = Path(root) / "quality" / "ARCH_LAYERS.toml"
    if not p.exists():
        p = REPO / "quality" / "ARCH_LAYERS.toml"
    try:
        with open(p, "rb") as f:
            data = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        return []
    for pkg in data.get("package", []):
        if pkg.get("name") == package:
            return list(pkg.get("crates", []))
    return []


def commonwealth_prefix(root: Path, package: str) -> str:
    """The directory the package's crates live under, as a path prefix.

    The peer-outside scope is "outside the commonwealth package"
    (campaigns/domains.toml:132), and the package's crate list is the registry
    of what that package IS (ARCH_LAYERS.toml:906). DERIVED from that list and
    the workspace members, so the script carries no path (O3 "Seams"). Empty
    when no member resolves (a bare fixture), which skips nothing.
    """
    names = set(_arch_package_crates(root, package))
    dirs = [d for n, d in _workspace_crates(root) if n in names]
    if not dirs:
        return ""
    common = Path(os.path.commonpath([str(d) for d in dirs]))
    try:
        rel = common.relative_to(Path(root)).as_posix()
    except ValueError:
        return ""
    return "" if rel == "." else rel + "/"


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
# NO LITERAL SUBJECT. The word is `[instrument].peer_word` and the scope is
# derived from the ARCH_LAYERS `commonwealth` package's crates — the script
# carries neither (ARCH 8, O3 "Seams"). `Peer` has no `[[context]].owns` home
# because Fabric owns `Member` (:136) and the bar exists to remove the legacy
# word; the registry declares it rather than the script.
_TYPE_DEF = re.compile(
    r"^\s*pub(?:\(crate\))?\s+(?:struct|enum|type|trait)\s+(\w+)")
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
    reg = registry_for(root)
    kept = {n["name"] for n in reg.get("noun", [])
            if n.get("disposition") == "decided:keep"}
    word = peer_word(reg)
    prefix = commonwealth_prefix(root, instrument(reg).get("commonwealth_package", ""))
    found: list[dict] = []
    for path in _rs_files(root):
        try:
            rel = path.relative_to(root).as_posix()
        except ValueError:
            rel = path.as_posix()
        if prefix and rel.startswith(prefix):
            continue
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        for i, line in enumerate(lines):
            m = _TYPE_DEF.match(code_part(line))
            if not m or not word or word not in m.group(1):
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


# ── shared-edges — dm-shared-edges ──────────────────────────────────────────
#
# A cross-context edge is a `[[edge]]` row: a type defined in one context that
# a module of another reads. The registry is the denominator (13 rows, E1-E13,
# hand-enumerated 2026-09-13, each carrying the fields the consumer actually
# reads); the tree decides which are still live. The value is the live edges
# with no named translation — the bar is "Fabric's member type reaches Serving
# and Compute only through a named translation", so a row carrying
# `translation = "<path:line>"` is off the count (E10, the working pattern, is
# the one already translated).
#
# LIVENESS IS NOT A `use`-LINE SCAN. A use-only sweep of the six consuming
# contexts finds 7 of the 13 (measured 2026-09-14): E1 crosses as a field
# reference (`Arc<commonwealth_core::peer_health::PeerHealthTracker>`), E7 as a
# `Debug`-flattened `trust_level`, E3/E4 as a whole-record pass with no import
# at all. The row's own `fields_read` names what the consumer touches, so an
# edge is live iff its `to_module` file names the `from_type` or any field it
# reads. A row whose file names neither is STALE and is printed, never counted
# (ARCH principle 6 — absence is reported, not defaulted).
#
# THE MEMBER TYPE IS READ, NOT RE-SPELLED. `MemberRecord` is the leaf of the
# `member_to_candidate` [[seam]].source_type (:1388); `_is_member_edge` takes
# the peer word and the seam-derived set from the registry, so neither is a
# constant here (ARCH 8, O3 "Seams").


def _is_member_edge(name: str, peer: str, members: set[str]) -> bool:
    """The bar's own scope: a name carrying the peer word, or a member type."""
    return (bool(peer) and peer in name) or name in members


def _field_leaf(field: str) -> str:
    """`capabilities.embed_model` -> `embed_model`; a bare name unchanged."""
    return field.strip().split(".")[-1].strip()


def shared_edges(root: Path) -> dict:
    """Classify every registry `[[edge]]` row against the tree at `root`.

    Returns four lists — `counted` (live, untranslated, member-named: the
    value), plus `translated`, `stale` and `offname`, which the subcommand
    prints so an excluded edge is visible rather than silently dropped.
    """
    reg = registry_for(root)
    peer = peer_word(reg)
    members = member_edge_types(reg)
    counted: list[dict] = []
    translated: list[dict] = []
    stale: list[dict] = []
    offname: list[dict] = []
    for edge in reg.get("edge", []):
        row = dict(edge)
        to = Path(root) / edge["to_module"]
        try:
            text = to.read_text(encoding="utf-8", errors="replace")
        except OSError:
            text = ""
        names = [edge["from_type"]] + [_field_leaf(f)
                                       for f in edge.get("fields_read", [])]
        row["live"] = bool(text) and any(n and n in text for n in names)
        if not _is_member_edge(edge["from_type"], peer, members):
            offname.append(row)
        elif edge.get("translation", "none") != "none":
            translated.append(row)
        elif not row["live"]:
            stale.append(row)
        else:
            counted.append(row)
    return {"counted": counted, "translated": translated,
            "stale": stale, "offname": offname}


def _shared_edges_detect(root: Path) -> list[dict]:
    """The axis's own function: truthy iff a live untranslated edge exists."""
    return shared_edges(root)["counted"]


def _shared_positive(root: Path) -> None:
    """A live, untranslated member edge: caught."""
    (root / "quality").mkdir(parents=True, exist_ok=True)
    (root / "quality" / "DOMAINS.toml").write_text(
        '[instrument]\n'
        'peer_word = "Peer"\n'
        '\n'
        '[[seam]]\n'
        'name = "member_to_candidate"\n'
        'source_type = "commonwealth_core::mesh::MemberRecord"\n'
        '\n'
        '[[edge]]\n'
        'from_type = "MemberRecord"\n'
        'from_context = "fabric"\n'
        'to_module = "fixture/consumer.rs"\n'
        'to_context = "serving"\n'
        'fields_read = ["node_id"]\n'
        'translation = "none"\n', encoding="utf-8")
    (root / "fixture").mkdir(parents=True, exist_ok=True)
    (root / "fixture" / "consumer.rs").write_text(
        "use commonwealth_core::mesh::MemberRecord;\n"
        "fn f(m: &MemberRecord) { let _ = m.node_id; }\n", encoding="utf-8")


def _shared_negative(root: Path) -> None:
    """Three refusals, each a plausible registry row.

    A translated edge (its `translation` names a function), a stale row (its
    `to_module` no longer names the type or any field), and an off-name type
    (`PlainThing` is live and untranslated but is not what the bar sweeps).
    """
    (root / "quality").mkdir(parents=True, exist_ok=True)
    (root / "quality" / "DOMAINS.toml").write_text(
        '[instrument]\n'
        'peer_word = "Peer"\n'
        '\n'
        '[[seam]]\n'
        'name = "member_to_candidate"\n'
        'source_type = "commonwealth_core::mesh::MemberRecord"\n'
        '\n'
        '[[edge]]\n'
        'from_type = "PeerFoo"\n'
        'from_context = "fabric"\n'
        'to_module = "fixture/translated.rs"\n'
        'to_context = "serving"\n'
        'fields_read = []\n'
        'translation = "sovereign/x.rs:1"\n'
        '\n'
        '[[edge]]\n'
        'from_type = "PeerBar"\n'
        'from_context = "fabric"\n'
        'to_module = "fixture/stale.rs"\n'
        'to_context = "serving"\n'
        'fields_read = []\n'
        'translation = "none"\n'
        '\n'
        '[[edge]]\n'
        'from_type = "PlainThing"\n'
        'from_context = "kernel"\n'
        'to_module = "fixture/plain.rs"\n'
        'to_context = "serving"\n'
        'fields_read = []\n'
        'translation = "none"\n', encoding="utf-8")
    (root / "fixture").mkdir(parents=True, exist_ok=True)
    (root / "fixture" / "translated.rs").write_text(
        "use commonwealth_core::mesh::PeerFoo;\n", encoding="utf-8")
    (root / "fixture" / "plain.rs").write_text(
        "use some::PlainThing;\n", encoding="utf-8")
    # fixture/stale.rs is deliberately absent: PeerBar's row names a file that
    # no longer exists, so the row is stale and must not count.


AXES.append({
    "id": "shared-edges",
    "detect": _shared_edges_detect,
    "positive": _shared_positive,
    "negative": _shared_negative,
})


@subcommand("shared-edges")
def cmd_shared_edges(args: list[str]) -> int:
    """Print the live untranslated edge count and, per edge, the fields read."""
    r = shared_edges(REPO)
    counted = r["counted"]
    if "--json" in args:
        emit_measurement(len(counted))
        return EXIT_OK
    print("shared-edges — live cross-context [[edge]] rows with no named "
          "translation\n  (registry denominator; a stale or translated row is "
          "printed, never counted)\n")
    for row in counted:
        edge = f"{row['from_context']} -> {row['to_context']}"
        fields = ", ".join(row.get("fields_read", [])) or "—"
        print(f"  {row['from_type']:<22} {edge:<26} "
              f"fields={len(row.get('fields_read', []))}")
        print(f"  {'':<22} reads: {fields}")
    for label, rows in (("translated", r["translated"]),
                        ("stale", r["stale"]), ("off-name", r["offname"])):
        for row in rows:
            print(f"  [excluded/{label}] {row['from_type']} "
                  f"({row['from_context']} -> {row['to_context']})")
    print(f"\n  value: {len(counted)} live untranslated edges "
          f"({len(r['translated'])} translated, {len(r['stale'])} stale, "
          f"{len(r['offname'])} off-name)")
    return EXIT_OK


# ── word-owners — dm-word-owners ────────────────────────────────────────────
#
# Every `[[context]]` owns a NOUN, matched ANYWHERE in a type name (`owns`) or
# as the bare name (`owns_exact`, Ingest's `Source`). A definition carrying an
# owned word must live inside its owner — a module tagged with that context, or
# a crate that is that context's home. A definition in a module tagged with a
# DIFFERENT context, or in a crate that is no context's home, is a violation.
# `kernel` and `back-of-house` are the published language and the observer: a
# definition there counts against NO owner and is PRINTED as exempt, never
# dropped (DOMAINS.md §10.1; the registry's own comment at its head).
#
# THE EXEMPTION IS REGISTRY DATA. Which contexts are exempt is `exempt = true`
# on their `[[context]]` rows (`kernel` :202, `back-of-house` :226); the script
# reads it and carries no id. Everything else — the words, the owners, the tags,
# the homes — is read from the registry too.
#
# THE GOODHART IS A ZERO-REFERENCE OWNER. The bar can hit target while the
# predicate is false if a word is made unique by a compound name nobody reads,
# so each definition prints its reference-site count and a zero-reference
# definition is flagged (campaigns/domains.toml, dm-word-owners goodhart).
_TOKEN = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def _module_context(reg: dict, rel: str) -> str | None:
    """The registry tag for a source path: exact file row, else longest dir row."""
    best: str | None = None
    best_len = -1
    for row in reg.get("module", []):
        p = row.get("path")
        if not p:
            continue
        prefix = p if p.endswith("/") else p + "/"
        if rel == p or rel.startswith(prefix):
            if len(p) > best_len:
                best = row.get("context")
                best_len = len(p)
    return best


def word_owners(root: Path) -> dict:
    """Classify every owned-word definition at `root` against its owner.

    Returns `counted` (definitions outside their owner) and `exempt`
    (kernel/back-of-house crates), each carrying the words it carries and a
    reference-site count. One pass collects the definitions, a second counts
    identifier occurrences so a zero-reference owner can be flagged.
    """
    root = Path(root)
    reg = registry_for(root)
    owned: list[tuple[str, str, bool]] = []
    for c in reg.get("context", []):
        for w in c.get("owns", []):
            owned.append((w, c["id"], False))
        for w in c.get("owns_exact", []):
            owned.append((w, c["id"], True))
    homes: dict[str, set[str]] = {}
    exempt = exempt_contexts(reg)
    exempt_crates: set[str] = set()
    for c in reg.get("context", []):
        for cr in c.get("crates", []):
            homes.setdefault(cr, set()).add(c["id"])
            if c["id"] in exempt:
                exempt_crates.add(cr)

    defs: list[dict] = []
    for path in _rs_files(root):
        try:
            rel = path.relative_to(root).as_posix()
        except ValueError:
            rel = path.as_posix()
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        crate = Path(_crate_dir(path, root)).name
        for i, line in enumerate(lines):
            m = _TYPE_DEF.match(code_part(line))
            if m:
                defs.append({"file": rel, "line": i + 1, "name": m.group(1),
                             "crate": crate})

    counted: list[dict] = []
    exempt: list[dict] = []
    for d in defs:
        matched = [(w, o) for (w, o, exact) in owned
                   if (d["name"] == w if exact else w in d["name"])]
        if not matched:
            continue
        if d["crate"] in exempt_crates:
            d["words"] = matched
            exempt.append(d)
            continue
        tag = _module_context(reg, d["file"])
        ctxs = {tag} if tag else homes.get(d["crate"], set())
        bad = [(w, o) for (w, o) in matched if o not in ctxs]
        if bad:
            d["words"] = bad
            counted.append(d)

    names = {d["name"] for d in defs}
    ndefs = Counter(d["name"] for d in defs)
    counts: Counter = Counter()
    for path in _rs_files(root):
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        for line in lines:
            for tok in _TOKEN.findall(code_part(line)):
                if tok in names:
                    counts[tok] += 1
    for d in counted + exempt:
        d["refs"] = counts[d["name"]] - ndefs[d["name"]]
    return {"counted": counted, "exempt": exempt}


def _word_owners_detect(root: Path) -> list[dict]:
    """The axis's own function: truthy iff an owned word is defined outside it."""
    return word_owners(root)["counted"]


def _word_registry(root: Path, tag: str) -> None:
    """A registry with one owning context and one tagging context.

    `tag` is the context the fixture module is tagged with, so the SAME source
    is a violation when tagged `other` and in-owner when tagged `widget`.
    """
    (root / "quality").mkdir(parents=True, exist_ok=True)
    (root / "quality" / "DOMAINS.toml").write_text(
        '[[context]]\n'
        'id = "widget"\n'
        'kind = "core"\n'
        'owns = ["Sprocket"]\n'
        'owns_exact = ["Cog"]\n'
        'crates = ["owner-crate"]\n'
        '\n'
        '[[context]]\n'
        'id = "kernel"\n'
        'kind = "published-language"\n'
        'owns = []\n'
        'exempt = true\n'
        'crates = ["exempt-crate"]\n'
        '\n'
        '[[module]]\n'
        f'path = "fixture/lib.rs"\n'
        f'context = "{tag}"\n',
        encoding="utf-8")


def _word_positive(root: Path) -> None:
    """A definition carrying an owned word, tagged a different context: caught."""
    _word_registry(root, "other")
    (root / "fixture").mkdir(parents=True, exist_ok=True)
    (root / "fixture" / "lib.rs").write_text(
        "pub struct SprocketThing {\n    n: u32,\n}\n", encoding="utf-8")


def _word_negative(root: Path) -> None:
    """Four refusals, each a plausible near-miss.

    The owned word in its owner's module (`widget`); a definition in a
    kernel-tagged crate (exempt, never counted); a name that carries the word
    but is not an owned match (`owns_exact` `Cog` vs `Cogwheel`); and the word
    in a `use`, a comment, a string and a variable, none of them a definition.
    """
    _word_registry(root, "widget")
    (root / "fixture").mkdir(parents=True, exist_ok=True)
    (root / "fixture" / "lib.rs").write_text(
        "pub struct SprocketThing;\n"
        "use x::SprocketOther;\n"
        "// pub struct SprocketCommented;\n"
        "const S: &str = \"SprocketString\";\n"
        "fn f() { let sprocket_local = 1; }\n",
        encoding="utf-8")
    (root / "exempt-crate").mkdir(parents=True, exist_ok=True)
    (root / "exempt-crate" / "lib.rs").write_text(
        "pub struct SprocketInKernel;\n", encoding="utf-8")
    (root / "other-crate").mkdir(parents=True, exist_ok=True)
    (root / "other-crate" / "lib.rs").write_text(
        "pub struct Cogwheel;\n", encoding="utf-8")


AXES.append({
    "id": "word-owners",
    "detect": _word_owners_detect,
    "positive": _word_positive,
    "negative": _word_negative,
})


@subcommand("word-owners")
def cmd_word_owners(args: list[str]) -> int:
    """Print the violating definitions, their words, refs and the exempt set."""
    r = word_owners(REPO)
    counted = r["counted"]
    if "--json" in args:
        emit_measurement(len(counted))
        return EXIT_OK
    print("word-owners — definitions carrying a context-owned word outside "
          "their owner\n  (owns = anywhere in the name; owns_exact = bare "
          "name; kernel/back-of-house crates print exempt)\n")
    for d in sorted(counted, key=lambda d: (d["crate"], d["file"], d["line"])):
        words = ", ".join(f"{w} ({o})" for w, o in d["words"])
        flag = "  ZERO-REF" if d["refs"] <= 0 else ""
        print(f"  {d['name']:<34} {d['crate']:<30} refs={d['refs']:<4} "
              f"word={words}{flag}")
    for d in sorted(r["exempt"], key=lambda d: (d["crate"], d["file"], d["line"])):
        words = ", ".join(f"{w} ({o})" for w, o in d["words"])
        print(f"  [exempt] {d['name']:<26} {d['crate']:<30} word={words}")
    print(f"\n  value: {len(counted)} definitions outside their owner "
          f"({len(r['exempt'])} exempt in kernel/back-of-house crates)")
    return EXIT_OK


# ── atom-outside — dm-atom-outside ──────────────────────────────────────────
#
# No consumer outside Understanding's vocabulary may declare its own `Atom*`
# type. The bar (campaigns/domains.toml dm-atom-outside) counts `pub Atom*`
# definitions outside the vocabulary roots — where the atom vocabulary is
# authored — minus the registry's allow-list (the three axum query binders),
# which lives as `[[noun]]` rows with `disposition = "decided:keep"` so that
# widening it is a registry diff a reviewer reads, never a script edit.
#
# NO LITERAL SUBJECT. The word is READ from Understanding's `owns` (its primary
# word, `Atom` at :41) and the roots are `vocab_roots` on the same context row;
# neither is a constant here (ARCH 8, O3 "Seams"). The allow-list, the part
# that can silently grow, is registry data too.


def atom_allowlist(reg: dict) -> list[str]:
    """The allow-list: kept `[[noun]]` names carrying the atom word.

    Read from the registry, so adding a binder to the allow-list is a registry
    diff (campaigns/domains.toml dm-atom-outside goodhart). A kept name that
    does not carry the word (the Peer carve-outs) is not this axis's business.
    """
    word = atom_word(reg)
    return sorted(n["name"] for n in reg.get("noun", [])
                  if n.get("disposition") == "decided:keep"
                  and word and word in n.get("name", ""))


def atom_defs(root: Path) -> list[dict]:
    """`Atom*` definitions outside the vocabulary roots, allow-list removed.

    One dict per definition (`file`, `line`, `name`, `crate`), so the self-test
    can ask it for truthiness and the subcommand can print the crate set.
    """
    root = Path(root)
    reg = registry_for(root)
    allow = set(atom_allowlist(reg))
    word = atom_word(reg)
    roots = [r.rstrip("/") + "/" for r in atom_roots(reg)]
    found: list[dict] = []
    for path in _rs_files(root):
        try:
            rel = path.relative_to(root).as_posix()
        except ValueError:
            rel = path.as_posix()
        if any(rel.startswith(r) for r in roots):
            continue
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        for i, line in enumerate(lines):
            m = _TYPE_DEF.match(code_part(line))
            if not m or not word or not m.group(1).startswith(word):
                continue
            if m.group(1) in allow:
                continue
            found.append({"file": rel, "line": i + 1, "name": m.group(1),
                          "crate": _crate_dir(path, root)})
    return found


def _allowlist_digest(allow: list[str]) -> str:
    """A short digest of the allow-list, printed beside the value.

    Tamper-evidence, not a gate: a digest change means the registry's kept
    `Atom*` rows changed, which is exactly the diff the goodhart demands be
    visible.
    """
    return hashlib.sha256("\n".join(allow).encode("utf-8")).hexdigest()[:12]


def _atom_registry(root: Path) -> None:
    """A registry whose only kept noun is the allow-list control.

    The context row carries the axis's word (`owns`) and roots (`vocab_roots`),
    read by `atom_word` / `atom_roots`; `[instrument]` names the context. Without
    them the fixture would exercise a different word and root set than the tree.
    """
    (root / "quality").mkdir(parents=True, exist_ok=True)
    (root / "quality" / "DOMAINS.toml").write_text(
        '[instrument]\n'
        'atom_context = "understanding"\n'
        '\n'
        '[[context]]\n'
        'id = "understanding"\n'
        'kind = "core"\n'
        'owns = ["Atom"]\n'
        'vocab_roots = ["corpus-engine-vocab", "corpus-engine/src/enrichment"]\n'
        '\n'
        '[[noun]]\n'
        'name = "AtomFixture"\n'
        'disposition = "decided:keep"\n', encoding="utf-8")


def _atom_positive(root: Path) -> None:
    """A planted `Atom*` definition outside the roots: caught."""
    _atom_registry(root)
    (root / "fixture").mkdir(parents=True, exist_ok=True)
    (root / "fixture" / "lib.rs").write_text(
        "pub struct AtomWidget {\n    n: u32,\n}\n", encoding="utf-8")


def _atom_negative(root: Path) -> None:
    """Refusals, each a plausible near-miss.

    The allow-listed name (a registry keep), a `pub` definition that does not
    carry the word, the non-definition shapes the sibling axes refuse (`use`, a
    commented-out definition, a doc comment, a string, a lower-case variable, an
    `impl`), and a real definition INSIDE each excluded root — without those the
    root filter and the allow-list subtraction are untested.
    """
    _atom_registry(root)
    (root / "fixture").mkdir(parents=True, exist_ok=True)
    (root / "fixture" / "lib.rs").write_text(
        "pub struct AtomFixture;\n"
        "pub struct PlainThing;\n"
        "use x::AtomThing;\n"
        "// pub struct AtomCommented;\n"
        "/// A doc comment naming `pub struct AtomDoc`.\n"
        "const NAME: &str = \"AtomString\";\n"
        "fn f() { let atom_local = 1; }\n"
        "impl AtomThing {}\n",
        encoding="utf-8")
    (root / "corpus-engine-vocab" / "src").mkdir(parents=True, exist_ok=True)
    (root / "corpus-engine-vocab" / "src" / "atoms.rs").write_text(
        "pub struct AtomInVocab;\n", encoding="utf-8")
    (root / "corpus-engine" / "src" / "enrichment").mkdir(parents=True,
                                                          exist_ok=True)
    (root / "corpus-engine" / "src" / "enrichment" / "writer.rs").write_text(
        "pub struct AtomInEnrichment;\n", encoding="utf-8")


AXES.append({
    "id": "atom-outside",
    "detect": atom_defs,
    "positive": _atom_positive,
    "negative": _atom_negative,
})


@subcommand("atom-outside")
def cmd_atom_outside(args: list[str]) -> int:
    """Print the definitions, the allow-list digest, or the measurement line."""
    found = atom_defs(REPO)
    allow = atom_allowlist(registry())
    if "--json" in args:
        emit_measurement(len(found))
        return EXIT_OK
    print("atom-outside — pub Atom* definitions outside corpus-engine-vocab/ and "
          "corpus-engine/src/enrichment/\n  (Understanding's word; the registry's "
          "kept Atom* rows are the allow-list)\n")
    for d in sorted(found, key=lambda d: (d["file"], d["line"])):
        print(f"  {d['name']:<22} {d['file']}:{d['line']}")
    print(f"\n  allow-list ({len(allow)}): {', '.join(allow) or '(none)'}")
    print(f"  allow-list digest: {_allowlist_digest(allow)}")
    print(f"\n  value: {len(found)} definitions outside the vocabulary roots")
    return EXIT_OK


# ── crate-lines, misnamed — dm-mesh-lines, dm-misnamed-crates ───────────────
#
# Two bars read the same table: the `[[module]]` rows' `lines`, and the
# `[[context]].crates` home lists. `crate-lines --crate X` sums the rows that
# belong to crate X (the dm-mesh-lines floor is 88,255 for sovereign-mesh);
# `misnamed` asks, per crate, what share of its lines carry a context the crate
# is the HOME of, and counts the crates whose name therefore describes less than
# half of what they hold (DOMAINS.md §3, §8 row 5; campaigns/domains.toml
# dm-misnamed-crates).
#
# A CRATE'S OWN CONTEXT IS ITS HOME LIST, never a name match: a crate is the
# home of every context whose `crates` list names it (registry head, "`crates`
# is a context's HOME"). A module tagged with a context that does not name the
# crate is MISFILED; a crate no context names has no own context and its share
# is zero. `unknown` is a legal tag and counts against the crate (registry,
# "Module tags" head) — absence is REPORTED (the module is listed in the
# per-crate table) and never silently dropped.
#
# THE COVERAGE ASSERTION PRECEDES EVERY COUNT. Every `.rs` under a workspace
# member's `src/` must have a `[[module]]` row — an exact file row, or a
# directory row whose path prefixes it. A file with neither is UNTAGGED: the
# count would silently understate, so the subcommand exits 4 naming the file
# (ARCH principle 6). The tag generator asserted exactly this when the census
# landed (dm3-tag-workspace: 0 untagged of 2073); the assertion keeps it true.
#
# THE DIGEST IS THE GOODHART. dm-misnamed-crates can hit target by editing the
# tag table instead of the code, so the value is printed beside a digest of the
# table — a retag changes the digest and is visible in the row.

def _workspace_crates(root: Path) -> list[tuple[str, Path]]:
    """(name, dir) for every workspace member crate under `root`.

    The repo path reads `[workspace].members`; a fixture with no root manifest
    is walked for `Cargo.toml` files. One enumerator, so the coverage the
    self-test drives is the coverage that runs on the tree.
    """
    root = Path(root)
    members: list[str] = []
    manifest = root / "Cargo.toml"
    if manifest.exists():
        try:
            with open(manifest, "rb") as f:
                members = list(tomllib.load(f).get("workspace", {}).get("members", []))
        except (OSError, tomllib.TOMLDecodeError):
            members = []
    if members:
        return [(Path(m).name, root / m) for m in members]
    out: list[tuple[str, Path]] = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in _WALK_SKIP]
        if "Cargo.toml" in filenames:
            out.append((Path(dirpath).name, Path(dirpath)))
    return out


def _module_paths(reg: dict) -> tuple[set[str], list[str]]:
    """The registry's module coverage: exact file rows and directory rows."""
    exact: set[str] = set()
    dirs: list[str] = []
    for r in reg.get("module", []):
        p = r.get("path")
        if not p:
            continue
        if p.endswith("/"):
            dirs.append(p)
        else:
            exact.add(p)
    return exact, dirs


def _covered(rel: str, exact: set[str], dirs: list[str]) -> bool:
    return rel in exact or any(rel.startswith(d) for d in dirs)


def coverage_holes(root: Path) -> list[str]:
    """Every `.rs` under a member's `src/` with no `[[module]]` row."""
    root = Path(root)
    exact, dirs = _module_paths(registry_for(root))
    files = _rs_files(root)
    holes: list[str] = []
    for _name, d in _workspace_crates(root):
        try:
            rel_dir = d.relative_to(root).as_posix()
        except ValueError:
            continue
        prefix = (rel_dir + "/") if rel_dir not in (".", "") else ""
        src = prefix + "src/"
        for path in files:
            try:
                rel = path.relative_to(root).as_posix()
            except ValueError:
                rel = path.as_posix()
            if rel.startswith(src) and not _covered(rel, exact, dirs):
                holes.append(rel)
    return sorted(set(holes))


def _row_crate(root: Path, rel: str) -> str:
    """The crate a `[[module]]` path belongs to: nearest Cargo.toml, by name."""
    d = _crate_dir(Path(root) / rel, Path(root))
    return Path(d).name if d not in (".", "") else "."


def crate_lines(root: Path, crate: str) -> list[dict]:
    """The `[[module]]` rows belonging to `crate`, in registry order."""
    reg = registry_for(root)
    return [dict(r) for r in reg.get("module", [])
            if r.get("path") and _row_crate(root, r["path"]) == crate]


def _crate_homes(reg: dict) -> dict[str, set[str]]:
    """crate -> the contexts whose `crates` list names it (its homes)."""
    homes: dict[str, set[str]] = {}
    for c in reg.get("context", []):
        for cr in c.get("crates", []):
            homes.setdefault(cr, set()).add(c["id"])
    return homes


def misnamed(root: Path) -> dict:
    """Per-crate own-context share; the crates whose name describes < half.

    Returns every crate that has `[[module]]` rows (`rows`, for the printed
    table) and the subset `misnamed` — `own * 2 < total`, i.e. the crate's name
    describes strictly less than half of what it holds.
    """
    root = Path(root)
    reg = registry_for(root)
    homes = _crate_homes(reg)
    total: Counter = Counter()
    own: Counter = Counter()
    for r in reg.get("module", []):
        rel = r.get("path")
        if not rel:
            continue
        crate = _row_crate(root, rel)
        n = r.get("lines", 0) or 0
        total[crate] += n
        if r.get("context") in homes.get(crate, set()):
            own[crate] += n
    rows = [{"crate": c, "own": own[c], "total": total[c],
             "homes": sorted(homes.get(c, set()))}
            for c in total if total[c] > 0]
    rows.sort(key=lambda r: (-r["total"], r["crate"]))
    mis = [r for r in rows if r["own"] * 2 < r["total"]]
    return {"rows": rows, "misnamed": mis}


def _tag_digest(reg: dict) -> str:
    """A digest of the tag table: module (path, context) + context homes."""
    parts = [f"{r.get('path')}\t{r.get('context')}" for r in reg.get("module", [])]
    for c in reg.get("context", []):
        for cr in c.get("crates", []):
            parts.append(f"home\t{c['id']}\t{cr}")
    return hashlib.sha256("\n".join(sorted(parts)).encode("utf-8")).hexdigest()[:12]


def _flag_value(args: list[str], flag: str) -> str | None:
    """The value of `--flag X` or `--flag=X`, else None."""
    for i, a in enumerate(args):
        if a == flag and i + 1 < len(args):
            return args[i + 1]
        if a.startswith(flag + "="):
            return a.split("=", 1)[1]
    return None


def _coverage_fixture(root: Path, tag_orphan: bool) -> None:
    """A member crate with two src files; the orphan's row is optional."""
    (root / "quality").mkdir(parents=True, exist_ok=True)
    reg = ('[[context]]\n'
           'id = "widget"\n'
           'kind = "core"\n'
           'crates = ["fixture-crate"]\n\n'
           '[[module]]\n'
           'path = "fixture-crate/src/lib.rs"\n'
           'context = "widget"\n'
           'lines = 1\n')
    if tag_orphan:
        reg += ('\n[[module]]\n'
                'path = "fixture-crate/src/orphan.rs"\n'
                'context = "widget"\n'
                'lines = 1\n')
    (root / "quality" / "DOMAINS.toml").write_text(reg, encoding="utf-8")
    crate = root / "fixture-crate"
    (crate / "src").mkdir(parents=True, exist_ok=True)
    (crate / "Cargo.toml").write_text(
        '[package]\nname = "fixture-crate"\nversion = "0.0.0"\n',
        encoding="utf-8")
    (crate / "src" / "lib.rs").write_text("pub struct A;\n", encoding="utf-8")
    (crate / "src" / "orphan.rs").write_text("pub struct B;\n", encoding="utf-8")


def _coverage_positive(root: Path) -> None:
    """A src file with no `[[module]]` row: caught as a coverage hole."""
    _coverage_fixture(root, tag_orphan=False)


def _coverage_negative(root: Path) -> None:
    """Every src file has a row: refused (no hole)."""
    _coverage_fixture(root, tag_orphan=True)


AXES.append({
    "id": "crate-lines",
    "detect": coverage_holes,
    "positive": _coverage_positive,
    "negative": _coverage_negative,
})


def _misnamed_fixture(root: Path, tag: str) -> None:
    """A member crate with one module, tagged `tag`, owned by `widget`."""
    (root / "quality").mkdir(parents=True, exist_ok=True)
    (root / "quality" / "DOMAINS.toml").write_text(
        '[[context]]\n'
        'id = "widget"\n'
        'kind = "core"\n'
        'crates = ["fixture-crate"]\n\n'
        '[[module]]\n'
        'path = "fixture-crate/src/lib.rs"\n'
        f'context = "{tag}"\n'
        'lines = 10\n',
        encoding="utf-8")
    crate = root / "fixture-crate"
    (crate / "src").mkdir(parents=True, exist_ok=True)
    (crate / "Cargo.toml").write_text(
        '[package]\nname = "fixture-crate"\nversion = "0.0.0"\n',
        encoding="utf-8")
    (crate / "src" / "lib.rs").write_text("pub struct A;\n", encoding="utf-8")


def _misnamed_positive(root: Path) -> None:
    """A crate whose only module is tagged another context: caught."""
    _misnamed_fixture(root, "other")


def _misnamed_negative(root: Path) -> None:
    """A crate whose module is its own context: refused."""
    _misnamed_fixture(root, "widget")


def _misnamed_detect(root: Path) -> list[dict]:
    """The axis's own function: truthy iff a crate describes < half itself."""
    return misnamed(root)["misnamed"]


AXES.append({
    "id": "misnamed",
    "detect": _misnamed_detect,
    "positive": _misnamed_positive,
    "negative": _misnamed_negative,
})


@subcommand("crate-lines")
def cmd_crate_lines(args: list[str]) -> int:
    """Print crate X's module rows and total lines, or the measurement line."""
    holes = coverage_holes(REPO)
    if holes:
        print("crate-lines: coverage hole — no [[module]] row for:",
              file=sys.stderr)
        for h in holes:
            print(f"  untagged: {h}", file=sys.stderr)
        return EXIT_COULD_NOT_JUDGE
    crate = _flag_value(args, "--crate")
    if not crate:
        print("usage: domains-census.py crate-lines --crate <name> [--json]",
              file=sys.stderr)
        return 2
    rows = crate_lines(REPO, crate)
    if not rows:
        print(f"crate-lines: no [[module]] rows for crate {crate!r}",
              file=sys.stderr)
        return EXIT_ARTIFACT_ABSENT
    total = sum(r.get("lines", 0) or 0 for r in rows)
    if "--json" in args:
        emit_measurement(total)
        return EXIT_OK
    print(f"crate-lines — {crate}: {len(rows)} module rows, {total} lines\n")
    for r in sorted(rows, key=lambda r: r.get("path", "")):
        print(f"  {r.get('context', '?'):<14} {r.get('lines', 0):>7}  "
              f"{r.get('path', '')}")
    print(f"\n  value: {total} lines")
    return EXIT_OK


@subcommand("misnamed")
def cmd_misnamed(args: list[str]) -> int:
    """Print per-crate own-context share, or the measurement line."""
    holes = coverage_holes(REPO)
    if holes:
        print("misnamed: coverage hole — no [[module]] row for:",
              file=sys.stderr)
        for h in holes:
            print(f"  untagged: {h}", file=sys.stderr)
        return EXIT_COULD_NOT_JUDGE
    r = misnamed(REPO)
    if "--json" in args:
        emit_measurement(len(r["misnamed"]))
        return EXIT_OK
    print("misnamed — share of each crate's lines carrying a context the crate "
          "is the home of\n  (a crate's name describes less than half of what "
          "it holds → counted)\n")
    for row in r["rows"]:
        share = row["own"] / row["total"] * 100
        flag = "  MISNAMED" if row["own"] * 2 < row["total"] else ""
        homes = ", ".join(row["homes"]) or "(no home)"
        print(f"  {row['crate']:<30} {homes:<22} {row['own']:>7} / "
              f"{row['total']:>7}  {share:5.1f}%{flag}")
    print(f"\n  tag-table digest: {_tag_digest(registry())}")
    print(f"  value: {len(r['misnamed'])} crates whose name describes less "
          f"than half of what they hold")
    return EXIT_OK


# ── queue — dm-queue ────────────────────────────────────────────────────────
#
# The loop's own progress bar (campaigns/domains.toml dm-queue): crates whose
# modules are not all tagged with the crate's own context, ordered by lines
# desc — the order the demolition loop pops. It reads the SAME table as
# `misnamed` (a crate's own context is its home list, never a name match;
# `unknown` is a legal tag that counts against the crate) at a different
# threshold: `misnamed` is DOMAINS.md §8 row 5's pre-registered prediction
# ("< half"), this is the loop's stop condition ("< 100%"), and it differs in
# threshold only. One computation, two readings (ARCH principle 8).
#
# THE COVERAGE ASSERTION PRECEDES THE COUNT, exactly as for `crate-lines` and
# `misnamed`: a crate whose `.rs` files are not all in the `[[module]]` census
# would silently understate the queue, so the subcommand exits 4 naming the
# untagged file. The whole-workspace census this bar needs landed in
# dm3-tag-workspace; a crate no context names still has its rows (`unknown`),
# so it is enqueued, not dropped.


def queue(root: Path) -> dict:
    """Crates not 100% their own context, ordered by lines desc.

    Built on `misnamed`'s per-crate table so the two bars cannot disagree about
    what a crate's own context is: a crate is enqueued when ANY line carries a
    context outside its home list (`own < total`).
    """
    r = misnamed(root)
    q = [row for row in r["rows"] if row["own"] < row["total"]]
    return {"queue": q, "rows": r["rows"]}


def _queue_detect(root: Path) -> list[dict]:
    """The axis's own function: truthy iff a crate is not 100% its own context."""
    return queue(root)["queue"]


def _queue_fixture(root: Path, all_own: bool) -> None:
    """A member crate with two modules, one always its home.

    The second is tagged `other` unless `all_own`, so `own` is HALF the lines:
    the crate is in the queue (`own < total`) but NOT misnamed (`own * 2 <
    total` is false). That is the queue axis's own threshold, which the
    misnamed fixture cannot exercise.
    """
    (root / "quality").mkdir(parents=True, exist_ok=True)
    other = "widget" if all_own else "other"
    (root / "quality" / "DOMAINS.toml").write_text(
        '[[context]]\n'
        'id = "widget"\n'
        'kind = "core"\n'
        'crates = ["fixture-crate"]\n\n'
        '[[module]]\n'
        'path = "fixture-crate/src/lib.rs"\n'
        'context = "widget"\n'
        'lines = 10\n\n'
        '[[module]]\n'
        'path = "fixture-crate/src/extra.rs"\n'
        f'context = "{other}"\n'
        'lines = 10\n',
        encoding="utf-8")
    crate = root / "fixture-crate"
    (crate / "src").mkdir(parents=True, exist_ok=True)
    (crate / "Cargo.toml").write_text(
        '[package]\nname = "fixture-crate"\nversion = "0.0.0"\n',
        encoding="utf-8")
    (crate / "src" / "lib.rs").write_text("pub struct A;\n", encoding="utf-8")
    (crate / "src" / "extra.rs").write_text("pub struct B;\n", encoding="utf-8")


def _queue_positive(root: Path) -> None:
    """A crate at half its own context: caught (in the queue)."""
    _queue_fixture(root, all_own=False)


def _queue_negative(root: Path) -> None:
    """A crate entirely its own context: refused (queue empty)."""
    _queue_fixture(root, all_own=True)


AXES.append({
    "id": "queue",
    "detect": _queue_detect,
    "positive": _queue_positive,
    "negative": _queue_negative,
})


@subcommand("queue")
def cmd_queue(args: list[str]) -> int:
    """Print the demolition queue, ordered by lines desc, or --json."""
    holes = coverage_holes(REPO)
    if holes:
        print("queue: coverage hole — no [[module]] row for:", file=sys.stderr)
        for h in holes:
            print(f"  untagged: {h}", file=sys.stderr)
        return EXIT_COULD_NOT_JUDGE
    r = queue(REPO)
    if "--json" in args:
        emit_measurement(len(r["queue"]))
        return EXIT_OK
    print("queue — crates whose modules are not all tagged with the crate's own "
          "context\n  (the loop's stop condition; ordered by lines desc, the "
          "order the loop pops)\n")
    for row in r["queue"]:
        share = row["own"] / row["total"] * 100
        homes = ", ".join(row["homes"]) or "(no home)"
        print(f"  {row['crate']:<30} {homes:<22} {row['own']:>7} / "
              f"{row['total']:>7}  {share:5.1f}%")
    print(f"\n  tag-table digest: {_tag_digest(registry())}")
    print(f"  value: {len(r['queue'])} crates not yet 100% their own context")
    return EXIT_OK


# ── congestion — dm-context-congestion ──────────────────────────────────────
#
# The campaign's one OUTCOME bar: distinct CONTEXTS touched per `.rs` commit,
# by AUTHOR month, over the registry's module tags — nc-congestion's shape at
# the context altitude (campaigns/domains.toml, the comment that held this bar
# back until its floor could be minted). The structural bars measure the
# demolition; this one measures whether the demolition bought anything, since
# a change that no longer needs to touch five contexts at once is the point.
#
# BUCKET BY AUTHOR DATE, NOT COMMITTER DATE. This history was rewritten around
# 2026-08-11 and committer dates cluster there (nc-congestion's header). The
# window is bounded by `git log --since` — a COMMITTER-date pre-filter — and
# then filtered by the author date in `%aI`, so a rewritten commit is bucketed
# where its author wrote it, not where the rewrite put it.
#
# THE WINDOW IS THE LAST 90 DAYS, AND THE VALUE IS THE AUTHOR-MONTH MEAN: the
# mean distinct-contexts-per-commit within each author month, averaged over the
# months the window holds (each month weighted equally, so a partial month does
# not dominate). The per-month series is printed beside it. `--since` and the
# author-date filter both derive from the machine clock, so a fixture committed
# now is always inside its own window.
#
# A FILE WITH NO MODULE ROW IS UNTAGGED, NEVER GUESSED. The registry tags the
# CURRENT tree; a path that has since moved or been deleted has no row, so a
# commit's distinct-context count is over its tagged files and the untagged
# count is reported beside the value (ARCH principle 6 — absence is reported,
# never defaulted). This UNDER-counts historical congestion, which is the
# direction a hold-bar can tolerate: it can never manufacture a pass.
#
# TWO CONSTANTS THE REGISTRY CANNOT SUPPLY, the same standing as peer-outside's
# word and atom-outside's roots: the window length (90 days, spelled in the
# objective and the bar) and the shape of a `git log` header. Everything else —
# the contexts, the tags — is data.
_WINDOW_DAYS = 90
_CONGESTION_HEADER = re.compile(r"^([0-9a-f]{40}) (\S+)$")


def _git_congestion_rows(root: Path, cutoff: str) -> list[tuple]:
    """[(sha, month, {contexts}, untagged_files, rs_files)] per `.rs` commit.

    One row per commit that touches a tracked `*.rs` file and whose AUTHOR date
    is on or after `cutoff`. A commit whose every changed `.rs` file is
    untagged still yields a row (with an empty context set and every file
    untagged), so absence is reported rather than dropped.
    """
    root = Path(root)
    try:
        r = subprocess.run(
            ["git", "log", f"--since={cutoff}", "--pretty=format:%H %aI",
             "--name-only", "--", "*.rs"],
            cwd=root, capture_output=True, text=True)
    except OSError:
        return []
    if r.returncode != 0:
        return []
    reg = registry_for(root)
    rows: list[tuple] = []
    state: dict = {"sha": None, "month": None, "author": None, "files": set()}

    def flush() -> None:
        sha, author = state["sha"], state["author"]
        files = state["files"]
        if sha is None or not files or author is None or author[:10] < cutoff:
            return
        ctxs: set[str] = set()
        untagged = 0
        for f in files:
            c = _module_context(reg, f)
            if c is None:
                untagged += 1
            else:
                ctxs.add(c)
        rows.append((sha, state["month"], ctxs, untagged, len(files)))

    for line in r.stdout.splitlines():
        line = line.rstrip()
        if not line:
            continue
        m = _CONGESTION_HEADER.match(line)
        if m:
            flush()
            state = {"sha": m.group(1), "author": m.group(2),
                     "month": m.group(2)[:7], "files": set()}
            continue
        state["files"].add(line)
    flush()
    return rows


def congestion(root: Path, since_days: int = _WINDOW_DAYS) -> dict | None:
    """The author-month congestion table at `root`, or None with no history.

    `months` maps a month to `{n, mean, untagged, files}` (means are per-commit
    averages within the month). `value` is the mean of the monthly means,
    rounded to 2. `congested` lists the commits touching two or more distinct
    contexts — the axis's own positive.
    """
    cutoff = (_dt.date.today() - _dt.timedelta(days=since_days)).isoformat()
    rows = _git_congestion_rows(root, cutoff)
    if not rows:
        return None
    per_month: dict[str, list[tuple]] = {}
    for _sha, month, ctxs, untagged, nfiles in rows:
        per_month.setdefault(month, []).append((len(ctxs), untagged, nfiles))
    months: dict[str, dict] = {}
    for mo in sorted(per_month):
        vals = per_month[mo]
        months[mo] = {
            "n": len(vals),
            "mean": sum(v[0] for v in vals) / len(vals),
            "untagged": sum(v[1] for v in vals) / len(vals),
            "files": sum(v[2] for v in vals) / len(vals),
        }
    value = sum(m["mean"] for m in months.values()) / len(months)
    congested = [{"sha": s, "month": mo, "contexts": len(c), "files": n}
                 for s, mo, c, _u, n in rows if len(c) >= 2]
    return {"months": months, "value": round(value, 2), "commits": len(rows),
            "congested": congested, "cutoff": cutoff}


def _congestion_detect(root: Path) -> list[dict]:
    """The axis's own function: truthy iff a commit touches 2+ contexts."""
    c = congestion(root)
    return c["congested"] if c else []


def _congestion_fixture(root: Path, cross_context: bool) -> None:
    """A one-commit git repo; the commit crosses contexts or stays in one.

    `alpha` tags two files and `beta` one. The cross-context commit stages one
    `alpha` file and the `beta` file (two contexts); the single-context commit
    stages BOTH `alpha` files — so the negative catches a distinct-count that
    counts files rather than contexts.
    """
    (root / "quality").mkdir(parents=True, exist_ok=True)
    (root / "quality" / "DOMAINS.toml").write_text(
        '[[module]]\n'
        'path = "fixture/a.rs"\n'
        'context = "alpha"\n\n'
        '[[module]]\n'
        'path = "fixture/a2.rs"\n'
        'context = "alpha"\n\n'
        '[[module]]\n'
        'path = "fixture/b.rs"\n'
        'context = "beta"\n', encoding="utf-8")
    (root / "fixture").mkdir(parents=True, exist_ok=True)
    for name in ("a.rs", "a2.rs", "b.rs"):
        (root / "fixture" / name).write_text("pub struct A;\n", encoding="utf-8")
    subprocess.run(["git", "init", "-q"], cwd=root)
    touched = (["fixture/a.rs", "fixture/b.rs"] if cross_context
               else ["fixture/a.rs", "fixture/a2.rs"])
    subprocess.run(["git", "add", *touched], cwd=root)
    subprocess.run(
        ["git", "-c", "user.email=census@test", "-c", "user.name=census",
         "-c", "commit.gpgsign=false", "commit", "-q", "-m", "fixture"],
        cwd=root)


def _congestion_positive(root: Path) -> None:
    """A commit touching two contexts: caught as congested."""
    _congestion_fixture(root, cross_context=True)


def _congestion_negative(root: Path) -> None:
    """A commit touching two files of ONE context: refused."""
    _congestion_fixture(root, cross_context=False)


AXES.append({
    "id": "congestion",
    "detect": _congestion_detect,
    "positive": _congestion_positive,
    "negative": _congestion_negative,
})


@subcommand("congestion")
def cmd_congestion(args: list[str]) -> int:
    """Print the author-month congestion table, or the measurement line."""
    c = congestion(REPO)
    if c is None:
        print(f"congestion: no `.rs` commits in the last {_WINDOW_DAYS} days — "
              "value NOT reported", file=sys.stderr)
        return EXIT_ARTIFACT_ABSENT
    if "--json" in args:
        emit_measurement(c["value"])
        return EXIT_OK
    print("congestion — distinct contexts touched per `.rs` commit, AUTHOR "
          f"month\n  (window: last {_WINDOW_DAYS} days, author date; a file "
          "with no module row is untagged and reported)\n")
    print(f"  {'month':9} {'mean':>6} {'untagged':>9} {'files':>7}  n")
    for mo, d in c["months"].items():
        print(f"  {mo:9} {d['mean']:>6.2f} {d['untagged']:>9.2f} "
              f"{d['files']:>7.2f}  {d['n']}")
    print(f"\n  value: {c['value']} (author-month mean over "
          f"{len(c['months'])} months, {c['commits']} commits; "
          f"{len(c['congested'])} commits touched 2+ contexts)")
    return EXIT_OK


# ── liftable — dm-contexts-liftable ─────────────────────────────────────────
#
# A context is liftable when someone outside this repository can take it alone
# and get value (DOMAINS.md §2). The mechanism already exists: a `[[package]]`
# row in `quality/ARCH_LAYERS.toml` that `cargo xtask boundary-gate` passes, and
# a `lift` script that copies the closure out of the monorepo and runs it. The
# instrument reports the two as two numbers — `gated` (a package row exists) and
# `lifted` (a declared lift passed) — because the goodhart's failure mode is a
# context packaged as one crate that still reaches everything through the shared
# `[[package_leaf]]` budget: a passing gate with no lift.
#
# THE KEPT-CONTEXT SET IS DERIVED, NOT LISTED. The registry's `[[context]]`
# table is the closed set, but its "Not contexts" section (kernel, host,
# back-of-house) marks three rows that are not domains — and none of the three
# owns a word, which is DOMAINS.md §4's own definition ("named by the word it
# owns exclusively"). So the set is the kept rows that own at least one word:
# 11 today, and a merge under the §8 kill bar is a registry row edit, not a
# constant here.
#
# THE VALUE. Floor 3 is the gate reading of 2026-09-13 (Fabric and Compute via
# `commonwealth`, Workbench via `code-intel`). The goodhart refines it: a
# context that DECLARES a lift counts only when that lift has passed. So a
# context contributes 1 when it is gated, EXCEPT that a context with a declared
# `lift` contributes only when its lift passed — read from the lift's own
# artifact when it is not run, from a live run when it is. Run-tier and
# read-tier therefore agree on today's tree (3), and a declared lift that fails
# drops the value, which is the goodhart made arithmetic.
#
# THE LIFT SCRIPTS ARE INVENTORY THAT CAN ONLY ABSTAIN WITHOUT AN INVITE
# (commonwealth/BOUNDARY.md "Tier 2"), so the read tier is the default: a live
# run happens only when the campaign row sets a `timeout_s` (the bar's own
# `timeout_s` decides, and `--no-lift` forces the read tier regardless).
_DEFAULT_LIFT_TIMEOUT_S = 120


def arch_packages_for(root: Path) -> set[str]:
    """The `[[package]]` names in ARCH_LAYERS.toml, fixture-aware.

    A fixture with no `quality/ARCH_LAYERS.toml` falls back to the repo's, so an
    axis can be driven against a bare source fixture; the controls write their
    own, so the fallback never blurs a planted case.
    """
    root = Path(root)
    p = root / "quality" / "ARCH_LAYERS.toml"
    if not p.exists():
        p = REPO / "quality" / "ARCH_LAYERS.toml"
    try:
        with open(p, "rb") as f:
            data = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        return set()
    return {pkg.get("name") for pkg in data.get("package", []) if pkg.get("name")}


def kept_contexts(reg: dict) -> list[dict]:
    """The kept contexts that are domains: `status = "kept"` and owns a word."""
    return [c for c in reg.get("context", [])
            if c.get("status") == "kept"
            and (c.get("owns") or c.get("owns_exact"))]


def _lift_artifact(cmd: str) -> Path | None:
    """The `last.json` a `*-lift.sh` script writes, derived from its name.

    Both lift scripts emit their verdict to `target/<script-stem>/last.json`
    (`verdict()` in scripts/cw-rails-lift.sh:83, scripts/cw-work-lift.sh:79), so
    the read tier can report a recorded lift without re-running it — a
    measurement, not a substitution. The command's first token is the script;
    anything else (a future non-script lift) has no artifact to read.
    """
    tokens = cmd.split()
    if not tokens or not tokens[0].endswith(".sh"):
        return None
    return REPO / "target" / Path(tokens[0]).stem / "last.json"


def _read_lift_artifact(cmd: str) -> bool | None:
    """The recorded lift verdict, or None when there is none to read."""
    art = _lift_artifact(cmd)
    if art is None or not art.exists():
        return None
    try:
        return json.loads(art.read_text(encoding="utf-8")).get("value") == 1
    except (OSError, json.JSONDecodeError):
        return None


def _run_lift(cmd: str, timeout_s: int) -> tuple[bool | None, str]:
    """Run one declared lift; (lifted, note). None means no claim was made.

    `rc 0` alone is NOT success: cw-work-lift.sh exits 0 with `{"value": 0}` on
    a measured failure (its own header is the contract), so a JSON value is read
    when the script prints one and `rc` is the fallback for a script that does
    not. A timeout makes no claim (ARCH principle 6).
    """
    try:
        r = subprocess.run(["bash", "-c", cmd], cwd=REPO, capture_output=True,
                           text=True, timeout=timeout_s)
    except subprocess.TimeoutExpired:
        return None, f"timeout {timeout_s}s"
    lines = [ln.strip() for ln in r.stdout.splitlines() if ln.strip()]
    if lines and lines[-1].startswith("{"):
        try:
            value = json.loads(lines[-1]).get("value")
            if isinstance(value, (int, float)) and not isinstance(value, bool):
                return value == 1, f"exit {r.returncode}, value {value}"
        except json.JSONDecodeError:
            pass
    return r.returncode == 0, f"exit {r.returncode}"


def liftable(root: Path, run_lifts: bool = False,
             timeout_s: int = _DEFAULT_LIFT_TIMEOUT_S) -> dict:
    """Classify every kept context as gated / lifted; return the rows and value.

    `run_lifts=False` is the read tier: a declared lift is reported from its
    last artifact when one exists and is otherwise NOT RUN (never a fabricated
    pass). `run_lifts=True` executes it. The value counts a context when it is
    gated, except that a declared-lift context counts only when the lift passed
    — or, in the read tier with no artifact, by its gate, since that is the only
    reading available without running it.
    """
    root = Path(root)
    reg = registry_for(root)
    packages = arch_packages_for(root)
    rows: list[dict] = []
    for c in kept_contexts(reg):
        pkg = c.get("package", "") or ""
        gated = bool(pkg) and pkg in packages
        cmd = c.get("lift", "") or ""
        declared = bool(cmd)
        lifted: bool | None = None
        note = "—"
        if declared:
            if run_lifts:
                lifted, note = _run_lift(cmd, timeout_s)
            else:
                lifted = _read_lift_artifact(cmd)
                note = "artifact" if lifted is not None else "not-run"
        if not declared:
            counted = gated
        elif lifted is not None:
            counted = lifted
        else:
            counted = gated          # read tier, no artifact: the gate reading
        rows.append({
            "context": c["id"], "package": pkg, "gate": "gated" if gated else "—",
            "lift": ("lifted" if lifted is True else "failed" if lifted is False
                     else note if declared else "—"),
            "applicable_as": c.get("applicable_as", ""), "counted": counted,
            "declared": declared, "lifted": lifted, "gated": gated,
        })
    value = sum(1 for r in rows if r["counted"])
    return {"rows": rows, "value": value,
            "gated": sum(1 for r in rows if r["gated"]),
            "lifted": sum(1 for r in rows if r["lifted"] is True)}


def _liftable_gaps(root: Path) -> list[dict]:
    """The axis's own function: truthy iff a kept context is not liftable."""
    return [r for r in liftable(root, run_lifts=False)["rows"] if not r["counted"]]


def _liftable_fixture(root: Path, package: str) -> None:
    """A kept domain context owning a word, and a package row to match it."""
    (root / "quality").mkdir(parents=True, exist_ok=True)
    (root / "quality" / "DOMAINS.toml").write_text(
        '[[context]]\n'
        'id = "widget"\n'
        'kind = "supporting"\n'
        'owns = ["Sprocket"]\n'
        f'package = "{package}"\n'
        'lift = ""\n'
        'status = "kept"\n'
        'applicable_as = "a widget you can hand a stranger"\n'
        '\n'
        '[[context]]\n'
        'id = "kernel"\n'
        'kind = "published-language"\n'
        'owns = []\n'
        'package = ""\n'
        'status = "kept"\n'
        'applicable_as = "not applicable by design"\n', encoding="utf-8")
    (root / "quality" / "ARCH_LAYERS.toml").write_text(
        '[[package]]\n'
        'name = "fixture-package"\n'
        'crates = ["fixture-crate"]\n', encoding="utf-8")


def _liftable_positive(root: Path) -> None:
    """A kept context with no package: caught as a gap.

    The kernel row is the negative control INSIDE the positive fixture: it owns
    no word, so it is not a kept domain and must never be reported as a gap —
    without it the owns-filter is untested.
    """
    _liftable_fixture(root, "")


def _liftable_negative(root: Path) -> None:
    """A kept context whose package is in ARCH_LAYERS: refused (no gap)."""
    _liftable_fixture(root, "fixture-package")


AXES.append({
    "id": "liftable",
    "detect": _liftable_gaps,
    "positive": _liftable_positive,
    "negative": _liftable_negative,
})


def _lift_run_fixture(root: Path, rc: int) -> None:
    """A kept domain context declaring a lift whose stub exits `rc`.

    The declared `lift` is a shell command run from the repo root
    (`_run_lift`), so the stub is referenced by absolute path. `package = ""`
    keeps the read tier out of it: only the run tier can score this row, which
    is the branch under test.
    """
    (root / "quality").mkdir(parents=True, exist_ok=True)
    stub = root / "stub-lift.sh"
    stub.write_text(f"#!/usr/bin/env bash\nexit {rc}\n", encoding="utf-8")
    (root / "quality" / "DOMAINS.toml").write_text(
        '[[context]]\n'
        'id = "widget"\n'
        'kind = "supporting"\n'
        'owns = ["Sprocket"]\n'
        'package = ""\n'
        f'lift = "bash {stub}"\n'
        'status = "kept"\n'
        'applicable_as = "a widget you can hand a stranger"\n', encoding="utf-8")
    (root / "quality" / "ARCH_LAYERS.toml").write_text(
        '[[package]]\n'
        'name = "fixture-package"\n'
        'crates = ["fixture-crate"]\n', encoding="utf-8")


def _lift_run_detect(root: Path) -> list[dict]:
    """The axis's own function: truthy iff a declared lift RAN and passed."""
    return [r for r in liftable(root, run_lifts=True)["rows"]
            if r["lifted"] is True]


def _lift_run_positive(root: Path) -> None:
    """A stub lift exiting 0: run and read as lifted (caught)."""
    _lift_run_fixture(root, 0)


def _lift_run_negative(root: Path) -> None:
    """A stub lift exiting non-zero: run and read as not lifted (refused)."""
    _lift_run_fixture(root, 3)


AXES.append({
    "id": "lift-run",
    "detect": _lift_run_detect,
    "positive": _lift_run_positive,
    "negative": _lift_run_negative,
})


def _bar_timeout_s(instrument_token: str) -> int | None:
    """The campaign row's `timeout_s` for the bar that names this subcommand.

    Read from `quality/campaigns/domains.toml` rather than typed here, because
    whether a lift may be RUN is the campaign's call: a row with a `timeout_s`
    is run-tier, a row without one is read-tier (`co-lineage.py:616` caps the
    read tier at 10 s), and the instrument follows the row.
    """
    p = REPO / "quality" / "campaigns" / "domains.toml"
    try:
        with open(p, "rb") as f:
            data = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        return None
    for b in data.get("bar", []):
        if instrument_token in (b.get("instrument") or ""):
            return b.get("timeout_s")
    return None


@subcommand("liftable")
def cmd_liftable(args: list[str]) -> int:
    """Print the per-context gated/lifted table, or the measurement line."""
    token = "domains-census.py liftable"
    timeout_s = _bar_timeout_s(token)
    run_lifts = timeout_s is not None and "--no-lift" not in args
    r = liftable(REPO, run_lifts=run_lifts,
                 timeout_s=timeout_s or _DEFAULT_LIFT_TIMEOUT_S)
    if "--json" in args:
        emit_measurement(r["value"])
        return EXIT_OK
    mode = "run tier (lifts executed)" if run_lifts else "read tier (lifts not run)"
    print("liftable — every kept context as a package the gate passes, and a "
          "lift that runs\n  (gated = a [[package]] row exists; lifted = a "
          "declared lift passed)\n")
    for row in r["rows"]:
        pkg = row["package"] or "—"
        print(f"  {row['context']:<15} {pkg:<13} gate={row['gate']:<6} "
              f"lift={row['lift']:<10} {row['applicable_as']}")
    print(f"\n  {r['gated']} gated / {r['lifted']} lifted of "
          f"{len(r['rows'])} kept contexts  [{mode}]")
    print(f"\n  value: {r['value']} contexts a stranger could take alone")
    return EXIT_OK


# ── predicate — the campaign's objective ────────────────────────────────────
#
# The sentence `quality/campaigns/domains.toml` `[predicate]` declares, as a
# program a runner can call. TWO CONJUNCTS, both REUSED, neither re-derived
# (ARCH principle 8): every kept context names an ARCH_LAYERS `[[package]]`
# (the same `gated` reading dm-contexts-liftable counts) and no crate outside
# the commonwealth package defines a `Peer*` type (the same `peer_defs` reading
# dm-peer-outside-fabric counts). The statement's third clause — that
# `cargo xtask boundary-gate` PASSES those packages — is the campaign check's
# own `&&` (quality/campaigns/domains.toml:41), because this script shells
# cargo for nothing but the declared lifts ("Seams": grep-shaped, no cargo).
#
# EXIT 1, NOT "NON-ZERO". `co-lineage.py::evaluate_predicate` reads 0 as TRUE,
# 1 as FALSE, and any other code as COULD-NOT-RUN; a predicate that is false is
# not one that could not run, and rendering it as the fourth verdict would be
# the "abstention with no run that demanded it" the smell table forbids. The
# reasons print BEFORE the judgement line, so the `detail` co-lineage captures
# from the first output line is the WHY, not a header.
#
# THE PREDICATE IS FALSE ON TODAY'S TREE, AND THAT IS THE POINT. A predicate
# that cannot be watched failing is not a predicate (domains-3-instrument,
# Objective). Its two halves are each already watched failing by their own
# axis's planted controls in `--self-test`, so the composition is not shipped
# blind either (ARCH principles 5 and 7).


def predicate_problems(root: Path) -> dict:
    """The two conjuncts, each as the list it fails on (empty list = holds)."""
    root = Path(root)
    reg = registry_for(root)
    packages = arch_packages_for(root)
    ungated = [c["id"] for c in kept_contexts(reg)
               if not ((c.get("package") or "") and c["package"] in packages)]
    return {"ungated": ungated, "peers": peer_defs(root)}


@subcommand("predicate")
def cmd_predicate(args: list[str]) -> int:
    """The campaign predicate: every kept context packaged, no outside Peer*."""
    p = predicate_problems(REPO)
    ungated, peers = p["ungated"], p["peers"]
    by_crate: dict[str, int] = {}
    for d in peers:
        by_crate[d["crate"]] = by_crate.get(d["crate"], 0) + 1
    ok = not ungated and not peers
    if ok:
        print("predicate — every kept context names an ARCH_LAYERS package and "
              "no crate outside commonwealth/ defines a Peer* type")
    else:
        print(f"predicate FALSE — {len(ungated)} kept contexts name no package; "
              f"{len(peers)} Peer* definitions outside commonwealth/")
        if ungated:
            print(f"\n  kept contexts with no ARCH_LAYERS [[package]] "
                  f"({len(ungated)}):")
            for c in ungated:
                print(f"    {c}")
        if peers:
            print(f"\n  Peer* definitions outside commonwealth/ "
                  f"({len(peers)} in {len(by_crate)} crates):")
            for crate in sorted(by_crate):
                print(f"    {crate}: {by_crate[crate]}")
    if ok:
        emit_judgement(
            "domains-census", "passed",
            "every kept context names an ARCH_LAYERS package and no crate "
            "outside commonwealth/ defines a Peer* type")
        return EXIT_OK
    emit_judgement(
        "domains-census", "failed",
        f"{len(ungated)} kept contexts name no package; {len(peers)} Peer* "
        f"definitions outside commonwealth/ in {len(by_crate)} crates")
    return 1


# ── plan — the move plan ────────────────────────────────────────────────────
#
# The demolition loop's per-crate plan (campaigns/domains.toml, THE STRATEGY;
# domains-3-instrument "Re-sequenced 2026-09-14" bullet `plan`). For the queue
# head — or `--crate X` — it reads that crate's `[[cluster]]` rows, re-derives
# what the tree can answer, diffs the two, orders the clusters (leaves first,
# then size) and asserts each move.
#
# THE REGISTRY IS THE DECIDER, THE TREE IS THE CROSS-CHECK. Every number the
# loop acts on (lines, the tier window, shim sites, the destination) is a
# `[[cluster]]` row — DATA, so a move is a diff a reviewer reads. The tree
# re-derivation is the instrument's own audit of those rows: lines and files
# from `wc -l` over the crate's `src/` under the `[[module]]` tags, in-crate
# edges from `crate::` references resolved to the module they name, and
# external consumers from `<crate_ident>::` outside the crate. A disagreement
# between the two is a FINDING printed beside the move (the row's banner says
# the rows predate the 2026-09-14 retags), never a silent overwrite — that is
# ARCH principle 4 and the reason the plan is worth running at all.
#
# THE WINDOW IS [own_dep_max_tier, consumer_min_tier] inclusive (the
# corpus-engine banner's own definition). A destination is legal when its tier
# sits inside it; `window_after_port` is the band once the port the design
# names lands, so a cluster legal only after its port is printed as such. A
# window whose floor is ABOVE its ceiling is EMPTY: the cluster is two
# clusters or owes a port, and the plan prints KNOT with the crossing symbols
# and the fix class, never a destination (campaign.md "Ambiguity policy";
# dm-queue kill).
#
# THE ASSERTS PER MOVE, all four named by the order: dest tier in the window;
# dest named by the context's `crates` list; no new `[[exception]]` (checked
# against ARCH_LAYERS' own forbid/exception ledger); the crate's own-context
# share monotone. All four are hard: `share_monotone` reads the registry's
# proposal (a cluster whose `dest` does not name the crate is a leaver) and a
# leaver that is one of the crate's own contexts fails loudly, because moving
# own lines out is the one thing that drops the share (ARCH 5).
_PLAN_REF = re.compile(
    r"crate::([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)")
_WS_CRATE_REF = re.compile(r"\b([a-z][a-z0-9_]*)::")
_NAMED_CRATE = re.compile(r"\b[a-z][a-z0-9]*(?:-[a-z0-9]+)+\b")


def _arch(root: Path) -> dict:
    """The layer/package/forbid/exception tables of ARCH_LAYERS.toml.

    A fixture with no ARCH_LAYERS falls back to the repo's, exactly as
    `arch_packages_for` does, so a planted case that does not care about tiers
    still runs against a real ledger.
    """
    p = Path(root) / "quality" / "ARCH_LAYERS.toml"
    if not p.exists():
        p = REPO / "quality" / "ARCH_LAYERS.toml"
    try:
        with open(p, "rb") as f:
            data = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        return {"layers": [], "packages": set(), "forbids": [], "exceptions": []}
    return {
        "layers": [(i, layer.get("crates", []))
                   for i, layer in enumerate(data.get("layer", []))],
        "packages": {pkg.get("name") for pkg in data.get("package", [])
                     if pkg.get("name")},
        "forbids": list(data.get("forbid", [])),
        "exceptions": list(data.get("exception", [])),
    }


def _crate_tier(arch: dict, crate: str) -> int | None:
    """The layer index naming `crate`, or None (a glob may name it, too)."""
    for i, crates in arch["layers"]:
        for pat in crates:
            if fnmatch.fnmatch(crate, pat):
                return i
    return None


def _crate_exists(root: Path, arch: dict, crate: str) -> bool:
    """True when a workspace member or an ARCH_LAYERS package carries the name.

    A layer glob naming a crate the tree has not created (sovereign-scheduler
    today) is NOT existence — `cargo xtask boundary-gate` cannot see it.
    """
    if crate in arch["packages"]:
        return True
    return any(name == crate for name, _d in _workspace_crates(root))


def _as_int(v) -> int | None:
    if isinstance(v, bool):
        return None
    if isinstance(v, int):
        return v
    if isinstance(v, str):
        m = re.search(r"-?\d+", v)
        return int(m.group()) if m else None
    return None


def _window_pair(row: dict, key: str = "window") -> tuple[int, int] | None:
    """A `window`/`window_after_port` as an inclusive (lo, hi), else None."""
    w = row.get(key)
    if isinstance(w, list) and len(w) >= 2:
        lo, hi = _as_int(w[0]), _as_int(w[1])
        return (lo, hi) if lo is not None and hi is not None else None
    if isinstance(w, str):
        nums = [int(n) for n in re.findall(r"\d+", w)]
        if len(nums) >= 2:
            return nums[0], nums[1]
    lo, hi = _as_int(row.get("own_dep_max_tier")), _as_int(
        row.get("consumer_min_tier"))
    if lo is not None and hi is not None:
        return lo, hi
    return None


def _dest_tiers(row: dict) -> list[int]:
    """The `dest_tier` as a list of ints: `5`, `"3"`, `"0 / 3"` all read."""
    dt = row.get("dest_tier")
    if isinstance(dt, bool):
        return []
    if isinstance(dt, int):
        return [dt]
    if isinstance(dt, str):
        return [int(n) for n in re.findall(r"\d+", dt)]
    return []


def _cluster_imports(row: dict, key: str) -> dict[str, int]:
    """`imports_clusters` / `imported_by_clusters` in either registry shape.

    corpus-engine writes a list of `"context:n"` strings; sovereign-mesh and
    sovereign-api write a list of `{context, sites}` tables (or, for
    `imported_by_clusters`, a `{context = n}` dict). One reader for all three.
    """
    raw = row.get(key, [])
    out: dict[str, int] = {}
    if isinstance(raw, dict):
        for k, v in raw.items():
            out[k] = out.get(k, 0) + (_as_int(v) or 0)
    elif isinstance(raw, list):
        for e in raw:
            if isinstance(e, str):
                ctx, _, n = e.partition(":")
                out[ctx] = out.get(ctx, 0) + (int(n) if n.isdigit() else 0)
            elif isinstance(e, dict) and e.get("context"):
                out[e["context"]] = out.get(e["context"], 0) + (
                    _as_int(e.get("sites")) or 0)
    return out


def _cluster_external(row: dict) -> list[dict]:
    """`external_consumers` normalized to `{crate, tier, sites}` rows."""
    raw = row.get("external_consumers")
    out: list[dict] = []
    if isinstance(raw, list):
        for e in raw:
            if isinstance(e, dict) and e.get("crate"):
                out.append({"crate": e["crate"], "tier": _as_int(e.get("tier")),
                            "sites": _as_int(e.get("sites")) or 0})
    elif isinstance(raw, dict):
        for k, v in raw.items():
            if isinstance(v, dict):
                out.append({"crate": k, "tier": _as_int(v.get("tier")),
                            "sites": _as_int(v.get("sites")) or 0})
            else:
                out.append({"crate": k, "tier": None,
                            "sites": _as_int(v) or 0})
    elif isinstance(raw, str):
        m = re.search(r"(\d+)\s*sites?\s*/\s*\d+\s*crates?:\s*(.*)", raw)
        if m:
            for part in m.group(2).split(","):
                mm = re.match(r"\s*([\w-]+)\(t(\d+)\):(\d+)", part)
                if mm:
                    out.append({"crate": mm.group(1), "tier": int(mm.group(2)),
                                "sites": int(mm.group(3))})
    return out


def _context_crates(reg: dict) -> dict[str, list[str]]:
    return {c["id"]: list(c.get("crates", [])) for c in reg.get("context", [])}


def _crate_dir_of(root: Path, crate: str) -> Path | None:
    for name, d in _workspace_crates(root):
        if name == crate:
            return d
    return None


def _ctx_lines_from_tree(root: Path, crate: str, reg: dict) -> dict[str, list]:
    """`{context: [lines, files]}` for the crate's `src/`, read from the tree.

    This is the re-derivation of the cluster rows' `lines`/`files`: the
    `[[module]]` tag decides a file's context, `wc -l` (via splitlines) the
    size. A file the crate's tags do not describe lands under `unknown`, which
    the coverage assertion elsewhere already refuses.
    """
    root = Path(root)
    cdir = _crate_dir_of(root, crate)
    out: dict[str, list] = {}
    if cdir is None:
        return out
    try:
        src = cdir.relative_to(root).as_posix() + "/src/"
    except ValueError:
        return out
    for path in _rs_files(root):
        try:
            rel = path.relative_to(root).as_posix()
        except ValueError:
            continue
        if not rel.startswith(src):
            continue
        try:
            n = len(path.read_text(encoding="utf-8",
                                   errors="replace").splitlines())
        except OSError:
            continue
        key = _module_context(reg, rel) or "unknown"
        row = out.setdefault(key, [0, 0])
        row[0] += n
        row[1] += 1
    return out


def _resolve_crate_ref(crate_dir: Path, segs: list[str]) -> Path | None:
    """The module file `crate::a::b::…` names: the longest existing prefix.

    `crate::decision_log::DecisionEvent` resolves to `src/decision_log.rs`
    (the module) with `DecisionEvent` the item; `crate::routes_internal::x`
    resolves to `src/routes_internal/mod.rs`. A path whose every prefix is a
    file (a nested item) stops at the deepest existing module.
    """
    src = crate_dir / "src"
    for i in range(len(segs), 0, -1):
        base = src.joinpath(*segs[:i])
        if base.with_suffix(".rs").exists():
            return base.with_suffix(".rs")
        if (base / "mod.rs").exists():
            return base / "mod.rs"
    return None


def _tree_in_crate_edges(root: Path, crate: str, reg: dict) -> dict:
    """Re-derive `(src_context -> dst_context) -> [symbols]` from `crate::`.

    Grep-shaped (the instrument's whole posture): every `crate::…` reference
    in the crate's `src/` is resolved to the module it names, that module's
    `[[module]]` tag is the destination context, and the pair is counted.
    `mod x;` lines carry no `crate::` and dissolve with a move, so they are
    excluded by construction (the corpus-engine banner's method).
    """
    root = Path(root)
    cdir = _crate_dir_of(root, crate)
    edges: dict = {}
    if cdir is None:
        return edges
    try:
        src = cdir.relative_to(root).as_posix() + "/src/"
    except ValueError:
        return edges
    for path in _rs_files(root):
        try:
            rel = path.relative_to(root).as_posix()
        except ValueError:
            continue
        if not rel.startswith(src):
            continue
        src_ctx = _module_context(reg, rel) or "unknown"
        try:
            lines = path.read_text(encoding="utf-8",
                                   errors="replace").splitlines()
        except OSError:
            continue
        for line in lines:
            code = code_part(line)
            if "crate::" not in code:
                continue
            for m in _PLAN_REF.finditer(code):
                segs = m.group(1).split("::")
                resolved = _resolve_crate_ref(cdir, segs)
                if resolved is None:
                    continue
                try:
                    drel = resolved.relative_to(root).as_posix()
                except ValueError:
                    continue
                dst_ctx = _module_context(reg, drel) or "unknown"
                if dst_ctx == src_ctx:
                    continue
                edges.setdefault((src_ctx, dst_ctx), []).append(m.group(1))
    return edges


def _tree_external_consumers(root: Path, crate: str, arch: dict) -> dict:
    """`{consumer_crate: {sites, tier}}` from `<ident>::` outside the crate.

    `git grep` over tracked `*.rs` (the tree path), comments stripped so a doc
    mention is not a consumer. A fixture that is not a work tree has no git
    grep and yields `{}` — absence reported, never a fabricated consumer.
    """
    root = Path(root)
    ident = crate.replace("-", "_")
    try:
        r = subprocess.run(
            ["git", "grep", "-nE", rf"{re.escape(ident)}::", "--", "*.rs"],
            cwd=root, capture_output=True, text=True)
    except OSError:
        return {}
    out: dict = {}
    for line in r.stdout.splitlines():
        parts = line.split(":", 2)
        if len(parts) < 3:
            continue
        path, _ln, content = parts
        if not code_part(content):
            continue
        cd = Path(_crate_dir(root / path, root)).name
        if cd == crate:
            continue
        entry = out.setdefault(cd, {"sites": 0, "tier": _crate_tier(arch, cd)})
        entry["sites"] += 1
    return out


def _tree_own_deps(root: Path, crate: str, ctx: str, reg: dict,
                   arch: dict) -> dict:
    """`{crate: tier}` this cluster's files name, from in-repo `ident::` uses."""
    root = Path(root)
    cdir = _crate_dir_of(root, crate)
    if cdir is None:
        return {}
    known = {}
    for name, _d in _workspace_crates(root):
        t = _crate_tier(arch, name)
        if t is not None and name != crate:
            known[name.replace("-", "_")] = (name, t)
    try:
        src = cdir.relative_to(root).as_posix() + "/src/"
    except ValueError:
        return {}
    found: dict = {}
    for path in _rs_files(root):
        try:
            rel = path.relative_to(root).as_posix()
        except ValueError:
            continue
        if not rel.startswith(src) or _module_context(reg, rel) != ctx:
            continue
        try:
            lines = path.read_text(encoding="utf-8",
                                   errors="replace").splitlines()
        except OSError:
            continue
        for line in lines:
            code = code_part(line)
            if "::" not in code:
                continue
            for m in _WS_CRATE_REF.finditer(code):
                hit = known.get(m.group(1))
                if hit:
                    found[hit[0]] = hit[1]
    return found


def _renames_riding(root: Path, crate: str, ctx: str, reg: dict) -> list[dict]:
    """Registry `[[noun]]` rows whose rename rides this cluster's move.

    A noun row is riding when its `disposition` is a rename and its `file` is
    a module the crate's tags put in this cluster. The registry is the list
    (a rename is a row, not a constant), so this reads rather than re-derives.
    """
    out: list[dict] = []
    for n in reg.get("noun", []):
        if not str(n.get("disposition", "")).startswith("decided:rename"):
            continue
        f = n.get("file", "")
        if not f:
            continue
        rel = f.split(":")[0]
        if _row_crate(root, rel) != crate:
            continue
        if (_module_context(reg, rel) or "unknown") == ctx:
            out.append(n)
    return out


def _move_order(clusters: list[dict]) -> tuple[list[str], list[list[str]]]:
    """Topological move order over the cluster graph: leaves first, then size.

    The registry's in-crate edges are NOT a DAG (host <-> serving, compute <->
    serving), so the order is computed on the CONDENSATION: Tarjan-free mutual
    reachability groups the cycles, the components are topologically sorted
    (a component that imports no other moves first), and each component's
    members are ordered by lines desc. A multi-member component is returned as
    a CYCLE — the knot a port or a split has to cut — and never silently
    flattened into a size order that pretends it was acyclic.
    """
    ctxs = [c["context"] for c in clusters]
    lines = {c["context"]: c["lines"] for c in clusters}
    imports = {c["context"]: {k for k in c["imports"] if k in ctxs}
               for c in clusters}

    reach: dict[str, set[str]] = {}
    for s in ctxs:
        seen: set[str] = set()
        stack = [s]
        while stack:
            n = stack.pop()
            for m in imports.get(n, ()):
                if m in ctxs and m not in seen:
                    seen.add(m)
                    stack.append(m)
        reach[s] = seen
    scc_of: dict[str, int] = {}
    comps: list[list[str]] = []
    for s in ctxs:
        if s in scc_of:
            continue
        comp = [t for t in ctxs
                if (t == s or t in reach[s]) and (t == s or s in reach[t])]
        for t in comp:
            scc_of[t] = len(comps)
        comps.append(comp)

    comp_imports: dict[int, set[int]] = {i: set() for i in range(len(comps))}
    for s in ctxs:
        for m in imports.get(s, ()):
            if scc_of[s] != scc_of[m]:
                comp_imports[scc_of[s]].add(scc_of[m])

    order: list[str] = []
    cycles: list[list[str]] = []
    remaining = set(range(len(comps)))
    while remaining:
        ready = [i for i in remaining if not (comp_imports[i] & remaining)]
        if not ready:
            ready = [max(remaining, key=lambda i: max(lines.get(c, 0)
                                                      for c in comps[i]))]
        ready.sort(key=lambda i: (-max(lines.get(c, 0) for c in comps[i]),
                                  min(comps[i])))
        for i in ready:
            members = sorted(comps[i], key=lambda c: (-lines.get(c, 0), c))
            order.extend(members)
            if len(members) > 1:
                cycles.append(members)
            remaining.discard(i)
    return order, cycles


def _no_new_exception(arch: dict, dest_crates: set[str],
                      deps: set[str]) -> tuple[bool, str]:
    """Would the move owe an `[[exception]]` ARCH_LAYERS does not already hold?

    For every forbid whose `from` matches a destination crate and whose `to`
    matches a crate the moved cluster would depend on: legal if the forbid's
    `except` names the dep, or an existing exception covers the pair; else a
    NEW row would be needed, which the campaign refuses (dm-queue kill).
    """
    for f in arch["forbids"]:
        frm, to = f.get("from"), f.get("to")
        if not frm or not to:
            continue
        for d in dest_crates:
            if not fnmatch.fnmatch(d, frm):
                continue
            for dep in deps:
                if not fnmatch.fnmatch(dep, to):
                    continue
                if any(fnmatch.fnmatch(dep, e) for e in (f.get("except") or [])):
                    continue
                if any(fnmatch.fnmatch(str(ex.get("from", "")), d)
                       and fnmatch.fnmatch(str(ex.get("to", "")), dep)
                       for ex in arch["exceptions"]):
                    continue
                return False, f"{d} -> {dep} violates forbid {frm} -> {to}"
    return True, ""


def plan(root: Path, crate: str | None = None) -> dict:
    """Build the move plan for `crate` (default: the queue head)."""
    root = Path(root)
    reg = registry_for(root)
    arch = _arch(root)
    ctx_crates = _context_crates(reg)
    clusters_raw = [dict(c) for c in reg.get("cluster", [])]
    if crate is None:
        crates_in_order = [r["crate"] for r in clusters_raw]
        if not crates_in_order:
            return {"crate": None, "clusters": [], "order": [],
                    "problems": ["no [[cluster]] rows in the registry"]}
        crate = crates_in_order[0]
    rows = [c for c in clusters_raw if c.get("crate") == crate]
    if not rows:
        return {"crate": crate, "clusters": [], "order": [],
                "problems": [f"no [[cluster]] rows for crate {crate!r}"]}

    tree_lines = _ctx_lines_from_tree(root, crate, reg)
    tree_edges = _tree_in_crate_edges(root, crate, reg)
    tree_ext = _tree_external_consumers(root, crate, arch)

    homes = {c["id"] for c in reg.get("context", [])
             if crate in c.get("crates", [])}
    own_lines = sum(tree_lines.get(h, [0, 0])[0] for h in homes)
    total_lines = sum(v[0] for v in tree_lines.values())

    named_dest_by_ctx = {
        c["context"]: set(_NAMED_CRATE.findall(c.get("dest", "") or ""))
        for c in rows}

    clusters: list[dict] = []
    for row in rows:
        ctx = row.get("context", "?")
        t_lines = tree_lines.get(ctx, [0, 0])
        reg_lines = _as_int(row.get("lines")) or 0
        reg_files = _as_int(row.get("files")) or 0
        imports = _cluster_imports(row, "imports_clusters")
        imported_by = _cluster_imports(row, "imported_by_clusters")
        ext = _cluster_external(row)
        for e in ext:
            if e["tier"] is None:
                e["tier"] = _crate_tier(arch, e["crate"])
        window = _window_pair(row, "window")
        after_port = _window_pair(row, "window_after_port")
        dest_tiers = _dest_tiers(row)
        dest_candidates = ctx_crates.get(ctx, [])
        registry_dest = row.get("dest", "")

        tree_imp = {d: len(v) for (s, d), v in tree_edges.items() if s == ctx}
        tree_imp_syms = {d: sorted(set(v))[:4]
                         for (s, d), v in tree_edges.items() if s == ctx}
        tree_imp_by = {s: len(v) for (s, d), v in tree_edges.items() if d == ctx}
        tree_own_deps = _tree_own_deps(root, crate, ctx, reg, arch)
        own_dep_max = _as_int(row.get("own_dep_max_tier"))
        consumer_min = _as_int(row.get("consumer_min_tier"))

        in_window = bool(window) and any(
            window[0] <= t <= window[1] for t in dest_tiers)
        in_after = bool(after_port) and any(
            after_port[0] <= t <= after_port[1] for t in dest_tiers)
        empty_window = bool(window) and window[0] > window[1]

        named = named_dest_by_ctx[ctx]
        dest_in_crates = bool(named & set(dest_candidates)) if named \
            else bool(dest_candidates)

        # The post-move dependencies: what this cluster's files name, plus the
        # destinations of every cluster it imports in-crate (whose symbols it
        # will name across the new crate line). The forbid ledger is checked
        # against the DESTINATION crate. The destination set is the row's `dest`
        # names FILTERED to the context's own homes — the prose around them
        # names comparison crates (understanding's dest says "the serving-policy
        # role"), and only a home can be where this cluster lands.
        deps = set(tree_own_deps)
        for imp_ctx in imports:
            deps |= (named_dest_by_ctx.get(imp_ctx)
                     or set(ctx_crates.get(imp_ctx, [])))
        dest_set = ((named & set(dest_candidates)) or named
                    or set(dest_candidates))
        exc_ok, exc_why = _no_new_exception(arch, dest_set, deps - dest_set)

        # The own-context share is monotone iff the move carries only non-own
        # lines out: a cluster whose `dest` does not name this crate is a
        # leaver, and a leaver that IS one of the crate's own contexts would
        # carry own lines out and drop the share. The plan's order already
        # filters the homes out, so this asserts the registry's proposal — a
        # `[[cluster]]` row that would move own lines fails loudly (ARCH 5).
        # The share itself stays telemetry, printed in the header.
        leaving = crate not in named
        share_ok = (not leaving) or (ctx not in homes)

        findings: list[str] = []
        if t_lines[0] != reg_lines or t_lines[1] != reg_files:
            findings.append(
                f"lines/files differ: tree {t_lines[0]}/{t_lines[1]} vs "
                f"registry {reg_lines}/{reg_files}")
        if tree_imp and tree_imp != imports:
            findings.append(
                f"in-crate imports differ: tree {tree_imp} vs registry "
                f"{imports}")
        if tree_own_deps and own_dep_max is not None:
            tmax = max(tree_own_deps.values())
            if tmax != own_dep_max:
                findings.append(
                    f"own-dep max tier: tree {tmax} vs registry {own_dep_max}")
        if named and not dest_in_crates:
            findings.append(
                f"registry dest {registry_dest!r} names no crate in the "
                f"context's crates {dest_candidates}")
        if not in_window and in_after:
            findings.append(
                f"dest tier {dest_tiers} in window_after_port {after_port}, "
                f"not window {window} — the port must land first")

        problems: list[str] = []
        if empty_window:
            syms = sorted({s for (src, _d), v in tree_edges.items()
                           if src == ctx for s in v}) or sorted(imports)
            fix = "port" if (tree_imp or imports) else "split"
            problems.append(
                f"KNOT {ctx}: empty window {window} — symbols "
                f"{', '.join(syms[:8]) or '(none)'} — fix: {fix}")
        if not dest_in_crates:
            problems.append(f"{ctx}: dest not in context crates")
        if not exc_ok:
            problems.append(f"{ctx}: {exc_why}")
        if not share_ok:
            problems.append(
                f"{ctx}: registry `dest` {registry_dest!r} would carry the "
                f"crate's own context out — share not monotone")

        clusters.append({
            "context": ctx, "lines": reg_lines, "files": reg_files,
            "tree_lines": t_lines[0], "tree_files": t_lines[1],
            "leaf": not imports, "hub": bool(imports),
            "imports": imports, "imported_by": imported_by,
            "tree_imports": tree_imp, "tree_imported_by": tree_imp_by,
            "tree_import_syms": tree_imp_syms,
            "external": ext,
            "own_dep_max_tier": own_dep_max, "consumer_min_tier": consumer_min,
            "tree_own_deps": tree_own_deps, "window": window,
            "window_after_port": after_port, "dest_tiers": dest_tiers,
            "dest_candidates": dest_candidates, "registry_dest": registry_dest,
            "dest_exists": any(_crate_exists(root, arch, c) for c in
                               dest_candidates),
            "shim_sites": _as_int(row.get("shim_sites")) or 0,
            "renames": _renames_riding(root, crate, ctx, reg),
            "constraint": row.get("constraint", ""),
            "asserts": {
                "dest_in_window": in_window or in_after,
                "dest_in_window_strict": in_window,
                "dest_in_crates": dest_in_crates,
                "no_new_exception": exc_ok,
                "share_monotone": share_ok,
            },
            "findings": findings, "problems": problems,
        })

    movable = [c for c in clusters if c["context"] not in homes]
    order, cycles = _move_order(movable)

    # The crate-level external-consumer cross-check. The registry's per-cluster
    # `external_consumers` are SYMBOL sets; re-deriving which symbol belongs to
    # which cluster needs the definition graph, so the tree audit is done once
    # for the crate (its union) and the per-cluster rows are read, not diffed.
    tree_consumers = {k: v["sites"] for k, v in tree_ext.items()}
    reg_consumers: dict[str, int] = {}
    for c in clusters:
        for e in c["external"]:
            reg_consumers[e["crate"]] = reg_consumers.get(e["crate"], 0) \
                + e["sites"]
    ext_finding = ""
    if tree_consumers != reg_consumers:
        ext_finding = (f"external consumers differ: tree {tree_consumers} vs "
                       f"registry {reg_consumers}")

    problems = [p for c in clusters for p in c["problems"]]
    return {"crate": crate, "homes": sorted(homes), "own_lines": own_lines,
            "total_lines": total_lines, "clusters": clusters, "order": order,
            "cycles": cycles, "tree_external": tree_ext,
            "external_finding": ext_finding, "problems": problems}


def _plan_detect(root: Path) -> list[str]:
    """The axis's own function: truthy iff the plan has a KNOT or a violation."""
    return plan(root)["problems"]


def _plan_fixture(root: Path, empty_window: bool) -> None:
    """A one-cluster crate whose window is empty (caught) or valid (refused).

    The destination `widget-home` is a context home AND a workspace member, so
    the valid case passes `dest_in_crates` and `no_new_exception`; the empty
    case plants `window = [3, 1]` — a floor above its ceiling — which is the
    one shape the order says prints KNOT rather than a destination.
    """
    (root / "quality").mkdir(parents=True, exist_ok=True)
    window = "[3, 1]" if empty_window else "[0, 3]"
    (root / "quality" / "DOMAINS.toml").write_text(
        '[[context]]\n'
        'id = "widget"\n'
        'kind = "supporting"\n'
        'owns = ["Sprocket"]\n'
        'crates = ["widget-home"]\n'
        'status = "kept"\n\n'
        '[[module]]\n'
        'path = "fixture-crate/src/lib.rs"\n'
        'context = "widget"\n'
        'lines = 10\n\n'
        '[[cluster]]\n'
        'crate = "fixture-crate"\n'
        'context = "widget"\n'
        'lines = 10\n'
        'files = 1\n'
        'leaf = true\n'
        f'window = {window}\n'
        'own_dep_max_tier = 0\n'
        'consumer_min_tier = 3\n'
        'dest_tier = 0\n'
        'dest = "widget-home"\n'
        'dest_exists = true\n'
        'shim_sites = 0\n', encoding="utf-8")
    (root / "quality" / "ARCH_LAYERS.toml").write_text(
        '[[layer]]\n'
        'name = "contract"\n'
        'crates = ["widget-home"]\n\n'
        '[[package]]\n'
        'name = "widget-home"\n'
        'crates = ["widget-home"]\n', encoding="utf-8")
    (root / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["fixture-crate", "widget-home"]\n',
        encoding="utf-8")
    for name in ("fixture-crate", "widget-home"):
        (root / name / "src").mkdir(parents=True, exist_ok=True)
        (root / name / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion = "0.0.0"\n',
            encoding="utf-8")
        (root / name / "src" / "lib.rs").write_text(
            "pub struct SprocketThing;\n", encoding="utf-8")


def _plan_positive(root: Path) -> None:
    """An empty tier window: caught as a KNOT."""
    _plan_fixture(root, empty_window=True)


def _plan_negative(root: Path) -> None:
    """A destination inside its window and its context: refused."""
    _plan_fixture(root, empty_window=False)


AXES.append({
    "id": "plan",
    "detect": _plan_detect,
    "positive": _plan_positive,
    "negative": _plan_negative,
})


@subcommand("plan")
def cmd_plan(args: list[str]) -> int:
    """Print the move plan for the queue head or `--crate X`."""
    crate = _flag_value(args, "--crate")
    if crate is None:
        q = queue(REPO)["queue"]
        if not q:
            print("plan: queue is empty — every crate is 100% its own context",
                  file=sys.stderr)
            return EXIT_ARTIFACT_ABSENT
        crate = q[0]["crate"]
    r = plan(REPO, crate)
    if not r["clusters"]:
        for p in r["problems"]:
            print(f"plan: {p}", file=sys.stderr)
        return EXIT_ARTIFACT_ABSENT
    total = r["total_lines"]
    share = (r["own_lines"] / total * 100) if total else 0.0
    print(f"plan — {r['crate']}  (own context: "
          f"{', '.join(r['homes']) or '(none)'} · own share "
          f"{r['own_lines']}/{total} {share:.1f}%)\n")
    print(f"  move order (leaves first, then size): {', '.join(r['order'])}\n")
    for cyc in r["cycles"]:
        print(f"  CYCLE (port or split before the order can be linear): "
              f"{' -> '.join(cyc)}\n")
    if r["external_finding"]:
        print(f"  {r['external_finding']}\n")
    for i, c in enumerate(r["clusters"], 1):
        kind = "hub" if c["hub"] else "leaf"
        stays = "  STAYS" if c["context"] in r["homes"] else ""
        print(f"  [{i}] {c['context']:<14} {kind:<4} "
              f"{c['tree_lines']}→{c['lines']} lines, "
              f"{c['tree_files']}→{c['files']} files{stays}")
        imp = ", ".join(f"{k}:{v}" for k, v in sorted(c["imports"].items())) \
            or "—"
        by = ", ".join(f"{k}:{v}" for k, v in sorted(c["imported_by"].items())) \
            or "—"
        print(f"      in-crate imports: {imp}   imported_by: {by}")
        if c["tree_import_syms"]:
            for d, syms in sorted(c["tree_import_syms"].items()):
                print(f"        -> {d}: {', '.join(syms)}")
        ext = ", ".join(f"{e['crate']}(t{e['tier']}):{e['sites']}"
                        for e in c["external"]) or "—"
        print(f"      external: {ext}")
        print(f"      own-dep-max {c['own_dep_max_tier']}  consumer-min "
              f"{c['consumer_min_tier']}  window {c['window']}"
              f"{'  after-port ' + str(c['window_after_port']) if c['window_after_port'] else ''}")
        print(f"      dest: {', '.join(c['dest_candidates']) or '—'} "
              f"(exists={c['dest_exists']})   registry dest: "
              f"{c['registry_dest']}")
        print(f"      shim sites {c['shim_sites']}   renames riding "
              f"{len(c['renames'])}")
        a = c["asserts"]
        print(f"      asserts: dest-in-window "
              f"{'ok' if a['dest_in_window'] else 'FAIL'}  "
              f"dest-in-crates {'ok' if a['dest_in_crates'] else 'FAIL'}  "
              f"no-new-exception {'ok' if a['no_new_exception'] else 'FAIL'}  "
              f"share-monotone {'ok' if a['share_monotone'] else 'FAIL'}")
        if c["constraint"]:
            print(f"      constraint: {c['constraint']}")
        for f in c["findings"]:
            print(f"      finding: {f}")
        for p in c["problems"]:
            print(f"      PROBLEM: {p}")
        print()
    print(f"  {len(r['problems'])} problems across {len(r['clusters'])} "
          f"clusters")
    return 1 if r["problems"] else EXIT_OK


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
