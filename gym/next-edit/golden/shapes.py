#!/usr/bin/env python3
"""Frontier shape taxonomy + mechanical detectors for the next-edit golden set.

Spec: `sovereign/docs/specs/NEXT_EDIT_BAKEOFF.md` §2. Sizing and gates:
`gym/next-edit/golden/README.md`.

WHY THIS EXISTS. The `gen` bank's 30 positives are drawn from exactly the
three shapes `should_consult` admits, so it is a mirror of the gate rather
than a sample of the world: an episode the gate declines by construction
can never become a measurable missed-fire, and no sample size fixes that.
This module enumerates the shapes a developer actually produces —
including several the current gate refuses — so the bank can measure the
ceiling of the DESIGN, not just the accuracy of the model inside it.

WHY NOT `harvest.py`. That miner reads only `difflib` `replace` opcodes
with equal line counts (`hunks_of`), i.e. single-line substitutions. Most
of the frontier is insertions and deletions — adding an import, adding a
match arm, inserting a guard, deleting a field and its uses — which are
structurally invisible to it. The diff model here carries insert/delete/
replace with unequal counts.

LABELS ARE BY CONSTRUCTION. Ground truth for every positive is the
remainder of the same commit: the edits the author actually went on to
make. No teacher, no judge, no model opinion anywhere in this file.
"""

from __future__ import annotations

import difflib
import re
from dataclasses import dataclass, field
from typing import Callable, Iterable

# Reuse the rule-lane replica so gate admissibility is computed by the
# same logic the daemon runs, never re-derived here.
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from harvest import CTX_CHARS, expand_rule, line_starts, strip_common  # noqa: E402
from harvest import LANG_OF as _BASE_LANG_OF  # noqa: E402

# `harvest.py`'s table was written for THIS repo's languages, so mining a
# multi-language corpus through it silently drops every Java, Go, C,
# Ruby and PHP file — the exact diversity the golden set exists to buy.
LANG_OF = dict(_BASE_LANG_OF)
LANG_OF.update({
    ".go": "go", ".java": "java", ".kt": "kotlin", ".kts": "kotlin",
    ".c": "c", ".h": "c", ".cc": "cpp", ".cpp": "cpp", ".hpp": "cpp",
    ".rb": "ruby", ".php": "php", ".cs": "csharp", ".swift": "swift",
    ".jsx": "javascript", ".mjs": "javascript", ".scala": "scala",
})

# ---- diff model -------------------------------------------------------


@dataclass(frozen=True)
class Edit:
    """One contiguous line-level change, in OLD-file line coordinates."""

    kind: str  # "replace" | "insert" | "delete"
    old_start: int
    old_end: int  # exclusive; == old_start for a pure insert
    old_lines: tuple[str, ...]
    new_lines: tuple[str, ...]

    @property
    def old_text(self) -> str:
        return "".join(l + "\n" for l in self.old_lines)

    @property
    def new_text(self) -> str:
        return "".join(l + "\n" for l in self.new_lines)

    @property
    def line_delta(self) -> int:
        return len(self.new_lines) - len(self.old_lines)

    def touches(self, pat: str) -> bool:
        return any(pat in l for l in self.old_lines + self.new_lines)

    def added(self) -> str:
        """Text present in new but not old — what this edit introduced."""
        return "".join(l for l in self.new_lines if l not in self.old_lines)

    def removed(self) -> str:
        return "".join(l for l in self.old_lines if l not in self.new_lines)


def diff_edits(old: str, new: str) -> list[Edit]:
    """Every line-level opcode, insertions and deletions included.

    An equal-length `replace` block is SPLIT per line. A developer edits
    one line at a time and the extension coalesces per burst, so a
    5-line replace is five edit units, not one — and a detector looking
    for "the same single-line change at N sites" is structurally blind
    if contiguous sites arrive fused. (Measured: fusing left
    `param_insert` at zero yield across 400 commits.) Unequal-length
    replaces stay whole; they have no line pairing to split on.
    """
    o, n = old.split("\n"), new.split("\n")
    out: list[Edit] = []
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(
        None, o, n, autojunk=False
    ).get_opcodes():
        if tag == "equal":
            continue
        if tag == "replace" and (i2 - i1) == (j2 - j1):
            for k in range(i2 - i1):
                if o[i1 + k] == n[j1 + k]:
                    continue
                out.append(
                    Edit("replace", i1 + k, i1 + k + 1, (o[i1 + k],), (n[j1 + k],))
                )
            continue
        out.append(Edit(tag, i1, i2, tuple(o[i1:i2]), tuple(n[j1:j2])))
    return out


