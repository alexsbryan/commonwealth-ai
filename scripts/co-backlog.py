#!/usr/bin/env python3
"""co-backlog.py — the seat's ranked, pull-based backlog, rendered from
the notes store.

PROTOCOL OVER EXISTING ARTIFACTS (order seat-backlog-protocol, note
47e6e132: periphery stays frozen). There is no backlog store. A backlog
item IS a notes-store todo carrying `related_entity=backlog` and a
structured header. This script only reads that store and ranks it; it
writes nothing back. co-closeout.py is the pattern.

    scripts/co-backlog.py --open          # render + open the heap
    scripts/co-backlog.py                 # render, print the path
    scripts/co-backlog.py --pull          # top chunk as an order draft
    scripts/co-backlog.py --self-test     # the lane (see "the lane" below)

Writes exactly one file: backlog.html, beside the seat's other rendered
surfaces (~/.sovereign/comaintainer/), for the same reason co-closeout
writes there — the seat's pages live together.


THE VALUE RULER LIVES IN quality/backlog-ruler.toml, NOT HERE. The axes
and their yardsticks, the 1-5 scale, the Blocks rule and the cost table
are that file, verbatim, and this script loads them at import
(`load_ruler()` below). They used to live in this docstring AND AGAIN as
Python constants forty lines down — two copies of one scorer, which is
exactly the smell ARCH §10.6 names. The renderer now prints the ruler it
actually loaded, naming the file and the version, and --self-test reddens
if the rendered page and the file disagree.

Change the ruler by editing the TOML. Nothing in this file has to move,
and the whole backlog re-scores for free: ordering is derived at read
(the priority-queue contract, order backlog-insert-system), so there is
no materialized heap to invalidate.

`svrn backlog add` sends the same TOML to the local model as its system
prompt, so the machine scorer and the renderer cannot drift apart either
— there is one ruler, and it is a file.


ITEM FORMAT — the body opens with a header block, terminated by the
first blank line. Recognized keys and nothing else:

    Objective: <standing objective / initiative / order id it serves>
    Value: <1-5> — <one falsifiable line, naming the axis A-F>
    Cost: <S|M|L> (session-chunks)
    Approach: <1-3 sentences: what gets built or changed, which EXISTING
               surface it builds on, and why that makes the Cost credible.
               Or "unknown — needs a design pass">
    Chunks-with: <note ids, or none>
    Blocks: <order/step, optional>
    Done-when: <falsifiable completion condition, optional>
    Evidence: <the citation that makes the above checkable, optional>

THE SIZING RULE (operator directive 341884f5). Cost must FOLLOW from
Approach. The operator's reason, verbatim: a raw note "struggles to get
to the point of how we'd actually solve it", and "I don't think I can
feel that the sizing is credible if I don't have a sense of the
potential solution." So an S/M/L with no stated approach is a number
with nothing behind it, and this renderer treats it as one. `Approach:
unknown — needs a design pass` is a FIRST-CLASS answer and the honest
one when nobody has thought it through — it forces the item unvetted
however complete the rest of the header is. Naming the existing surface
is what makes the size arguable rather than asserted (principle 11: the
inventory outranks the plan).

VETTED is one structural rule, in one place (`vet()`), and it is
deliberately strict: an item is vetted iff its header parses clean AND
it carries a non-empty `Done-when:` AND a non-empty `Evidence:` AND an
`Approach:` that is not "unknown". Prose
is never sniffed for an implied done-when — a heuristic that guesses
"this reads falsifiable enough" would let the seat pull work nobody
scoped. Unvetted items render greyed, are never pullable, and each one
NAMES what it is missing (ARCH §18.3: absence is reported, never
defaulted). Vetting is an act someone performs, not a shape the parser
infers.


ACCESS PATH — read-only sqlite over an EXPLICITLY NAMED store path.

This is co-closeout.py's access path (`directive_log_path()`,
co-closeout.py:164-171): an env override first, else a deterministic
absolute path under the data dir, opened directly off disk — no daemon,
no CLI. It is chosen over shelling out to `sovereign notes list` on
measured evidence, not on the documented caveat alone. Measured
2026-08-09 on this host, same query, only cwd differs:

    cwd=/Users/alexsbryan/dev/commonwealth-ai  ->  ./sovereign/.sovereign/notes.db   (68 notes)
    cwd=$HOME                                  ->  ~/.sovereign/notes.db           (6811 notes)

`sovereign notes list --id 0807272f` returns a hit from BOTH — a
different note each time, exit 0 either way. That is the failure this
script must not inherit: a cwd-sensitive resolver that answers
confidently from the wrong store. The comaintainer skill records the
caveat ("from a repo cwd, `sovereign notes list` can resolve a stray
nested notes.db"; SKILL.md, Stewardship section) and prescribes the MCP
tool — which a script cannot call. Naming the path is the script-side
form of the same fix: the store is never discovered from cwd.

The page's footer prints the resolved path and the row count, so a
render against the wrong store is visible rather than plausible.

Presentation note: the stylesheet below is the same visual language as
co-closeout.py, deliberately duplicated rather than imported. Each seat
script stays a standalone single file (co-closeout's own contract), and
a stylesheet is presentation, not a threshold, scorer, schema or key —
ARCH §10.6's "one decider" governs those, not chrome. The DECIDERS in
this file (ranking, vetting, parsing) each exist exactly once.
"""

from __future__ import annotations

import argparse
import datetime as dt
import html
import os
import re
import sqlite3
import sys
import tempfile
import tomllib
from pathlib import Path

# --- the ruler, as data ---------------------------------------------------
#
# The closed sets below are still enums and not string matches (ARCH
# §2/§9) — they are just no longer WRITTEN here. They are loaded from
# quality/backlog-ruler.toml, the one copy, which the parser, the ranker,
# the renderer and `svrn backlog add`'s model prompt all read.
#
# Absence is reported, never defaulted (ARCH §18.3): if the file is
# missing or malformed this script exits, it does not fall back to a
# built-in ruler. A silent built-in fallback is precisely how the two
# copies would grow back.


def ruler_path() -> Path:
    """The ruler, NEVER discovered from cwd — same rule as the store.

    CO_BACKLOG_RULER is the test override, and exists so --self-test can
    render against an EDITED ruler without touching the repo's copy.
    Otherwise the path is derived from this file's own location, so the
    renderer reads the ruler that ships beside it whatever the cwd."""
    env = os.environ.get("CO_BACKLOG_RULER")
    if env:
        return Path(env).expanduser()
    return Path(__file__).resolve().parent.parent / "quality" / "backlog-ruler.toml"


class Ruler:
    """The loaded ruler. Every field the rest of this script used to
    hardcode, plus the prose the page renders so a reader can see which
    ruler scored the heap in front of them."""

    def __init__(self, path: Path, data: dict):
        self.path = path
        self.version = str(data["version"])
        self.minted = str(data.get("minted", ""))
        self.axes = [(a["letter"], a["name"], a["yardstick"].strip())
                     for a in data["axis"]]
        self.axis_names = {letter: name for letter, name, _ in self.axes}
        self.axis_set = set(self.axis_names)
        self.cost_chunks = {k: int(v) for k, v in data["cost"]["chunks"].items()}
        self.value_min = int(data["value"]["min"])
        self.value_max = int(data["value"]["max"])
        self.scale = [str(s) for s in data["scoring"]["scale"]]
        self.blocks_rule = data["scoring"]["blocks_rule"].strip()
        self.roi_rule = data["scoring"]["roi"].strip()
        self.axis_f_rule = data["scoring"].get("axis_f", "").strip()
        self.provenance = {k: str(v).strip()
                           for k, v in data.get("provenance", {}).items()}
        self.header_keys = [str(k) for k in data["format"]["header_keys"]]

    @property
    def value_range(self):
        return range(self.value_min, self.value_max + 1)

    @property
    def letters(self) -> str:
        """"A-F" — the axis alphabet, for the messages that name it."""
        letters = [a[0] for a in self.axes]
        return f"{letters[0]}-{letters[-1]}" if len(letters) > 1 else letters[0]


def load_ruler(path: Path = None) -> Ruler:
    path = path or ruler_path()
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise SystemExit(
            f"co-backlog: no value ruler at {path}. The ruler is data, not "
            "code — this script has no built-in copy to fall back to. "
            "(Set CO_BACKLOG_RULER if it lives elsewhere.)")
    except (tomllib.TOMLDecodeError, OSError) as exc:
        raise SystemExit(f"co-backlog: cannot read the value ruler at {path}: {exc}")
    try:
        return Ruler(path, data)
    except (KeyError, TypeError, ValueError) as exc:
        raise SystemExit(
            f"co-backlog: the value ruler at {path} is missing or malformed "
            f"at {exc!r}. Fix the file; there is no default ruler.")


RULER = load_ruler()

# Names kept for the readers that were already using them. They are views
# on RULER now, not a second copy — rebind them all through reload_ruler()
# so a test that swaps the ruler cannot leave half the script on the old
# one (that half-swap IS the divergence this deliverable is about).
COST_CHUNKS = RULER.cost_chunks
AXES = RULER.axis_set
AXIS_NAMES = RULER.axis_names
VALUE_RANGE = RULER.value_range


