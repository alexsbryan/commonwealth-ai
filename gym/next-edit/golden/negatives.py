#!/usr/bin/env python3
"""Negative episodes — where the correct answer is SILENCE.

Spec: `sovereign/docs/specs/NEXT_EDIT_BAKEOFF.md` §4. Half the product is
restraint: `NEXT_EDIT.md` §1 fixes the failure cost as *a wrong edit
proposal*, and **no published next-edit benchmark scores silence at all**
— Sweep's, Continue's, Zed's and CUHK's are positives-only. A bank
without negatives can measure whether a model is useful and cannot
measure whether it is safe, which is the wrong half.

Every negative here is labelled by CONSTRUCTION, not by judgement: the
reason silence is correct is a mechanical property of the episode, and
each detector names it. Two are mined (`exhausted`, `divergent`), three
are built by transforming a real edit (`dissimilar`, `revert`,
`literal_trap`) — transformation keeps the code real while making the
label certain, which mining alone cannot do for cases that by definition
leave no trace in a commit.
"""

from __future__ import annotations

import re

from shapes import Edit, Episode, FileDiff, strip_common

COMMENT_RE = re.compile(r"^\s*(?://|#|\*|--|;)")


def _rule_key(e: Edit) -> tuple[str, str] | None:
    if e.kind != "replace" or len(e.old_lines) != 1 or len(e.new_lines) != 1:
        return None
    a, b = e.old_lines[0], e.new_lines[0]
    p, s = strip_common(a, b)
    mid_a, mid_b = a[p : len(a) - s], b[p : len(b) - s]
    if not (mid_a or mid_b):
        return None
    return (mid_a, mid_b)


def n_dissimilar(fd: FileDiff) -> list[Episode]:
    """Two consecutive edits with no shared transformation.

    Support is 1 for each rule, so nothing may fire — the single most
    common real state, since most editing is not a repeated pattern.
    """
    keyed = [(e, _rule_key(e)) for e in fd.edits]
    keyed = [(e, k) for e, k in keyed if k]
    out = []
    for (e1, k1), (e2, k2) in zip(keyed, keyed[1:]):
        if k1 == k2 or k1[0] == k2[0] or k1[1] == k2[1]:
            continue
        # Reject near-misses that a reasonable gate SHOULD consult on:
        # a shared prefix in the replacement is exactly `param_insert`.
        if k1[1][:4] and k1[1][:4] == k2[1][:4]:
            continue
        out.append(
            Episode("neg_dissimilar", [e1, e2], [], note="support 1 per rule")
        )
    return out[:6]


def n_exhausted(fd: FileDiff) -> list[Episode]:
    """Every site of a repeated pattern is already edited.

    The rule is real and well-supported; there is simply nothing left to
    propose. Firing here means inventing a site — or, for an
    insertion-shaped rule, re-applying it to a site that already has it.
    """
    groups: dict[tuple, list[Edit]] = {}
    for e in fd.edits:
        k = _rule_key(e)
        if k:
            groups.setdefault(k, []).append(e)
    out = []
    for (mid_a, mid_b), g in groups.items():
        if len(g) < 2 or len(mid_a) < 3:
            continue
        # Nothing may remain in the post-edit document, or it is not
        # exhausted and silence would be the WRONG answer.
        after = fd.new
        if re.search(rf"(?<!\w){re.escape(mid_a)}(?!\w)", after):
            continue
        g.sort(key=lambda e: e.old_start)
        out.append(Episode("neg_exhausted", g, [], note=f"no site left for {mid_a!r}"))
    return out[:4]