@dataclass
class FileDiff:
    repo: str
    commit: str
    date: str
    path: str
    language: str
    old: str
    new: str
    edits: list[Edit] = field(default_factory=list)


@dataclass
class Episode:
    """An interrupted editing session with its ground truth.

    `exemplars` are replayed as edit history and applied to the document
    the model sees; `truth` is what the author actually did next and is
    held out. Both are disjoint line ranges of the same commit.
    """

    shape: str
    exemplars: list[Edit]
    truth: list[Edit]
    note: str = ""
    # "apply" = the document is old + exemplars. "old" = the document is
    # unchanged, which is what a revert leaves behind: the pair of edits
    # happened and cancelled, so applying them would be wrong.
    doc_mode: str = "apply"


# ---- language surface -------------------------------------------------
#
# Deliberately shallow: these are RECALL filters over a huge commit
# corpus, not parsers. A false positive costs one discarded episode
# (every candidate is re-validated downstream); a parser costs a
# tree-sitter dependency per language and weeks of bring-up.

IMPORT_RE = re.compile(
    r"^\s*(?:import\s|from\s+\S+\s+import\s|use\s|#include\s|using\s|require\s*\(|"
    r"const\s+\{[^}]*\}\s*=\s*require)"
)
FUNC_DECL_RE = re.compile(
    r"^\s*(?:pub\s+)?(?:async\s+)?(?:fn|def|func|function|static|public|private|"
    r"protected|internal)\b[^=]*?\b(\w+)\s*\("
)
TYPE_DECL_RE = re.compile(r"^\s*(?:pub\s+)?(?:struct|class|interface|type|enum)\s+(\w+)")
ENUM_MEMBER_RE = re.compile(r"^\s*(\w+)\s*(?:=\s*[^,]+)?,\s*$")
MATCH_ARM_RE = re.compile(r"^\s*(?:case\s+)?([\w:.]+)\s*(?:=>|:|->)")
GUARD_RE = re.compile(r"^\s*(?:if|guard|unless)\b.*\b(?:return|throw|raise|continue|break)\b")
DOC_RE = re.compile(r"^\s*(?://[/!]|#|\*|/\*\*|\"\"\")")

ERROR_IDIOMS = (
    (".unwrap()", "?"),
    (".expect(", "?"),
    ("panic!(", "return Err"),
    ("throw ", "return "),
    ("None", "Err("),
)


def ident_of(line: str, pat: re.Pattern) -> str | None:
    m = pat.search(line)
    return m.group(1) if m and m.groups() else None


def casing_variants(name: str) -> set[str]:
    """snake/SCREAMING/camel/Pascal renderings of one identifier."""
    if "_" in name:
        parts = [p for p in name.split("_") if p]
    else:
        parts = [p.lower() for p in re.findall(r"[A-Z]?[a-z0-9]+|[A-Z]+(?![a-z])", name)]
    if not parts:
        return set()
    return {
        "_".join(parts),
        "_".join(parts).upper(),
        parts[0] + "".join(p.capitalize() for p in parts[1:]),
        "".join(p.capitalize() for p in parts),
    }


# ---- detector framework ----------------------------------------------


def anchor_followers(
    fd: FileDiff,
    shape: str,
    anchor: Callable[[Edit], str | None],
    follows: Callable[[Edit, str], bool],
    min_followers: int = 2,
    exemplar_count: int = 2,
) -> list[Episode]:
    """The shape most frontier cases share: one anchor edit establishes an
    intent, N later edits carry it out. Exemplars are the anchor plus the
    first followers; truth is every remaining follower.

    ORDERING IS DOCUMENT ORDER, which is a known proxy for editing order
    (§2 Stratum 2). A commit records what changed, never in what sequence
    — recovering plausible sequence is the session-synthesis stage, and
    until it lands every episode here assumes top-to-bottom, the same
    idealisation the CUHK benchmark documents as a limitation.
    """
    out: list[Episode] = []
    for a in fd.edits:
        key = anchor(a)
        if not key:
            continue
        followers = [e for e in fd.edits if e is not a and follows(e, key)]
        if len(followers) < min_followers:
            continue
        followers.sort(key=lambda e: e.old_start)
        take = max(1, exemplar_count - 1)
        if len(followers) <= take:
            continue
        out.append(
            Episode(
                shape=shape,
                exemplars=[a] + followers[:take],
                truth=followers[take:],
                note=f"anchor={key!r}",
            )
        )
    return out