def reload_ruler(path: Path = None) -> Ruler:
    """Re-read the ruler and rebind every view of it. Used by --self-test
    to render under an EDITED ruler; there is no other caller."""
    global RULER, COST_CHUNKS, AXES, AXIS_NAMES, VALUE_RANGE
    global HEADER_KEYS, _KEY_LOOKUP
    RULER = load_ruler(path)
    COST_CHUNKS = RULER.cost_chunks
    AXES = RULER.axis_set
    AXIS_NAMES = RULER.axis_names
    VALUE_RANGE = RULER.value_range
    HEADER_KEYS = tuple(RULER.header_keys)
    _KEY_LOOKUP = {k.lower(): k for k in HEADER_KEYS}
    return RULER

# The item format's key list is ALSO in quality/backlog-ruler.toml, and
# is read from there rather than written here — because the writer is now
# in another language. `svrn backlog add` (Rust) emits this header and
# this parser (Python) reads it; a key list written twice would drift the
# moment either side gained a field. One decider, one name, across the
# language boundary (ARCH §10.6).
HEADER_KEYS = tuple(RULER.header_keys)

# "Approach: unknown — needs a design pass" is a FIRST-CLASS answer, not a
# missing field, and it forces the item unvetted however complete the rest
# of the header is (operator directive 341884f5). An unsized item that
# looks pullable is worse than one that admits it needs a design pass.
APPROACH_UNKNOWN = re.compile(r"^\s*unknown\b", re.IGNORECASE)
_KEY_LOOKUP = {k.lower(): k for k in HEADER_KEYS}

HEADER_LINE = re.compile(r"^([A-Za-z][A-Za-z-]*):[ \t]*(.*)$")
# "4 — moves A: ..." / "4 - A: ..." / "4 — A/C: ...". The separator is
# em-dash or hyphen; the axis letters are the load-bearing part.
#
# These three deliberately match WIDER than the ruler and let the ruler's
# own closed sets reject what it does not recognize (one decider — ARCH
# §10.6). A regex that also encoded `[1-5]`, `[A-F]` and `[SML]` would be
# a second copy of the ruler, and an item scored 7 would come back
# "unparseable" rather than "outside the scale".
VALUE_LINE = re.compile(r"^([0-9])\s*(?:[—-]\s*)?(.*)$", re.S)
AXIS_TOKEN = re.compile(r"\b([A-Z])\b")
COST_LINE = re.compile(r"^([A-Za-z])\b")
ID_TOKEN = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f-]+)?", re.IGNORECASE)

# --- defect injection -----------------------------------------------------
#
# ARCH §18.1: "a check with no failing input you can name is not a
# check." Rather than name the failing inputs in a comment and trust a
# future reader to have watched them fail once, this script CARRIES
# them. Three deciders consult DEFECT, and --self-test re-runs its whole
# battery under each defect and requires the battery to go red. A gate
# nobody has watched fail is not a gate — so this one watches itself
# fail on every single run, and cannot rot into a rubber stamp.

DEFECT = None
DEFECTS = ("bad-roi-order", "unvetted-pullable", "malformed-swallowed",
           "machine-score-vets")


class Malformed:
    """A header line that would not parse, or an item whose required
    fields are absent. Reported on the page with its note id and the
    offending text — never silently dropped (ARCH §18.3)."""

    def __init__(self, note_id: str, lineno, err: str, raw: str = ""):
        self.note_id, self.lineno, self.err = note_id, lineno, err
        self.raw = (raw or "")[:160]


# --- the store ------------------------------------------------------------


def notes_db_path() -> Path:
    """The store, NEVER discovered from cwd (see ACCESS PATH above).

    CO_BACKLOG_NOTES_DB is the test override and exists for exactly the
    reason co-closeout honors CO_DIRECTIVE_LOG: --self-test must not be
    able to touch the operator's real store. SOVEREIGN_DATA_DIR is the
    registered per-user data root (quality/env-flags.toml:603) and is
    honored so a rebranded or staged install resolves correctly."""
    env = os.environ.get("CO_BACKLOG_NOTES_DB")
    if env:
        return Path(env).expanduser()
    data_dir = os.environ.get("SOVEREIGN_DATA_DIR")
    if data_dir:
        return Path(data_dir).expanduser() / "notes.db"
    return Path.home() / ".sovereign" / "notes.db"


class StoreRead:
    """What one read of the store returned, including what it did NOT
    return. `other_todos` is the count of live kind=todo notes that are
    not backlog items; the page names it so a mostly-unmigrated store
    cannot render as a short backlog."""

    def __init__(self, path, rows, other_todos, error=None):
        self.path, self.rows = path, rows
        self.other_todos, self.error = other_todos, error


def read_store(path: Path) -> StoreRead:
    if not path.exists():
        return StoreRead(path, [], 0, f"no notes store at {path}")
    try:
        conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    except sqlite3.Error as exc:
        return StoreRead(path, [], 0, f"cannot open {path} read-only: {exc}")
    try:
        live = "retired_at IS NULL AND tombstone = 0"
        rows = conn.execute(
            "SELECT id, content, created_at, scope FROM notes "
            f"WHERE kind = 'todo' AND related_entity = 'backlog' AND {live} "
            "ORDER BY created_at ASC"
        ).fetchall()
        other = conn.execute(
            "SELECT COUNT(*) FROM notes WHERE kind = 'todo' "
            f"AND (related_entity IS NULL OR related_entity <> 'backlog') AND {live}"
        ).fetchone()[0]
    except sqlite3.Error as exc:
        return StoreRead(path, [], 0, f"query failed against {path}: {exc}")
    finally:
        conn.close()
    return StoreRead(path, rows, other)


# --- the parser (decider 1) -----------------------------------------------


class Item:
    def __init__(self, note_id: str, created_at, body: str):
        self.id = note_id
        self.short = note_id[:8]
        self.created_at = created_at
        self.body = body
        self.fields = {}
        self.problems = []          # why this item is malformed
        self.missing = []           # why this item is unvetted
        self.value = None           # as declared
        self.effective_value = None # after the Blocks rule
        self.inherited_from = None
        self.cost = None
        self.chunks_with = []
        self.blocks = None
        self.blocks_unresolved = False
        self.axes = []

    # The rest of the body, below the header block. Rendered as the
    # item's own words; never summarized (co-closeout's rule).
    @property
    def prose(self) -> str:
        parts = self.body.split("\n\n", 1)
        return parts[1].strip() if len(parts) > 1 else ""

    @property
    def objective(self) -> str:
        return self.fields.get("Objective", "")

    @property
    def cost_chunks(self):
        return COST_CHUNKS.get(self.cost) if self.cost else None

    @property
    def roi(self):
        if self.effective_value is None or not self.cost_chunks:
            return None
        return self.effective_value / self.cost_chunks

    @property
    def parsed(self) -> bool:
        return not self.problems


def parse_item(note_id: str, created_at, body: str, malformed: list) -> Item:
    """The header block is the leading run of lines up to the first blank
    line. Every non-empty line in it must be `Key: value` with a
    recognized key; anything else is malformed and SAID SO."""
    item = Item(note_id, created_at, body)
    lines = body.split("\n")
    header = []
    for lineno, line in enumerate(lines, 1):
        if not line.strip():
            break
        header.append((lineno, line))

    for lineno, line in header:
        m = HEADER_LINE.match(line.strip())
        if not m:
            item.problems.append(f"line {lineno} is not `Key: value`")
            if DEFECT != "malformed-swallowed":
                malformed.append(Malformed(note_id, lineno,
                                           "not a `Key: value` header line", line))
            continue
        key_raw, val = m.group(1), m.group(2).strip()
        key = _KEY_LOOKUP.get(key_raw.lower())
        if key is None:
            item.problems.append(f"line {lineno}: unrecognized key {key_raw!r}")
            if DEFECT != "malformed-swallowed":
                malformed.append(Malformed(note_id, lineno,
                                           f"unrecognized header key {key_raw!r}", line))
            continue
        item.fields[key] = val

    # Value: 1-5, and it must name at least one axis. A value with no
    # axis is a number with no argument behind it.
    raw_value = item.fields.get("Value", "")
    m = VALUE_LINE.match(raw_value.strip()) if raw_value else None
    if not m:
        item.problems.append(
            f"no parseable `Value:` (want `<{RULER.value_min}-{RULER.value_max}> — "
            f"<line naming axis {RULER.letters}>`)")
        if DEFECT != "malformed-swallowed" and raw_value:
            malformed.append(Malformed(note_id, None, "unparseable Value", raw_value))
    else:
        v = int(m.group(1))
        if v not in VALUE_RANGE:
            item.problems.append(
                f"Value {v} outside {RULER.value_min}-{RULER.value_max}")
        else:
            item.value = v
        item.axes = [a for a in dict.fromkeys(AXIS_TOKEN.findall(m.group(2)))
                     if a in AXES]
        if not item.axes:
            item.problems.append(f"`Value:` names no axis {RULER.letters}")

    raw_cost = item.fields.get("Cost", "")
    cm = COST_LINE.match(raw_cost.strip()) if raw_cost else None
    sizes = ", ".join(sorted(COST_CHUNKS, key=lambda k: COST_CHUNKS[k]))
    if not cm or cm.group(1).upper() not in COST_CHUNKS:
        item.problems.append(f"no parseable `Cost:` (want one of {sizes})")
        if DEFECT != "malformed-swallowed" and raw_cost:
            malformed.append(Malformed(note_id, None, "unparseable Cost", raw_cost))
    else:
        item.cost = cm.group(1).upper()

    if "Objective" not in item.fields or not item.fields["Objective"]:
        item.problems.append("no `Objective:` — the item serves nothing nameable")

    raw_chunks = item.fields.get("Chunks-with", "").strip()
    if raw_chunks and raw_chunks.lower() not in ("none", "-", "(none)"):
        item.chunks_with = [t.lower() for t in ID_TOKEN.findall(raw_chunks)]
        if not item.chunks_with:
            item.problems.append(f"`Chunks-with:` names no note id: {raw_chunks!r}")

    blocks = item.fields.get("Blocks", "").strip()
    if blocks and blocks.lower() not in ("none", "-", "(none)"):
        item.blocks = blocks

    item.effective_value = item.value
    return item