def n_divergent(fd: FileDiff) -> list[Episode]:
    """The same text edited at N sites, each to something DIFFERENT.

    There is no single transformation to induce, so any proposal is a
    guess about intent. The gate's `param_insert` shape requires the
    replacements to share a >=4-char prefix precisely to exclude this,
    so a fire here is a gate regression.
    """
    by_before: dict[str, list[tuple[Edit, str]]] = {}
    for e in fd.edits:
        k = _rule_key(e)
        if k and len(k[0]) >= 3:
            by_before.setdefault(k[0], []).append((e, k[1]))
    out = []
    for before, sites in by_before.items():
        if len(sites) < 2:
            continue
        afters = {a for _, a in sites}
        if len(afters) < 2:
            continue
        pre = min(afters, key=len)
        if all(a.startswith(pre[:4]) for a in afters) and len(pre) >= 4:
            continue  # a legitimate param_insert, not divergence
        out.append(
            Episode(
                "neg_divergent",
                [e for e, _ in sites[:2]],
                [],
                note=f"{before!r} -> {sorted(afters)[:3]}",
            )
        )
    return out[:4]


def n_revert(fd: FileDiff) -> list[Episode]:
    """The developer made an edit and immediately undid it.

    Constructed by pairing a real edit with its own inverse, because a
    revert leaves no trace in a commit and can therefore never be mined
    — yet it is one of the most common things a person does in an
    editor. Proposing anything here re-applies a change the user just
    rejected, which is the most user-hostile wrong edit available.
    """
    out = []
    for e in fd.edits:
        k = _rule_key(e)
        if not k or len(k[0]) < 3 or len(k[1]) < 3:
            continue
        inverse = Edit("replace", e.old_start, e.old_end, e.new_lines, e.old_lines)
        out.append(
            Episode("neg_revert", [e, inverse], [],
                    note=f"{k[0]!r}->{k[1]!r}->{k[0]!r}", doc_mode="old")
        )
        if len(out) >= 4:
            break
    return out


def n_literal_trap(fd: FileDiff) -> list[Episode]:
    """A real repeated edit whose ONLY remaining occurrence sits inside a
    comment or a string literal.

    The classic wrong edit: the induced rule matches textually, so a
    text-only engine will happily rewrite prose or a user-visible
    message. Silence is correct, and a fire is a genuine defect rather
    than a miss.
    """
    groups: dict[tuple, list[Edit]] = {}
    for e in fd.edits:
        k = _rule_key(e)
        if k:
            groups.setdefault(k, []).append(e)
    out = []
    for (mid_a, mid_b), g in groups.items():
        if len(g) < 2 or len(mid_a) < 4:
            continue
        lines = fd.new.split("\n")
        hits = [
            (i, l) for i, l in enumerate(lines)
            if re.search(rf"(?<!\w){re.escape(mid_a)}(?!\w)", l)
        ]
        if not hits or len(hits) > 3:
            continue
        # Every survivor must be inert: a comment line, or inside quotes.
        def inert(line: str) -> bool:
            if COMMENT_RE.match(line):
                return True
            for m in re.finditer(rf"(?<!\w){re.escape(mid_a)}(?!\w)", line):
                before = line[: m.start()]
                if before.count('"') % 2 == 1 or before.count("'") % 2 == 1:
                    return True
            return False

        if not all(inert(l) for _, l in hits):
            continue
        g.sort(key=lambda e: e.old_start)
        out.append(
            Episode("neg_literal_trap", g[:2], [],
                    note=f"{mid_a!r} survives only in comments/strings")
        )
    return out[:4]


NEGATIVES = {
    "neg_dissimilar": {"fn": n_dissimilar, "why": "support 1 — no repeated pattern"},
    "neg_exhausted": {"fn": n_exhausted, "why": "pattern complete — no site remains"},
    "neg_divergent": {"fn": n_divergent, "why": "same target, conflicting replacements"},
    "neg_revert": {"fn": n_revert, "why": "the edit was undone — never re-propose it"},
    "neg_literal_trap": {"fn": n_literal_trap, "why": "only comment/string sites remain"},
}


def detect_negatives(fd: FileDiff, only=None) -> list[Episode]:
    out: list[Episode] = []
    for name, spec in NEGATIVES.items():
        if only and name not in only:
            continue
        try:
            out.extend(spec["fn"](fd))
        except Exception:
            continue
    return out