def repeated(
    fd: FileDiff,
    shape: str,
    key_of: Callable[[Edit], str | None],
    min_sites: int = 3,
    exemplar_count: int = 2,
) -> list[Episode]:
    """N edits sharing a mechanical key, with no distinguished anchor."""
    groups: dict[str, list[Edit]] = {}
    for e in fd.edits:
        k = key_of(e)
        if k:
            groups.setdefault(k, []).append(e)
    out: list[Episode] = []
    for k, g in groups.items():
        if len(g) < min_sites:
            continue
        g.sort(key=lambda e: e.old_start)
        out.append(
            Episode(shape, g[:exemplar_count], g[exemplar_count:], note=f"key={k!r}")
        )
    return out


# ---- the shapes -------------------------------------------------------
#
# Registry, not a match (ARCH §4): shapes are an open set that grows as
# the frontier is explored, and each row records whether the CURRENT
# consult gate would even look at it. The `gate` column is the point of
# the exercise — a shape marked `declines` that models handle well is a
# gap in our design, not in the field's models.


def s_literal_fanout(fd: FileDiff) -> list[Episode]:
    def key(e: Edit) -> str | None:
        if e.kind != "replace" or len(e.old_lines) != 1 or len(e.new_lines) != 1:
            return None
        a, b = e.old_lines[0], e.new_lines[0]
        p, s = strip_common(a, b)
        mid_a, mid_b = a[p : len(a) - s], b[p : len(b) - s]
        if not mid_a and not mid_b:
            return None
        r = expand_rule({"before": mid_a, "after": mid_b, "left": a[:p], "right": a[len(a) - s :]})
        return f"{r['find']}=>{r['replace']}" if r else None

    return repeated(fd, "literal_fanout", key, min_sites=3)


def s_signature_fanout(fd: FileDiff) -> list[Episode]:
    def anchor(e: Edit) -> str | None:
        if e.kind != "replace":
            return None
        for old, new in zip(e.old_lines, e.new_lines):
            a, b = ident_of(old, FUNC_DECL_RE), ident_of(new, FUNC_DECL_RE)
            if a and a == b and old != new:
                return a
        return None

    return anchor_followers(
        fd, "signature_fanout", anchor, lambda e, k: e.touches(k + "(")
    )


def s_param_insert(fd: FileDiff) -> list[Episode]:
    def key(e: Edit) -> str | None:
        if e.kind != "replace" or len(e.old_lines) != 1:
            return None
        a, b = e.old_lines[0], e.new_lines[0]
        # A pure insertion INSIDE the line: common prefix and suffix
        # both survive, nothing is removed. Substring containment does
        # NOT work here — `foo(x)` -> `foo(x, y)` moves the closing
        # paren, so the old line is not a substring of the new one.
        # (Measured: containment yielded 2 episodes in 400 commits.)
        p, s = strip_common(a, b)
        mid_a, mid_b = a[p : len(a) - s], b[p : len(b) - s]
        if mid_a or not mid_b.strip():
            return None
        # The insertion must land inside a call's argument list.
        calls = re.findall(r"(\w+)\s*\(", a[:p])
        return f"call:{calls[-1]}" if calls else None

    return repeated(fd, "param_insert", key, min_sites=3)


def s_field_init(fd: FileDiff) -> list[Episode]:
    def key(e: Edit) -> str | None:
        if e.kind != "insert" or len(e.new_lines) != 1:
            return None
        m = re.match(r"^\s*([\w\"']+)\s*[:=]", e.new_lines[0])
        return f"field:{m.group(1)}" if m else None

    return repeated(fd, "field_init", key, min_sites=3)