# --- vetting (decider 2) --------------------------------------------------


def vet(item: Item) -> bool:
    """The ONE vetted rule. Populates item.missing with the named reason
    for every failure, so the page can say WHY an item is greyed."""
    item.missing = []
    if item.problems:
        item.missing.extend(item.problems)
    # A MACHINE SCORE IS NEVER A VETTING. `svrn backlog add` scores items
    # with the local model and stamps `Scored-by: <model id>`; vetting is
    # a human review act, so a machine-scored item is unpullable no
    # matter how complete its header looks — and it stays unpullable
    # until a person removes the stamp, which is the review (order
    # backlog-insert-system D2). Structural, not remembered (ARCH §10):
    # the producer cannot opt out by writing a good Done-when, because
    # the gate is here and not in the producer.
    scorer = item.fields.get("Scored-by", "").strip()
    if scorer and DEFECT != "machine-score-vets":
        item.missing.append(
            f"scored by {scorer}, not by a person — a machine score is a "
            "draft; clear `Scored-by:` when you have reviewed it")
    if not item.fields.get("Done-when", "").strip():
        item.missing.append("no `Done-when:` — nothing here is falsifiable yet")
    if not item.fields.get("Evidence", "").strip():
        item.missing.append("no `Evidence:` — the done-when cites nothing checkable")
    approach = item.fields.get("Approach", "").strip()
    if not approach:
        item.missing.append(
            "no `Approach:` — with no stated solution the Cost is a guess, "
            "and an uncredible size is not pullable")
    elif APPROACH_UNKNOWN.match(approach):
        item.missing.append(
            "`Approach: unknown` — needs a design pass before it can be sized")
    return not item.missing


def pullable(item: Item) -> bool:
    if DEFECT == "unvetted-pullable":
        return True
    return vet(item)


# --- the Blocks rule ------------------------------------------------------


def apply_blocks_rule(items):
    """"An item carrying `Blocks: <order/step>` inherits the value of what
    it blocks." Resolvable only when the target names another item in
    THIS set (by note id). An unresolvable target keeps the item's own
    value and is flagged — the page says "blocks something outside the
    backlog" rather than quietly inflating a score."""
    by_short = {i.short: i for i in items}

    def resolve(item, seen):
        if item.effective_value is not None and item.inherited_from:
            return item.effective_value
        if item.short in seen:          # cycle: no inheritance, keep own
            return item.value
        if not item.blocks:
            return item.value
        target = None
        for tok in ID_TOKEN.findall(item.blocks):
            cand = by_short.get(tok.lower()[:8])
            if cand is not None and cand is not item:
                target = cand
                break
        if target is None:
            item.blocks_unresolved = True
            return item.value
        tv = resolve(target, seen | {item.short})
        if tv is not None and (item.value is None or tv > item.value):
            item.inherited_from = target.short
            return tv
        return item.value

    for it in items:
        if it.value is not None or it.blocks:
            it.effective_value = resolve(it, frozenset())


# --- chunk groups + ranking (decider 3) -----------------------------------


class Group:
    def __init__(self, items):
        # Within a group: highest ROI first, then the ruler's own
        # tie-breaks (value desc, older first, id) so a render is
        # byte-identical across runs on an unchanged store.
        self.items = sorted(items, key=item_sort_key)

    @property
    def is_chunk(self) -> bool:
        return len(self.items) > 1

    @property
    def value(self):
        vals = [i.effective_value for i in self.items if i.effective_value is not None]
        return sum(vals) if vals else None

    @property
    def cost_chunks(self):
        costs = [i.cost_chunks for i in self.items if i.cost_chunks]
        return sum(costs) if costs else None

    @property
    def roi(self):
        if self.value is None or not self.cost_chunks:
            return None
        return self.value / self.cost_chunks


def item_sort_key(item: Item):
    # None-valued (malformed) items sort last, never first — an item we
    # could not score must not be able to head the heap.
    roi = item.roi
    val = item.effective_value
    return (
        0 if roi is not None else 1,
        -(roi or 0) if DEFECT != "bad-roi-order" else 0,
        -(val or 0),
        item.created_at or 0,
        item.id,
    )


def group_sort_key(group: Group):
    roi = group.roi
    return (
        0 if roi is not None else 1,
        -(roi or 0) if DEFECT != "bad-roi-order" else 0,
        -(group.value or 0),
        min((i.created_at or 0) for i in group.items),
        group.items[0].id,
    )


def build_groups(items):
    """Connected components over Chunks-with, treated as SYMMETRIC: if a
    declares it chunks with b, they chunk, whether or not b says so.
    A one-sided declaration is the common case when the seat banks the
    second item later."""
    by_short = {i.short: i for i in items}
    parent = {i.short: i.short for i in items}

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[rb] = ra

    for it in items:
        for tok in it.chunks_with:
            mate = by_short.get(tok[:8])
            if mate is not None:
                union(it.short, mate.short)

    buckets = {}
    for it in items:
        buckets.setdefault(find(it.short), []).append(it)
    groups = [Group(v) for v in buckets.values()]
    groups.sort(key=group_sort_key)
    return groups


def rank(items):
    """ONE ranker, used by --open and --pull alike. Returns
    (groups, top_item, top_group) where top_item is the highest-ROI
    PULLABLE item, or None when nothing in the backlog is pullable."""
    apply_blocks_rule(items)
    groups = build_groups(items)
    top_item = top_group = None
    for g in groups:
        for it in g.items:
            if it.roi is not None and pullable(it):
                top_item, top_group = it, g
                break
        if top_item is not None:
            break
    return groups, top_item, top_group


def load_backlog(path: Path):
    read = read_store(path)
    malformed = []
    items = [parse_item(r[0], r[2], r[1] or "", malformed) for r in read.rows]
    groups, top_item, top_group = rank(items)
    return read, items, groups, top_item, top_group, malformed


# --- rendering ------------------------------------------------------------

E = html.escape

CSS = """
:root{
  --ground:#FAF9F5; --panel:#FFFFFF; --ink:#20241F; --meta:#6E7369; --rule:#E5E3D9;
  --ok:#3E6B50; --ok-soft:#EAF1EC; --pend:#A8721F; --pend-soft:#F7EFDF;
  --code-bg:#F1F0E9; --shadow:0 1px 3px rgba(32,36,31,.06); --grey:#9AA096;
}
@media (prefers-color-scheme: dark){:root{
  --ground:#191B18; --panel:#20231F; --ink:#E7E6DF; --meta:#9AA096; --rule:#31352E;
  --ok:#7FB393; --ok-soft:#24322A; --pend:#D9A24C; --pend-soft:#332B1D;
  --code-bg:#262922; --shadow:none; --grey:#6E7369;
}}
:root[data-theme="dark"]{
  --ground:#191B18; --panel:#20231F; --ink:#E7E6DF; --meta:#9AA096; --rule:#31352E;
  --ok:#7FB393; --ok-soft:#24322A; --pend:#D9A24C; --pend-soft:#332B1D;
  --code-bg:#262922; --shadow:none; --grey:#6E7369;
}
:root[data-theme="light"]{
  --ground:#FAF9F5; --panel:#FFFFFF; --ink:#20241F; --meta:#6E7369; --rule:#E5E3D9;
  --ok:#3E6B50; --ok-soft:#EAF1EC; --pend:#A8721F; --pend-soft:#F7EFDF;
  --code-bg:#F1F0E9; --shadow:0 1px 3px rgba(32,36,31,.06); --grey:#9AA096;
}
*{box-sizing:border-box}
body{margin:0;background:var(--ground);color:var(--ink);
  font:15px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;
  padding:40px 20px 80px}
main{max-width:900px;margin:0 auto;display:flex;flex-direction:column;gap:36px}
h1{font-size:26px;margin:0;letter-spacing:-.01em;text-wrap:balance}
h2{font-size:13px;margin:0;text-transform:uppercase;letter-spacing:.09em;color:var(--meta);font-weight:600}
p{margin:0}
.sub{color:var(--meta);margin-top:6px}
.chips{display:flex;gap:8px;flex-wrap:wrap;margin-top:14px}
.chip{font-size:12.5px;padding:3px 10px;border:1px solid var(--rule);border-radius:99px;color:var(--meta)}
.chip b{color:var(--ink);font-weight:600}
section{display:flex;flex-direction:column;gap:14px}
.card{background:var(--panel);border:1px solid var(--rule);border-radius:10px;
  box-shadow:var(--shadow);overflow:hidden}
.card>header{display:flex;align-items:center;gap:10px;padding:13px 18px;
  border-bottom:1px solid var(--rule);flex-wrap:wrap}
.ref{font:600 13px/1 ui-monospace,SFMono-Regular,Menlo,monospace;background:var(--code-bg);
  padding:5px 9px;border-radius:6px}
.roi{font:600 13px/1 ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--ink);
  font-variant-numeric:tabular-nums}
.axis{font-size:11.5px;text-transform:uppercase;letter-spacing:.07em;color:var(--meta)}
.pill{margin-left:auto;font-size:12px;font-weight:600;padding:4px 11px;border-radius:99px;
  background:var(--ok-soft);color:var(--ok)}
.pill.grey{background:transparent;color:var(--grey);border:1px solid var(--rule)}
.card .body{padding:14px 18px;display:flex;flex-direction:column;gap:10px}
.lbl{font-size:11.5px;text-transform:uppercase;letter-spacing:.07em;color:var(--meta);margin-bottom:3px}
.top{border:2px solid var(--ok)}
.top>header{background:var(--ok-soft)}
.unvetted{opacity:.62}
.unvetted .ref{color:var(--grey)}
.why{border-left:3px solid var(--rule);padding:8px 13px;font-size:13.5px;color:var(--meta)}
.approach{border-left:3px solid var(--ok);background:var(--ok-soft);padding:9px 13px;
  border-radius:0 8px 8px 0;font-size:14px}
.approach.none{border-left-color:var(--pend);background:var(--pend-soft);color:var(--pend)}
.chunkgroup{border:1px dashed var(--rule);border-radius:12px;padding:14px;
  display:flex;flex-direction:column;gap:12px;background:rgba(127,127,127,.035)}
.chunkhdr{display:flex;gap:10px;align-items:baseline;flex-wrap:wrap;font-size:12.5px;color:var(--meta)}
.chunkhdr b{color:var(--ink)}
ul{margin:0;padding-left:18px;display:flex;flex-direction:column;gap:5px}
code{font:12.5px ui-monospace,SFMono-Regular,Menlo,monospace;background:var(--code-bg);
  padding:1.5px 5px;border-radius:4px}
pre{background:var(--code-bg);border:1px solid var(--rule);border-radius:8px;padding:12px 14px;
  overflow-x:auto;font:12.5px/1.7 ui-monospace,SFMono-Regular,Menlo,monospace;margin:0;
  white-space:pre-wrap}
details summary{cursor:pointer;color:var(--meta)}
details[open] summary{margin-bottom:6px}
.empty{background:var(--panel);border:1px dashed var(--rule);border-radius:10px;
  padding:16px 18px;color:var(--meta)}
.foot{color:var(--meta);font-size:12.5px;border-top:1px solid var(--rule);padding-top:14px;line-height:1.7}
.bad{color:var(--pend)}
"""


def empty(msg: str) -> str:
    return f'<div class="empty">{E(msg)}</div>'


def fmt_roi(roi) -> str:
    return "unscored" if roi is None else f"{roi:.2f}"


def axis_label(item: Item) -> str:
    if not item.axes:
        return "no axis"
    return " / ".join(f"{a} {AXIS_NAMES[a]}" for a in item.axes)


def render_item(item: Item, is_top: bool) -> str:
    ok = pullable(item)
    classes = ["card"]
    if is_top:
        classes.append("top")
    if not ok:
        classes.append("unvetted")

    if is_top:
        pill = '<span class="pill">next pull</span>'
    elif ok:
        pill = '<span class="pill">pullable</span>'
    else:
        pill = '<span class="pill grey">unvetted — not pullable</span>'

    val = item.effective_value
    val_txt = "?" if val is None else str(val)
    if item.inherited_from:
        val_txt += f" (inherited from {item.inherited_from})"
    cost_txt = item.cost or "?"

    approach = item.fields.get("Approach", "").strip()
    if not approach:
        appr_html = ('<div class="approach none">No Approach line. The Cost below '
                     'is a guess until someone states how this gets solved.</div>')
    elif APPROACH_UNKNOWN.match(approach):
        appr_html = (f'<div class="approach none">{E(approach)}</div>')
    else:
        appr_html = f'<div class="approach">{E(approach)}</div>'

    body = [
        f'<div><div class="lbl">Objective</div>'
        f'{E(item.objective) if item.objective else "<span class=bad>(absent)</span>"}</div>',
        # The approach is the point; the note below is only the evidence
        # for it (operator directive 341884f5), so it renders high and
        # never inside the collapsed verbatim block.
        f'<div><div class="lbl">Approach — how this gets solved</div>{appr_html}</div>',
        f'<div><div class="lbl">Value / Cost</div>'
        f'{E(val_txt)} &middot; {E(cost_txt)} '
        f'({E(str(item.cost_chunks or "?"))} session-chunk(s)) &middot; '
        f'ROI {E(fmt_roi(item.roi))}</div>',
    ]
    raw_value = item.fields.get("Value", "")
    if raw_value:
        body.append(f'<div><div class="lbl">The claim</div>{E(raw_value)}</div>')
    if item.fields.get("Done-when"):
        body.append(f'<div><div class="lbl">Done when</div>{E(item.fields["Done-when"])}</div>')
    if item.fields.get("Evidence"):
        body.append(f'<div><div class="lbl">Evidence</div>{E(item.fields["Evidence"])}</div>')
    if item.blocks:
        flag = " — blocks something outside this backlog; no value inherited" \
            if item.blocks_unresolved else ""
        body.append(f'<div><div class="lbl">Blocks</div>{E(item.blocks)}{E(flag)}</div>')
    if not ok:
        body.append('<div class="why">Unvetted, so it cannot be pulled: '
                    + E("; ".join(item.missing)) + "</div>")
    if item.prose:
        body.append(f"<details><summary>the note, verbatim</summary>"
                    f"<pre>{E(item.prose)}</pre></details>")

    return (
        f'<div class="{" ".join(classes)}"><header>'
        f'<span class="ref">{E(item.short)}</span>'
        f'<span class="roi">ROI {E(fmt_roi(item.roi))}</span>'
        f'<span class="axis">{E(axis_label(item))}</span>{pill}</header>'
        f'<div class="body">{"".join(body)}</div></div>'
    )


def render_heap(groups, top_item) -> str:
    if not groups:
        return ("<section><h2>The heap — every item, highest ROI first</h2>"
                + empty("No backlog items. The heap is empty because the store "
                        "says so, not because nothing was read.")
                + "</section>")
    blocks = []
    for g in groups:
        cards = "".join(render_item(i, i is top_item) for i in g.items)
        if g.is_chunk:
            ids = ", ".join(i.short for i in g.items)
            blocks.append(
                '<div class="chunkgroup"><div class="chunkhdr">'
                f'<b>chunk of {len(g.items)}</b>'
                f'<span>group ROI {E(fmt_roi(g.roi))} '
                f'(value {E(str(g.value or "?"))} / '
                f'{E(str(g.cost_chunks or "?"))} session-chunks)</span>'
                f'<span>{E(ids)}</span></div>{cards}</div>'
            )
        else:
            blocks.append(cards)
    return ("<section><h2>The heap — every item, highest ROI first</h2>"
            + "".join(blocks) + "</section>")


def render_pull_banner(top_item, top_group, items) -> str:
    if top_item is None:
        vetted_n = sum(1 for i in items if pullable(i))
        why = (f"{len(items)} item(s) present, {vetted_n} vetted. "
               "Nothing is pullable: an item needs a clean header plus a "
               "Done-when and an Evidence line before the seat can pull it.")
        return ("<section><h2>Next pull</h2>" + empty(why) + "</section>")
    mates = [i for i in top_group.items if i is not top_item]
    mate_txt = ("" if not mates else
                " It chunks with " + ", ".join(i.short for i in mates) + ".")
    return (
        "<section><h2>Next pull</h2>"
        f'<div class="card top"><header><span class="ref">{E(top_item.short)}</span>'
        f'<span class="roi">ROI {E(fmt_roi(top_item.roi))}</span>'
        f'<span class="axis">{E(axis_label(top_item))}</span>'
        '<span class="pill">say pull</span></header>'
        f'<div class="body"><div>{E(top_item.objective)}</div>'
        f'<div class="lbl">The claim</div><div>{E(top_item.fields.get("Value", ""))}</div>'
        f'<div class="why">{E("Run scripts/co-backlog.py --pull for the order draft." + mate_txt)}</div>'
        "</div></div></section>"
    )