def s_import_addition(fd: FileDiff) -> list[Episode]:
    """An import lands, then the symbol it names starts being used."""

    def anchor(e: Edit) -> str | None:
        if e.kind not in ("insert", "replace"):
            return None
        for l in e.new_lines:
            if l in e.old_lines or not IMPORT_RE.match(l):
                continue
            syms = re.findall(r"[\w]+", l)
            if syms:
                return syms[-1]
        return None

    return anchor_followers(
        fd, "import_addition", anchor, lambda e, k: e.touches(k), min_followers=2
    )


def s_delete_propagation(fd: FileDiff) -> list[Episode]:
    """A declaration goes away and its references must follow."""

    # The anchor must look like a DECLARATION, and a follower must carry
    # the identifier as a whole word. A looser pair (any 4+ char token,
    # substring match) over-matched 4x on this repo's history — nearly
    # every multi-hunk deletion looked like propagation.
    def anchor(e: Edit) -> str | None:
        if e.kind != "delete":
            return None
        for l in e.old_lines:
            name = (
                ident_of(l, FUNC_DECL_RE)
                or ident_of(l, TYPE_DECL_RE)
                or ident_of(l, re.compile(r"^\s*(?:pub\s+)?(?:let|const|var|static)\s+(\w{3,})"))
                or ident_of(l, re.compile(r"^\s*(\w{3,})\s*:"))
            )
            if name and len(name) >= 4:
                return name
        return None

    def follows(e: Edit, k: str) -> bool:
        if e.kind not in ("delete", "replace"):
            return False
        return re.search(rf"\b{re.escape(k)}\b", e.removed()) is not None

    return anchor_followers(fd, "delete_propagation", anchor, follows)


def s_enum_match_arm(fd: FileDiff) -> list[Episode]:
    """A variant is added; every exhaustive match must grow an arm."""

    def anchor(e: Edit) -> str | None:
        if e.kind != "insert":
            return None
        for l in e.new_lines:
            v = ident_of(l, ENUM_MEMBER_RE)
            if v and v[0].isupper():
                return v
        return None

    return anchor_followers(
        fd,
        "enum_match_arm",
        anchor,
        lambda e, k: e.kind == "insert" and any(MATCH_ARM_RE.match(l) and k in l for l in e.new_lines),
        min_followers=2,
    )


def s_error_conversion(fd: FileDiff) -> list[Episode]:
    def key(e: Edit) -> str | None:
        if e.kind != "replace":
            return None
        rem, add = e.removed(), e.added()
        for a, b in ERROR_IDIOMS:
            if a in rem and a not in add and b in add:
                return f"err:{a}->{b}"
        return None

    return repeated(fd, "error_conversion", key, min_sites=3)


def s_type_fanout(fd: FileDiff) -> list[Episode]:
    def key(e: Edit) -> str | None:
        if e.kind != "replace" or len(e.old_lines) != 1:
            return None
        a, b = e.old_lines[0], e.new_lines[0]
        ta = re.findall(r":\s*([\w<>:\[\], ]+)", a)
        tb = re.findall(r":\s*([\w<>:\[\], ]+)", b)
        if not ta or not tb or ta == tb:
            return None
        for x, y in zip(ta, tb):
            if x != y:
                return f"type:{x.strip()}->{y.strip()}"
        return None

    return repeated(fd, "type_fanout", key, min_sites=3)


def s_rename_casing(fd: FileDiff) -> list[Episode]:
    """The SAME rename at two casing styles — the gate DECLINES this
    (`casing_deferred`, NEXT_EDIT.md §4), so every episode here is a
    measured missed-fire rather than a model failure. That is the point:
    it sizes a known deferral instead of leaving it an opinion."""

    def key(e: Edit) -> str | None:
        if e.kind != "replace" or len(e.old_lines) != 1:
            return None
        a, b = e.old_lines[0], e.new_lines[0]
        p, s = strip_common(a, b)
        mid_a, mid_b = a[p : len(a) - s], b[p : len(b) - s]
        if not mid_a.strip() or not mid_b.strip():
            return None
        va, vb = casing_variants(mid_a), casing_variants(mid_b)
        if len(va) < 2 or len(vb) < 2:
            return None
        return f"rename:{sorted(va)[0]}->{sorted(vb)[0]}"

    eps = repeated(fd, "rename_casing", key, min_sites=3)
    # Keep only groups whose sites are NOT all the same literal rendering
    # — otherwise it is plain literal_fanout wearing a costume.
    out = []
    for ep in eps:
        rends = {e.old_lines[0][slice(*strip_common(e.old_lines[0], e.new_lines[0])[:1])] for e in ep.exemplars + ep.truth}
        if len({e.removed() for e in ep.exemplars + ep.truth}) > 1:
            out.append(ep)
    return out