# The ruler section's heading, written once because the renderer emits it
# and the divergence check looks for it (one decider — a second copy is
# how the check would quietly stop matching the page).
RULER_HEADING = " — v{version}, and it is a file"


def flat(text: str) -> str:
    """One line, single-spaced. The ruler's strings are wrapped in the
    TOML for human editing; the page and the divergence check must agree
    on how that wrapping is flattened, so both call this."""
    return " ".join(str(text).split())


def render_ruler(ruler: Ruler) -> str:
    """The ruler, on the page that used it.

    A reader looking at an ordering should be able to see the yardstick
    that produced it without opening a file, and a ruler edit should be
    visible HERE on the next render — that is what --self-test's
    divergence check watches. Rendered from the loaded Ruler object, never
    from a second copy of the text (ARCH §10.6)."""
    rows = [
        f'<div><div class="lbl">{E(letter)} — {E(name)}</div>{E(flat(yard))}</div>'
        for letter, name, yard in ruler.axes
    ]
    rows.append('<div><div class="lbl">The scale</div>'
                + "<br>".join(E(flat(s)) for s in ruler.scale) + "</div>")
    rows.append(f'<div><div class="lbl">Blocks rule</div>{E(flat(ruler.blocks_rule))}</div>')
    rows.append(f'<div><div class="lbl">ROI</div>{E(flat(ruler.roi_rule))}</div>')
    if ruler.axis_f_rule:
        rows.append('<div><div class="lbl">Axis F, as an axis not a modifier</div>'
                    + E(flat(ruler.axis_f_rule)) + "</div>")
    return (
        "<section><h2>The ruler"
        + RULER_HEADING.format(version=E(ruler.version)) + "</h2>"
        f'<p class="sub">Everything above was ordered by this. It is data, at '
        f'<code>{E(str(ruler.path))}</code> — edit it and the whole heap '
        "re-scores on the next render, because ordering is derived at read. "
        "The same file is the system prompt "
        "<code>svrn backlog add</code> scores against, so the machine scorer "
        "and this page cannot drift apart.</p>"
        f'<div class="card"><div class="body">{"".join(rows)}</div></div></section>'
    )


def render_footer(read: StoreRead, items, malformed, generated_at: str) -> str:
    vetted_n = sum(1 for i in items if pullable(i))
    bits = [
        f"Rendered {E(generated_at)} from <code>{E(str(read.path))}</code> "
        f"(kind=todo, related_entity=backlog, live only).",
        f"{len(items)} item(s) read; {vetted_n} vetted, {len(items) - vetted_n} unvetted.",
    ]
    if read.error:
        bits.append(f'<span class="bad">Store could not be read: {E(read.error)}</span>')
    if read.other_todos:
        bits.append(
            f'<span class="bad">{read.other_todos} live kind=todo note(s) carry no '
            "related_entity=backlog and are NOT on this page</span> — either not "
            "backlog items (the seat's own business stays on the "
            "<code>comaintainer-seat</code> anchor) or not yet migrated. Absent "
            "from the heap, present in the store.")
    else:
        bits.append("Every live kind=todo note in the store is a backlog item.")
    if malformed:
        bits.append(f'<span class="bad">{len(malformed)} malformed item line(s)</span>: '
                    + "; ".join(f"{E(m.note_id[:8])}"
                                + (f" line {m.lineno}" if m.lineno else "")
                                + f" — {E(m.err)}: <code>{E(m.raw)}</code>"
                                for m in malformed))
    else:
        bits.append("No malformed item lines.")
    bits.append(f"Ranked by value ruler v{E(RULER.version)}, read from "
                f"<code>{E(str(RULER.path))}</code> and printed in full above. "
                f"{E(flat(RULER.roi_rule))}")
    return '<div class="foot">' + "<br>".join(bits) + "</div>"


def build_page(read: StoreRead, items, groups, top_item, top_group,
               malformed, now: dt.datetime) -> str:
    vetted_n = sum(1 for i in items if pullable(i))
    chunk_n = sum(1 for g in groups if g.is_chunk)
    chips = "".join(
        f'<span class="chip"><b>{v}</b> {E(k)}</span>' for k, v in [
            ("items", len(items)), ("vetted", vetted_n),
            ("unvetted", len(items) - vetted_n), ("chunks", chunk_n),
        ])
    head = (
        "<section><h1>The backlog — pull, do not push</h1>"
        '<p class="sub">Ranked by ROI = Value / Cost. The store is the notes '
        "store; this page is a view of it and writes nothing back.</p>"
        f'<div class="chips">{chips}</div></section>'
    )
    body = head + render_pull_banner(top_item, top_group, items) \
        + render_heap(groups, top_item) \
        + render_ruler(RULER) \
        + render_footer(read, items, malformed, local_str(now))
    return (
        '<!doctype html>\n<html lang="en"><head><meta charset="utf-8">'
        '<meta name="viewport" content="width=device-width,initial-scale=1">'
        f"<title>Backlog — {E(now.astimezone().strftime('%Y-%m-%d'))}</title>"
        f"<style>{CSS}</style></head><body><main>{body}</main></body></html>\n"
    )


def local_str(stamp: dt.datetime) -> str:
    return stamp.astimezone().strftime("%Y-%m-%d %H:%M")


def out_path() -> Path:
    env = os.environ.get("CO_BACKLOG_OUT")
    if env:
        return Path(env).expanduser()
    return Path.home() / ".sovereign" / "comaintainer" / "backlog.html"


def render(db: Path, out: Path) -> Path:
    now = dt.datetime.now(dt.timezone.utc)
    read, items, groups, top_item, top_group, malformed = load_backlog(db)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(
        build_page(read, items, groups, top_item, top_group, malformed, now),
        encoding="utf-8")
    return out


# --- --pull: the order draft ----------------------------------------------


def pull_draft(db: Path, today: str) -> tuple:
    """(text, exit_code). The draft is PRE-FILLED, not authored: every
    line traces to an item's own words. Sections the backlog cannot
    speak to are left for the seat, named as such — the seat and the
    operator fill Lane/Engine/Budget, which is the M0 line."""
    read, items, groups, top_item, top_group, malformed = load_backlog(db)
    if read.error:
        return (f"co-backlog: {read.error}\n", 2)
    if top_item is None:
        vetted_n = sum(1 for i in items if pullable(i))
        return (
            f"co-backlog: nothing to pull — {len(items)} item(s) in the backlog, "
            f"{vetted_n} vetted, 0 pullable.\n"
            "An item is pullable only once it carries a clean header plus a "
            "`Done-when:` and an `Evidence:` line. Run --open to see which line "
            "each item is missing.\n", 3)

    mates = [i for i in top_group.items if i is not top_item]
    pull = [top_item] + [m for m in mates if pullable(m)]
    held = [m for m in mates if not pullable(m)]

    slug = re.sub(r"[^a-z0-9]+", "-", (top_item.objective or "backlog-pull").lower())
    slug = slug.strip("-")[:40] or "backlog-pull"

    L = []
    L.append("---")
    L.append("schema: work-order/v1")
    L.append(f"id: {slug}")
    L.append("status: draft")
    L.append(f"drafted: {today}")
    L.append("approved: pending")
    L.append("---")
    L.append("")
    L.append(f"# Order: {top_item.objective or '(objective absent from the item)'}")
    L.append("")
    L.append("<!-- PRE-FILLED BY scripts/co-backlog.py --pull from the backlog's")
    L.append("     top chunk. Every line below is an item's own words; nothing")
    L.append("     here is authored by the renderer. The seat edits, the")
    L.append("     operator approves or edits it — M0 is unchanged. -->")
    L.append("")
    L.append("## Objective")
    L.append("")
    L.append(f"Serves: {top_item.objective or '(absent)'}")
    L.append("")
    for it in pull:
        L.append(f"- [{it.short}] {it.fields.get('Value', '(no value line)')}")
        L.append(f"  Approach: {it.fields.get('Approach', '(none stated)')}")
        if it.blocks:
            L.append(f"  Blocks: {it.blocks}")
        if it.prose:
            for line in it.prose.split("\n"):
                L.append(f"  {line}" if line.strip() else "")
        L.append("")
    L.append("Done when:")
    for it in pull:
        L.append(f"  - [{it.short}] {it.fields.get('Done-when', '')}")
    L.append("")
    L.append("Not worth continuing if: <!-- the backlog does not carry this; "
             "the seat writes it before the operator sees the draft -->")
    L.append("")
    L.append("## Lane")
    L.append("")
    L.append("<!-- Not in the backlog item. Seat fills. The item's Evidence line "
             "is the starting point: -->")
    for it in pull:
        L.append(f"  [{it.short}] Evidence: {it.fields.get('Evidence', '')}")
    L.append("")
    L.append("## Scope")
    L.append("")
    L.append("<!-- The items' Approach lines name the existing surfaces this")
    L.append("     builds on; they are the seat's starting point for Scope. -->")
    for it in pull:
        L.append(f"  [{it.short}] {it.fields.get('Approach', '(none stated)')}")
    L.append("")
    L.append("## Engine")
    L.append("")
    L.append("(none — the seat RECOMMENDS, the operator approves or edits)")
    L.append("")
    L.append("## Budget")
    L.append("")
    total = sum(i.cost_chunks or 0 for i in pull)
    L.append(f"{total} session-chunk(s) by the items' own Cost lines "
             f"({', '.join(f'{i.short}={i.cost}' for i in pull)}).")
    L.append("")
    L.append("## Seams")
    L.append("")
    L.append("(none)")
    L.append("")
    L.append("<!-- Provenance ------------------------------------------------")
    L.append(f"     store:        {read.path}")
    L.append(f"     backlog:      {len(items)} item(s), "
             f"{sum(1 for i in items if pullable(i))} vetted")
    L.append(f"     pulled:       {', '.join(i.short for i in pull)} "
             f"(group ROI {fmt_roi(top_group.roi)})")
    if held:
        L.append(f"     HELD BACK:    {', '.join(i.short for i in held)} — chunk mates")
        for h in held:
            L.append(f"                   {h.short}: {'; '.join(h.missing)}")
    else:
        L.append("     held back:    none — every mate in the chunk is vetted")
    if malformed:
        L.append(f"     malformed:    {len(malformed)} item line(s) could not be "
                 "parsed; see --open")
    L.append("     Close the loop: retire the pulled note(s) with a pointer to")
    L.append("     this order once it lands (svrn notes rationalize).")
    L.append("-->")
    return ("\n".join(L) + "\n", 0)


# --- the lane -------------------------------------------------------------
#
# --self-test is the gate, and it is a gate that has been WATCHED TO FAIL
# — on every run, not once in the author's terminal. The battery runs
# four times: clean (must be all green) and then once under each of the
# three injected defects the order names, each of which must turn a
# NAMED check red. A defect that no longer reddens the battery is itself
# reported as a failure, so the day someone weakens a check, this says
# so instead of going quietly green (ARCH §18.1, §18.2).

FIXTURE = [
    # High value, high cost -> ROI 5/3 = 1.67. A value-sorter puts this
    # near the top; the ruler does not. Half of check 1's discrimination.
    ("aaaa1111", "Objective: native grounding H0\n"
                 "Value: 5 — A Grounded: cuts wrong-accepts, measured 2/7 -> 0/7\n"
                 "Cost: L (session-chunks)\n"
                 "Approach: extend the existing holdings gate rather than a new pass\n"
                 "Chunks-with: none\n"
                 "Done-when: wrong-accepts at 0/7 on the frozen bank\n"
                 "Evidence: D2_SHAKEOUT.md, commit 224a7bbd\n"
                 "\nThe long one.\n"),
    # Lower value, cheap -> ROI 4.00. The top PULLABLE item, and the
    # other half of check 1: it must outrank aaaa1111 despite value 4<5.
    ("bbbb2222", "Objective: native grounding H0\n"
                 "Value: 4 — B Responsive: recovers wrongly-declined answers\n"
                 "Cost: S (session-chunks)\n"
                 "Approach: lower the abstain threshold in the existing decline path\n"
                 "Chunks-with: cccc3333\n"
                 "Done-when: competence-when-present above 0.80\n"
                 "Evidence: bench lane retrieval-prod baseline 0.71\n"
                 "\nThe cheap one.\n"),
    # bbbb2222's chunk mate, VETTED, so it rides along on --pull.
    # Group: value 7 / 2 chunks -> group ROI 3.50.
    ("cccc3333", "Objective: native grounding H0\n"
                 "Value: 3 — E Clean handoffs: typed stage output\n"
                 "Cost: S (session-chunks)\n"
                 "Approach: return the existing struct instead of a String\n"
                 "Chunks-with: bbbb2222\n"
                 "Done-when: the stage returns a typed struct, not a String\n"
                 "Evidence: sovereign/src/pipeline.rs:120\n"
                 "\nThe mate.\n"),
    # ROI 5.00 — the best in the fixture — but UNVETTED. It heads the
    # HEAP (correctly: the heap is ranked, and unvetted items are greyed
    # rather than hidden) and must never be the PULL. That gap is
    # check 2, and it is why the fixture makes the unvetted item the
    # single most attractive thing on the page.
    ("dddd4444", "Objective: native grounding H0\n"
                 "Value: 5 — A Grounded: something wonderful\n"
                 "Cost: S (session-chunks)\n"
                 "Approach: a real approach, so the unvetted reason is the missing pair\n"
                 "\nNo done-when, no evidence. Not pullable.\n"),
    # A malformed header line — check 3's input. It still SCORES
    # (ROI 1.00): a malformed line makes an item unvetted, not invisible.
    ("eeee5555", "Objective: native grounding H0\n"
                 "this line is not a key value pair\n"
                 "Value: 2 — D One sweep: fewer recheck loops\n"
                 "Cost: M (session-chunks)\n"
                 "Approach: reuse the existing recheck cache\n"
                 "\nMalformed on purpose.\n"),
    # Blocks aaaa1111, so it inherits value 5 over its own 1. Cost L
    # deliberately: inheritance must be visible (ROI 0.33 -> 1.67)
    # WITHOUT letting this item contend for the top pull, which would
    # confound check 2 with the Blocks rule.
    # Also the axis-F carrier (ruler v2): axis plays no part in ordering,
    # so this proves F parses and renders without perturbing the heap.
    ("ffff6666", "Objective: native grounding H0\n"
                 "Value: 1 — F Viable: a chore on the install path\n"
                 "Cost: L (session-chunks)\n"
                 "Approach: a chore on the existing installer script\n"
                 "Blocks: aaaa1111\n"
                 "Done-when: the chore is done\n"
                 "Evidence: note aaaa1111\n"
                 "\nInherits 5 from aaaa1111.\n"),
    # Complete in every other respect — Done-when, Evidence, a clean
    # header, and an ROI of 4.00 that would place it second on the heap
    # and make it the pull target ahead of bbbb2222. It is blocked
    # SOLELY by "Approach: unknown", which is the whole point of the
    # sizing rule (directive 341884f5).
    ("77770000", "Objective: native grounding H0\n"
                 "Value: 4 — A Grounded: a real win nobody has scoped\n"
                 "Cost: S (session-chunks)\n"
                 "Approach: unknown — needs a design pass\n"
                 "Done-when: the thing is done\n"
                 "Evidence: bench lane foo, baseline 0.5\n"
                 "\nUnsized on purpose.\n"),
    # A MACHINE-SCORED item, complete in every respect a human could
    # check: clean header, a real Approach, a Done-when, an Evidence, ROI
    # 2.50. Nothing about its CONTENT keeps it out of the pull queue —
    # only the `Scored-by:` stamp does — and its ROI of 5.00 ties the top
    # of the heap, so with the gate flipped off (defect
    # `machine-score-vets`) it does not merely become pullable, it
    # becomes THE PULL TARGET. That is the failure this gate exists to
    # prevent, and the battery watches it happen.
    ("cafe9999", "Objective: native grounding H0\n"
                 "Value: 5 — A Grounded: cuts wrong-accepts, measured 3/7 -> 1/7\n"
                 "Cost: S (session-chunks)\n"
                 "Approach: extend the existing holdings gate with the new check\n"
                 "Done-when: wrong-accepts at 1/7 on the frozen bank\n"
                 "Evidence: bench lane retrieval-prod, baseline 3/7\n"
                 "Producer: svrn backlog add\n"
                 "Scored-by: commonwealth/primary\n"
                 "Key: selftest:machine-scored\n"
                 "\nFiled by a machine. A person has not looked at it yet.\n"),
    # No Value line at all -> unscorable. Must sort LAST and never head
    # anything: an item we could not score cannot be the top of a heap.
    ("99990000", "Objective: native grounding H0\n"
                 "Cost: S (session-chunks)\n"
                 "Approach: irrelevant — this item has no Value line at all\n"
                 "\nNo Value line at all.\n"),
]

# The ruler's arithmetic, written out once so the expected order below
# is auditable rather than magic. Group ROI is summed value over summed
# chunks, which is why the bbbb/cccc pair sits at 3.50 and not at 4.00.
#
#   dddd4444          5 / 1 = 5.00   (unvetted: no done-when/evidence)
#   cafe9999          5 / 1 = 5.00   (unvetted: machine-scored, nothing else;
#                                     ties dddd4444 on ROI and loses on age)
#   77770000          4 / 1 = 4.00   (unvetted: Approach is "unknown")
#   bbbb2222+cccc3333 7 / 2 = 3.50   (chunk)
#   aaaa1111          5 / 3 = 1.67
#   ffff6666          5 / 3 = 1.67   (inherited value; older loses tie to aaaa)
#   eeee5555          2 / 2 = 1.00   (malformed -> unvetted)
#   99990000          unscored       (always last)
EXPECTED_HEAP = ["dddd4444", "cafe9999", "77770000", "bbbb2222", "cccc3333",
                 "aaaa1111", "ffff6666", "eeee5555", "99990000"]