def s_guard_insert(fd: FileDiff) -> list[Episode]:
    """A guard clause inserted at N sites. Matched over the inserted
    BLOCK, not one line: in braced languages the condition and its
    early exit are usually two or three lines, and a single-line-only
    matcher found zero across 400 commits."""

    def key(e: Edit) -> str | None:
        if e.kind != "insert" or not (1 <= len(e.new_lines) <= 4):
            return None
        block = " ".join(l.strip() for l in e.new_lines)
        if not re.match(r"^\s*(?:if|guard|unless)\b", block):
            return None
        if not re.search(r"\b(?:return|throw|raise|continue|break|bail|Err)\b", block):
            return None
        # Shape-normalise so different conditions on the same guard
        # SHAPE group together; the literal condition varies per site.
        return "guard:" + re.sub(r"[\w.\"']+", "X", block)[:48]

    return repeated(fd, "guard_insert", key, min_sites=3)


def s_doc_sync(fd: FileDiff) -> list[Episode]:
    """A signature changes and its doc comment must track it."""

    def anchor(e: Edit) -> str | None:
        if e.kind != "replace":
            return None
        for old, new in zip(e.old_lines, e.new_lines):
            a = ident_of(old, FUNC_DECL_RE)
            if a and a == ident_of(new, FUNC_DECL_RE) and old != new:
                return a
        return None

    return anchor_followers(
        fd,
        "doc_sync",
        anchor,
        lambda e, k: any(DOC_RE.match(l) for l in e.new_lines) and e.touches(k),
        min_followers=2,
    )


# `far_from_cursor` is a VIEW over other shapes, not a detector: any
# episode whose next truth edit is far from the last exemplar stresses
# needle anchoring rather than pattern induction. Tagged at build time.
FAR_LINES = 60

SHAPES: dict[str, dict] = {
    "literal_fanout": {"fn": s_literal_fanout, "gate": "rule-lane", "desc": "same literal rewrite at N sites"},
    "signature_fanout": {"fn": s_signature_fanout, "gate": "admits", "desc": "decl changes, call sites follow"},
    "param_insert": {"fn": s_param_insert, "gate": "admits", "desc": "argument added at N call sites"},
    "field_init": {"fn": s_field_init, "gate": "admits", "desc": "field added to N literals"},
    "import_addition": {"fn": s_import_addition, "gate": "unknown", "desc": "import lands, uses follow"},
    "delete_propagation": {"fn": s_delete_propagation, "gate": "unknown", "desc": "decl deleted, refs follow"},
    "enum_match_arm": {"fn": s_enum_match_arm, "gate": "unknown", "desc": "variant added, arms follow"},
    "error_conversion": {"fn": s_error_conversion, "gate": "unknown", "desc": "error idiom migrated"},
    "type_fanout": {"fn": s_type_fanout, "gate": "unknown", "desc": "type changed across annotations"},
    "rename_casing": {"fn": s_rename_casing, "gate": "DECLINES", "desc": "rename across casing styles"},
    "guard_insert": {"fn": s_guard_insert, "gate": "unknown", "desc": "same guard inserted at N sites"},
    "doc_sync": {"fn": s_doc_sync, "gate": "unknown", "desc": "signature change, doc follows"},
}


def detect_all(fd: FileDiff, only: Iterable[str] | None = None) -> list[Episode]:
    out: list[Episode] = []
    for name, spec in SHAPES.items():
        if only and name not in only:
            continue
        try:
            out.extend(spec["fn"](fd))
        except Exception:
            # A detector that throws on a pathological diff drops that
            # file, never the run: recall filters are best-effort by
            # design and one bad regex must not cost a corpus sweep.
            continue
    return out