FIXTURE_SCHEMA = """
CREATE TABLE notes (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, content TEXT NOT NULL,
  created_at INTEGER NOT NULL, retired_at INTEGER, tombstone INTEGER NOT NULL DEFAULT 0,
  related_entity TEXT, scope TEXT NOT NULL DEFAULT 'global'
);
"""


def _write_fixture(path: Path, rows, extra_todos: int = 0):
    conn = sqlite3.connect(path)
    conn.executescript(FIXTURE_SCHEMA)
    for n, (nid, body) in enumerate(rows):
        conn.execute(
            "INSERT INTO notes (id, kind, content, created_at, related_entity) "
            "VALUES (?,'todo',?,?,'backlog')",
            (nid + "-0000-0000-0000-000000000000", body, 1700000000 + n))
    for n in range(extra_todos):
        conn.execute(
            "INSERT INTO notes (id, kind, content, created_at, related_entity) "
            "VALUES (?,'todo',?,?,NULL)", (f"unmigrated-{n}", "an old todo", 1600000000))
    conn.commit()
    conn.close()


def _battery(db: Path, out: Path, check):
    """The whole battery, as a function of the current DEFECT. Every
    check here is asserted in both directions where a direction exists
    (co-closeout's rule): what must be on the page, and what must NOT."""
    page = render(db, out).read_text(encoding="utf-8")
    draft, code = pull_draft(db, "2026-08-09")

    # Scope each assertion to the region that actually carries it. The
    # first cut of this battery read ordering off the WHOLE page and so
    # measured the pull banner, not the heap — every ordering check
    # passed under the bad-roi-order defect because the banner it was
    # reading is not sorted at all. Validate the instrument, then the
    # result (ARCH §18.4).
    heap_html = page.split("The heap — every item", 1)[-1]
    footer_html = page.split('<div class="foot">', 1)[-1]
    banner_html = page.split("<h2>Next pull</h2>", 1)[-1].split("<section>", 1)[0]
    heap_order = list(dict.fromkeys(
        re.findall(r'<span class="ref">([0-9a-f]{8})</span>', heap_html)))

    # check 1 — the heap is ordered by ROI, not by raw value.
    check("the heap is in exact ROI order", heap_order == EXPECTED_HEAP,
          f"got {heap_order}, want {EXPECTED_HEAP}")
    check("the cheap 4-value item outranks the expensive 5-value one",
          "aaaa1111" in heap_order and "bbbb2222" in heap_order
          and heap_order.index("bbbb2222") < heap_order.index("aaaa1111"),
          "ROI 4.00 must beat ROI 1.67 even though 4 < 5")
    check("NEGATIVE: the UNSCORABLE item never heads the heap",
          heap_order and heap_order[0] != "99990000" and heap_order[-1] == "99990000",
          "an item with no Value line must sort last, never first")

    # check 2 — an unvetted item is never pullable, however good its ROI.
    check("the unvetted item renders greyed and says why",
          "unvetted — not pullable" in heap_html
          and "no `Done-when:`" in heap_html and "no `Evidence:`" in heap_html)
    check("the best-ROI item in the fixture IS the unvetted one",
          heap_order and heap_order[0] == "dddd4444",
          "check 2 only means something while dddd4444 tops the heap")
    check("the unvetted item is NOT the pull target",
          "dddd4444" not in banner_html and "dddd4444" not in draft,
          "dddd4444 has ROI 5.00 — the highest — and must still be unpullable")
    check("--pull pulled the top VETTED item",
          "[bbbb2222]" in draft and code == 0, f"exit {code}")
    check("--pull carried the vetted chunk mate", "[cccc3333]" in draft)

    # the sizing rule (directive 341884f5) — Cost must follow Approach.
    check("`Approach: unknown` blocks an OTHERWISE-COMPLETE item",
          "77770000" in heap_order and "77770000" not in banner_html
          and "77770000" not in draft,
          "77770000 has a Done-when, an Evidence and ROI 4.00 — better than "
          "the pull target — and must still be unpullable")
    check("it says the approach is why, not the done-when",
          "needs a design pass before it can be sized" in heap_html)
    check("the approach renders ABOVE the collapsed verbatim note",
          heap_html.find("Approach — how this gets solved")
          < heap_html.find("the note, verbatim"),
          "the note is the evidence; the approach is the point")
    check("--pull carries the pulled item's Approach into the draft",
          "Approach: lower the abstain threshold" in draft)
    no_appr = parse_item("probe-a", 0, "Objective: o\nValue: 3 — A x: y\n"
                                       "Cost: S\nDone-when: d\nEvidence: e\n", [])
    check("NEGATIVE: a MISSING Approach is unvetted too, not just 'unknown'",
          not vet(no_appr) and any("no `Approach:`" in x for x in no_appr.missing))

    # the machine-score gate (order backlog-insert-system D2) — a score
    # the local model drafted is a DRAFT, and vetting is a human act.
    # cafe9999 is the failing input: complete header, real Approach,
    # Done-when, Evidence, ROI 2.50, and unpullable for one reason only.
    mach = [i for i in load_backlog(db)[1] if i.short == "cafe9999"][0]
    check("a machine-scored item is NOT pullable", not pullable(mach),
          f"missing: {mach.missing}")
    check("and it is unpullable for the SCORE, not for a missing field",
          all("Done-when" not in m and "Evidence" not in m and "Approach" not in m
              for m in mach.missing),
          f"cafe9999 must be complete except for the stamp; got {mach.missing}")
    check("the page names the model that scored it",
          "scored by commonwealth/primary, not by a person" in heap_html)
    check("the machine-scored item never reaches the pull queue",
          "cafe9999" not in banner_html and "cafe9999" not in draft,
          "cafe9999 ties the highest ROI on the page — with the gate off it "
          "is what the seat pulls next")
    check("NEGATIVE: clearing `Scored-by:` is what vets it — nothing else",
          vet(parse_item("probe-m", 0, "\n".join(
              l for l in dict(FIXTURE)["cafe9999"].split("\n")
              if not l.startswith("Scored-by:")), [])),
          "the same item without the stamp must vet — otherwise this gate "
          "is measuring some other missing field")

    # check 3 — a malformed item line is reported in the FOOTER, never
    # swallowed. Scoped to the footer: the same strings also appear in
    # the item's own "unvetted because" block, which would let these
    # pass while the footer had gone silent.
    check("the footer names the malformed line count",
          "malformed item line(s)" in footer_html)
    check("the footer names the offending item and its line number",
          "eeee5555" in footer_html and "line 2" in footer_html)
    check("the footer shows the raw offending text",
          "this line is not a key value pair" in footer_html)
    check("the malformed item still renders, scored and greyed",
          "eeee5555" in heap_order,
          "malformed makes an item unvetted, not invisible")

    # the record, and what it does not say
    check("the footer names the resolved store path", str(db) in footer_html)
    check("non-backlog todos are reported, not omitted",
          "are NOT on this page" in footer_html
          and "Absent from the heap, present in the store." in footer_html)
    check("the Blocks rule lifted the blocker's value",
          "inherited from aaaa1111" in heap_html)

    # ruler v2 — axis F parses, renders, and the axis set still CLOSES.
    check("axis F parses and renders as Viable", "F Viable" in heap_html)
    probe_ok, probe_bad = [], []
    parse_item("probe-f", 0, "Objective: o\nValue: 3 — F Viable: x\n"
                             "Cost: S\n", probe_ok)
    it_g = parse_item("probe-g", 0, "Objective: o\nValue: 3 — G Whatever: x\n"
                                    "Cost: S\n", probe_bad)
    check("NEGATIVE: G is not an axis — the set is closed at F",
          "`Value:` names no axis A-F" in it_g.problems,
          "an open axis set would let any capital letter score")
    check("NEGATIVE: no emoji anywhere in the rendered page",
          all(ord(c) < 0x2190 or c in "—§·…" for c in page),
          "operator convention: no emojis in any output")
    check("NEGATIVE: the draft does not invent a Not-worth-continuing-if",
          "the seat writes it before the operator sees the draft" in draft)


def _empty_and_absent(check):
    """Direction 2: an empty store and an absent store must each be
    honest, and must not look like each other."""
    with tempfile.TemporaryDirectory(prefix="co-backlog-empty-") as tmp:
        tmp = Path(tmp)
        db = tmp / "empty.db"
        _write_fixture(db, [], extra_todos=0)
        out = tmp / "empty.html"
        page = render(db, out).read_text(encoding="utf-8")
        draft, code = pull_draft(db, "2026-08-09")
        check("empty store: says the heap is empty because the store says so",
              "The heap is empty because the store" in page)
        check("empty store: --pull refuses with a distinct exit code", code == 3,
              f"exit {code}, want 3")
        check("NEGATIVE: no fixture leaks into the empty render",
              "aaaa1111" not in page and "bbbb2222" not in page)

        missing = tmp / "nope.db"
        page2 = render(missing, tmp / "absent.html").read_text(encoding="utf-8")
        draft2, code2 = pull_draft(missing, "2026-08-09")
        check("absent store: the page NAMES the failure to read",
              "Store could not be read" in page2)
        check("absent store: --pull exits 2, distinct from empty's 3", code2 == 2,
              f"exit {code2}, want 2")
        check("NEGATIVE: an absent store does not render as an empty backlog",
              "Store could not be read" in page2 and "0 item(s) read" in page2)


def ruler_divergence(page: str, ruler: Ruler) -> list:
    """Everything `ruler` says that `page` does not. Empty list == the
    rendered page and the ruler file agree.

    This is the divergence check itself, factored out so the self-test can
    run it in BOTH directions: green against the ruler the page was
    rendered from, and RED against a different one. A check that cannot be
    made to say "divergent" is not a check (ARCH §18.1)."""
    section = page.split("<h2>The ruler", 1)[-1].split("</section>", 1)[0]
    missing = []
    # The whole heading phrase, not a bare `v{version}` substring: "v2" is
    # a prefix of "v2-edited-by-selftest", so the loose form reported
    # agreement between two different rulers. Caught by the watched
    # failure below, which is what it is for.
    if RULER_HEADING.format(version=ruler.version) not in section:
        missing.append(f"version v{ruler.version}")
    if E(str(ruler.path)) not in section:
        missing.append(f"path {ruler.path}")
    for letter, name, yard in ruler.axes:
        if f"{E(letter)} — {E(name)}" not in section:
            missing.append(f"axis {letter} — {name}")
        if E(flat(yard)) not in section:
            missing.append(f"yardstick for axis {letter}")
    for level in ruler.scale:
        if E(flat(level)) not in section:
            missing.append(f"scale level {flat(level)[:12]!r}")
    for label, text in (("blocks rule", ruler.blocks_rule), ("ROI rule", ruler.roi_rule)):
        if E(flat(text)) not in section:
            missing.append(label)
    return missing


EDITED_RULER = """
version = "2-edited-by-selftest"
[provenance]
v1 = "a self-test edit, never committed"
[[axis]]
letter = "A"
name = "Grounded"
yardstick = "an EDITED yardstick that appears nowhere in the committed ruler"
[[axis]]
letter = "F"
name = "Reachable"
yardstick = "axis F, renamed by the self-test to prove the page follows the file"
[[axis]]
letter = "G"
name = "Invented"
yardstick = "an axis that does not exist in the committed ruler"
[scoring]
scale = ["5 = an edited top of the scale", "1 = an edited bottom"]
blocks_rule = "an edited blocks rule"
roi = "an edited ROI rule"
[value]
min = 1
max = 5
[cost.chunks]
S = 1
M = 2
L = 3
[format]
header_keys = ["Objective", "Value", "Cost", "Approach", "Done-when",
               "Evidence", "Scored-by", "Key", "Producer", "Chunks-with",
               "Blocks"]
"""


def _ruler_as_data(db: Path, tmp: Path, check):
    """Deliverable 1's gate: the ruler is DATA, and the page proves it.

    Watched to fail by editing the TOML — literally. The self-test writes
    an edited ruler, re-renders, and requires (a) the page to have
    followed the edit, (b) the divergence check to go RED when the page is
    compared against the ruler it was NOT rendered from, and (c) the
    parser's axis set to have moved too, not just the prose on the page.
    Without (b) the check would pass on a page that ignored the file."""
    committed = load_ruler(ruler_path())
    page = render(db, tmp / "ruler-committed.html").read_text(encoding="utf-8")

    check("the page prints the ruler it loaded, in full",
          not ruler_divergence(page, committed),
          "missing: " + "; ".join(ruler_divergence(page, committed)[:4]))
    check("the footer cites the ruler file and version, not a doc header",
          f"value ruler v{committed.version}" in page
          and str(committed.path) in page)

    edited_path = tmp / "backlog-ruler.edited.toml"
    edited_path.write_text(EDITED_RULER, encoding="utf-8")
    try:
        edited = reload_ruler(edited_path)
        page2 = render(db, tmp / "ruler-edited.html").read_text(encoding="utf-8")

        # (a) the page followed the file.
        check("editing the TOML changes the rendered ruler",
              not ruler_divergence(page2, edited),
              "missing: " + "; ".join(ruler_divergence(page2, edited)[:4]))
        check("the renamed axis renders under its NEW name",
              "F — Reachable" in page2 and "F — Viable" not in page2,
              "axis F is named 'Viable' in the committed ruler and 'Reachable' "
              "in the edited one; the page must show the loaded one")

        # (b) the watched failure — the same check, compared against the
        # ruler this page was NOT rendered from, must come back RED.
        drift = ruler_divergence(page2, committed)
        check("WATCHED: the divergence check REDDENS against the wrong ruler",
              bool(drift),
              "comparing the edited-ruler page against the committed ruler "
              "reported agreement — the check is a rubber stamp")
        check("WATCHED: and it NAMES what diverged",
              any("axis F" in d for d in drift) and any("version" in d for d in drift),
              f"got {drift[:4]}")

        # (c) the axis set moved too — the TOML drives the PARSER, not
        # just the prose. G is not an axis in the committed ruler; the
        # battery has a negative check proving that.
        it_g = parse_item("probe-g2", 0, "Objective: o\nValue: 3 — G Invented: x\n"
                                         "Cost: S\n", [])
        check("the edited ruler's new axis PARSES — the file drives the parser",
              it_g.axes == ["G"] and not it_g.problems, f"axes {it_g.axes}, "
              f"problems {it_g.problems}")
    finally:
        reload_ruler(ruler_path())

    # and the restore actually restored — otherwise every later check in
    # this run would be measuring the edited ruler.
    it_g = parse_item("probe-g3", 0, "Objective: o\nValue: 3 — G Invented: x\n"
                                     "Cost: S\n", [])
    check("NEGATIVE: the committed ruler is back — G is not an axis again",
          f"`Value:` names no axis {RULER.letters}" in it_g.problems,
          f"ruler is v{RULER.version} from {RULER.path}")


def self_test() -> int:
    global DEFECT
    failures, watched = [], []

    def mk(sink):
        def check(name, ok, detail=""):
            print(f"  {'PASS' if ok else 'FAIL'}  {name}"
                  + (f" — {detail}" if detail else ""))
            if not ok:
                sink.append(name)
        return check

    with tempfile.TemporaryDirectory(prefix="co-backlog-selftest-") as tmp:
        tmp = Path(tmp)
        db = tmp / "fixture.db"
        _write_fixture(db, FIXTURE, extra_todos=3)

        print("battery — clean (no defect injected): every check must pass")
        DEFECT = None
        _battery(db, tmp / "clean.html", mk(failures))
        _empty_and_absent(mk(failures))
        print("\nthe ruler is data — and the page is watched to follow an edit")
        _ruler_as_data(db, tmp, mk(failures))

        # Now watch the gate fail. Each defect must redden the battery.
        for defect in DEFECTS:
            print(f"\nwatched failure — defect {defect!r}: the battery must go RED")
            DEFECT = defect
            red = []
            try:
                _battery(db, tmp / f"defect-{defect}.html", mk(red))
            except Exception as exc:  # a defect that crashes still counts as red
                red.append(f"raised {type(exc).__name__}: {exc}")
            DEFECT = None
            if red:
                print(f"  WATCHED  {defect} reddened {len(red)} check(s): "
                      + "; ".join(red[:3]) + ("; ..." if len(red) > 3 else ""))
                watched.append((defect, red))
            else:
                print(f"  UNWATCHED  {defect} changed NOTHING — the checks that "
                      "are supposed to catch it do not")
                failures.append(f"defect {defect} was not caught by any check")

    DEFECT = None
    print()
    if failures:
        print(f"self-test FAILED — {len(failures)} check(s): " + "; ".join(failures))
        return 1
    print(f"self-test PASSED — clean battery green, and all {len(DEFECTS)} injected "
          "defects were watched to fail:")
    for defect, red in watched:
        print(f"  {defect}: {len(red)} check(s) red — {red[0]}")
    return 0


# --- entry ----------------------------------------------------------------


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        prog="co-backlog.py",
        description="Render the seat's ranked, pull-based backlog from the notes store.")
    ap.add_argument("--open", action="store_true", dest="open_it",
                    help="render the heap and open it in a browser")
    ap.add_argument("--pull", action="store_true",
                    help="print the top chunk as a pre-filled order draft, on stdout")
    ap.add_argument("--self-test", action="store_true",
                    help="run the lane: the clean battery plus the three watched "
                         "defect injections, then exit")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()

    db = notes_db_path()
    if args.pull:
        text, code = pull_draft(db, dt.date.today().isoformat())
        (sys.stdout if code == 0 else sys.stderr).write(text)
        return code

    if not db.exists():
        # §18.3: absence is reported, never rendered as an empty success.
        print(f"co-backlog: no notes store at {db} — nothing to render. "
              "(Set CO_BACKLOG_NOTES_DB if it lives elsewhere.)", file=sys.stderr)
        return 2
    out = render(db, out_path())
    print(out)
    if args.open_it:
        import webbrowser
        webbrowser.open(out.as_uri())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
