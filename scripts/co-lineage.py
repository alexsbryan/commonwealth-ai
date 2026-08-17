#!/usr/bin/env python3
"""co-lineage.py — the initiative rollup: which declared bars have no order.

Orders have accountability structure; initiatives did not. Sixteen orders ran
under NATIVE_GROUNDING.md, every one passed its own bar, and the initiative's
headline objective — parity at >=5x lower gated-turn latency — was never
carried by any order. One of the five mechanisms (H3) was never ordered, never
killed, never scoped. Both were found by hand, four months in.

THIS RENDERS BARS, NOT ORDERS, and that is the whole design. A parent pointer
alone makes the failure WORSE-looking-better: sixteen children, all closed,
gates green. Performed work was never the problem. The missing view is the
inverse — the bars with no order at all — and you cannot see a gap in a list
of things that happened.

  scripts/co-lineage.py coverage <initiative> [--as-of YYYY-MM-DD]
  scripts/co-lineage.py postmortem <initiative> [--as-of YYYY-MM-DD]
  scripts/co-lineage.py list
  scripts/co-lineage.py --self-test

Data:  quality/initiative-bars.toml   (the bars, as data — ARCH §6)
       .sovereign/features/*/order.md (the `serves:` frontmatter field)

TWO AXES, held apart on purpose:
  coverage — a property of ORDERS. Does any order's `serves:` name this bar?
  verdict  — a property of EVIDENCE. met / failed / could-not-judge /
             never-attempted (ARCH §18.2, four verdicts not two), plus the
             one performance state `met-floor`: measured, above the bar's
             floor, below its target. Yellow SHIPS and stays OPEN carrying a
             dated tuning debt — the answer to a bar guessed before any data
             that gets missed by two points and stalls the worker. Every
             route from yellow to a pass (closes_a_bar, a floor with no
             measured basis, a band on a target-only bar, a debt with no
             review date) is a load-time error, not a convention.
Their cross-product is where the interesting rows live: a COVERED bar whose
verdict is still never-attempted, with every covering order LANDED, is work
that closed green while the bar it existed to move never moved.

THE HEADLINE IS UNCOVERED BARS, and never-attempted counts as OPEN. The
precedent is co-mesh-drill.sh f-assemble fix #6 (2026-08-12): its headline
read "escalations needed = 1" while seven cases had never run. Same lie, one
scale down. See `_open_bars` — it is the only place that rule is implemented.

Exit codes: 0 rendered; 2 usage / unknown initiative; 3 the data file is
malformed or carries a value outside a closed set (never defaulted, #6).
"""
from __future__ import annotations

import argparse
import datetime as _dt
import os
import re
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BARS_TOML = REPO / "quality" / "initiative-bars.toml"
FEATURES = REPO / ".sovereign" / "features"

# The one place a date is parsed. A bad date is named, never coerced.
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


class DataError(Exception):
    """The declaration file said something outside its own closed set."""


# --------------------------------------------------------------------------
# data model
# --------------------------------------------------------------------------


@dataclass
class Transition:
    on: str
    to: str
    by: str
    note: str = ""
    review_by: str = ""   # required on met-floor: when the debt is re-read
    debt_key: str = ""    # required on met-floor: the backlog identity


@dataclass
class Bar:
    id: str
    one_line: str
    derives_from: str
    declared: str
    bar: str = ""
    kill: str = ""
    evidence_note: str = ""
    floor: str = ""        # the red line — data-backed or absent
    floor_basis: str = ""  # where the floor's number came from
    target: str = ""       # the aspiration — may be invented, operator-only to move
    lane: str = ""
    noise_band: str = ""   # from RUNBOOK §6; absent means unknown, never zero
    transitions: list[Transition] = field(default_factory=list)

    @property
    def banded(self) -> bool:
        """Does this bar have a yellow band at all?

        A bar with no floor is target-only: red/green, and a near-miss on it
        is a genuine escalation rather than a debt. Structural zeros are
        SUPPOSED to be this shape.
        """
        return bool(self.floor)


@dataclass
class Initiative:
    id: str
    title: str
    spec: str
    declared: str
    status: str
    notes: str = ""
    bars: list[Bar] = field(default_factory=list)


@dataclass
class Order:
    id: str
    path: Path
    status: str
    drafted: str
    approved: str
    serves_raw: str | None  # None = field absent
    serves_initiative: str | None
    serves_bars: list[str]

    @property
    def attributed(self) -> bool:
        return self.serves_initiative is not None


@dataclass
class Vocabulary:
    verdict: list[str]
    transition: list[str]
    closes_a_bar: list[str]
    unattributed: str

    @property
    def verdict_bearing(self) -> list[str]:
        """Transition values that SET a bar's verdict.

        Everything else (declared / deferred / descoped / re-entered) records
        scope movement without claiming an outcome — which is exactly the
        H0-latency shape this tool exists to surface.
        """
        return [t for t in self.transition if t in self.verdict]


NEVER = "never-attempted"

# The performance state, not a fifth epistemic verdict: measured, above the
# floor, below the target. Verdict-bearing (it says what the evidence
# showed) but absent from closes_a_bar (it leaves the bar OPEN). Both halves
# are enforced below rather than remembered.
YELLOW = "met-floor"


# --------------------------------------------------------------------------
# loading — a value outside a closed set is an error, never a default (#6)
# --------------------------------------------------------------------------


def _check_date(value: str, where: str) -> str:
    if not DATE_RE.match(value or ""):
        raise DataError(f"{where}: {value!r} is not a YYYY-MM-DD date")
    return value


def load_declaration(path: Path = BARS_TOML) -> tuple[Vocabulary, list[Initiative], dict]:
    try:
        raw = tomllib.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise DataError(f"{path} not found — nothing declares any bars")
    except tomllib.TOMLDecodeError as exc:
        raise DataError(f"{path} is not valid TOML: {exc}")

    voc_raw = raw.get("vocabulary") or {}
    fmt = raw.get("format") or {}
    for key in ("verdict", "transition", "closes_a_bar"):
        if key not in voc_raw:
            raise DataError(f"{path}: [vocabulary] is missing {key!r}")
    voc = Vocabulary(
        verdict=list(voc_raw["verdict"]),
        transition=list(voc_raw["transition"]),
        closes_a_bar=list(voc_raw["closes_a_bar"]),
        unattributed=fmt.get("unattributed", "(unattributed)"),
    )
    if NEVER not in voc.verdict:
        raise DataError(
            f"{path}: [vocabulary] verdict does not contain {NEVER!r} — a verdict "
            "set that cannot express never-attempted reproduces the failure this "
            "file was minted for"
        )
    if YELLOW in voc.closes_a_bar:
        raise DataError(
            f"{path}: [vocabulary] closes_a_bar contains {YELLOW!r} — yellow that "
            "closes a bar is a pass with better manners. A bar above its floor and "
            "below its target stays OPEN, carrying a debt; only `met` and "
            "`descoped` close one."
        )
    if (YELLOW in voc.transition) != (YELLOW in voc.verdict):
        raise DataError(
            f"{path}: [vocabulary] declares {YELLOW!r} in only one of transition/"
            "verdict. It must be in BOTH (a transition that records it, a verdict "
            "it sets) or NEITHER — in one list alone it is silently non-bearing, "
            "which reads as never-attempted on a bar that was measured."
        )

    initiatives: list[Initiative] = []
    for i_raw in raw.get("initiative", []):
        init = Initiative(
            id=i_raw["id"],
            title=i_raw.get("title", i_raw["id"]),
            spec=i_raw.get("spec", ""),
            declared=_check_date(i_raw.get("declared", ""), f"initiative {i_raw['id']} declared"),
            status=i_raw.get("status", "active"),
            notes=i_raw.get("notes", ""),
        )
        for b_raw in i_raw.get("bar", []):
            bar = Bar(
                id=b_raw["id"],
                one_line=b_raw.get("one_line", ""),
                derives_from=b_raw.get("derives_from", ""),
                declared=_check_date(b_raw.get("declared", ""), f"bar {b_raw['id']} declared"),
                bar=b_raw.get("bar", ""),
                kill=b_raw.get("kill", ""),
                evidence_note=b_raw.get("evidence_note", ""),
                floor=b_raw.get("floor", ""),
                floor_basis=b_raw.get("floor_basis", ""),
                target=b_raw.get("target", ""),
                lane=b_raw.get("lane", ""),
                noise_band=b_raw.get("noise_band", ""),
            )
            # The honesty asymmetry, enforced: a target may be invented, a
            # floor may not. A floor with no basis is a second guess wearing
            # the word "floor", and it would make yellow a rubber stamp.
            if bar.floor and not bar.floor_basis:
                raise DataError(
                    f"bar {bar.id}: `floor` is declared with no `floor_basis`. Name "
                    "where the number came from — a committed baseline path, a "
                    "measurement with its date, or \"structural\" — or drop the "
                    "floor and let the bar be target-only."
                )
            for t_raw in b_raw.get("transition", []):
                to = t_raw["to"]
                if to not in voc.transition:
                    raise DataError(
                        f"bar {bar.id}: transition to {to!r} is not in [vocabulary] "
                        f"transition {voc.transition}"
                    )
                if to == YELLOW:
                    # No band, no yellow. A target-only bar (structural zero,
                    # or one whose floor was never measured) has no room
                    # between floor and target to sit in, and inventing one
                    # here is exactly the silent substitution (#6).
                    if not bar.banded:
                        raise DataError(
                            f"bar {bar.id}: transition to {YELLOW!r} on a bar with no "
                            "`floor`. Target-only bars are red/green by construction — "
                            "measure a floor and declare it, or record the honest "
                            "`failed`/`could-not-judge` and escalate."
                        )
                    if not t_raw.get("review_by"):
                        raise DataError(
                            f"bar {bar.id}: {YELLOW!r} transition on {t_raw['on']} has no "
                            "`review_by`. A debt with no date is how a band becomes the "
                            "ceiling; the DEFAULTS_LEDGER row pattern applies here."
                        )
                    _check_date(t_raw["review_by"], f"bar {bar.id} transition review_by")
                    if not t_raw.get("debt_key"):
                        raise DataError(
                            f"bar {bar.id}: {YELLOW!r} transition on {t_raw['on']} has no "
                            "`debt_key`. Yellow ships a tuning-debt backlog item keyed by "
                            "essence (#7.5) — conventionally the bar id, so repeated "
                            "yellows update one item instead of filing thirty."
                        )
                bar.transitions.append(
                    Transition(
                        on=_check_date(t_raw["on"], f"bar {bar.id} transition"),
                        to=to,
                        by=t_raw.get("by", ""),
                        note=t_raw.get("note", ""),
                        review_by=t_raw.get("review_by", ""),
                        debt_key=t_raw.get("debt_key", ""),
                    )
                )
            # Stable within a date: file order breaks ties, so two same-day
            # transitions read in the order the author recorded them.
            bar.transitions.sort(key=lambda t: t.on)
            init.bars.append(bar)
        initiatives.append(init)
    return voc, initiatives, raw


FRONT_RE = re.compile(r"\A---\n(.*?)\n---", re.S)


def parse_order(path: Path) -> Order | None:
    text = path.read_text(encoding="utf-8", errors="replace")
    m = FRONT_RE.match(text)
    if not m:
        return None
    fields: dict[str, str] = {}
    for line in m.group(1).splitlines():
        if line.lstrip().startswith("#"):
            continue
        if ":" not in line:
            continue
        k, _, v = line.partition(":")
        fields[k.strip()] = v.strip()
    serves_raw = fields.get("serves")
    return Order(
        id=fields.get("id", path.parent.name),
        path=path,
        status=fields.get("status", "unknown"),
        drafted=fields.get("drafted", ""),
        approved=fields.get("approved", ""),
        serves_raw=serves_raw,
        serves_initiative=_serves_initiative(serves_raw),
        serves_bars=_serves_bars(serves_raw),
    )


def _serves_tokens(serves_raw: str | None) -> list[str]:
    if not serves_raw:
        return []
    cleaned = serves_raw.split("#", 1)[0].strip()
    if not cleaned or cleaned.startswith("("):
        return []  # (unattributed) and friends — a legal, visible state
    return cleaned.replace(",", " ").split()


def _serves_initiative(serves_raw: str | None) -> str | None:
    toks = _serves_tokens(serves_raw)
    return toks[0] if toks else None


def _serves_bars(serves_raw: str | None) -> list[str]:
    return _serves_tokens(serves_raw)[1:]


def load_orders(features: Path = FEATURES) -> list[Order]:
    out = []
    if not features.is_dir():
        return out
    for p in sorted(features.glob("*/order.md")):
        o = parse_order(p)
        if o is not None:
            out.append(o)
    return out


# --------------------------------------------------------------------------
# the deciders — one implementation each (#8)
# --------------------------------------------------------------------------


def transitions_asof(bar: Bar, as_of: str | None) -> list[Transition]:
    return [t for t in bar.transitions if as_of is None or t.on <= as_of]


def verdict_of(bar: Bar, voc: Vocabulary, as_of: str | None = None) -> str:
    """The bar's verdict = the last verdict-bearing transition, else never-attempted.

    The ONLY implementation of this rule (#8). `deferred` is deliberately NOT
    verdict-bearing: a bar moved out of a plan has not been judged, and
    recording it as anything but never-attempted is the exact substitution
    that hid H0's latency clause for four months.
    """
    bearing = [t for t in transitions_asof(bar, as_of) if t.to in voc.verdict_bearing]
    return bearing[-1].to if bearing else NEVER


def descoped_by(bar: Bar, as_of: str | None = None) -> Transition | None:
    for t in reversed(transitions_asof(bar, as_of)):
        if t.to == "descoped":
            return t
    return None


def deferrals(bar: Bar, as_of: str | None = None) -> list[Transition]:
    """Deferrals not followed by a re-entry — the silent-re-scope detector."""
    out: list[Transition] = []
    for t in transitions_asof(bar, as_of):
        if t.to == "deferred":
            out.append(t)
        elif t.to == "re-entered":
            out.clear()
    return out


def yellow_debt(bar: Bar, voc: Vocabulary, as_of: str | None = None) -> Transition | None:
    """The met-floor transition that is CURRENTLY standing, if any.

    Only when the bar's verdict is still yellow — a bar later tuned to `met`
    or knocked to `failed` has no standing debt, and the historical yellow
    stays in the transition list where the post-mortem reads it.
    """
    if verdict_of(bar, voc, as_of) != YELLOW:
        return None
    for t in reversed(transitions_asof(bar, as_of)):
        if t.to == YELLOW:
            return t
    return None


def overdue_yellow(bar: Bar, voc: Vocabulary, as_of: str | None = None,
                   today: str | None = None) -> Transition | None:
    """A standing yellow whose review_by has passed — the escalation trigger.

    Without this, a band quietly becomes the ceiling: the bar reads "measured,
    above floor" forever and nobody is ever surprised by it again. The
    loader guarantees review_by exists, so this is a comparison, not a search.
    """
    debt = yellow_debt(bar, voc, as_of)
    if debt is None:
        return None
    now = today or as_of or _dt.date.today().isoformat()
    return debt if debt.review_by < now else None


def covering_orders(bar: Bar, init: Initiative, orders: list[Order], as_of: str | None) -> list[Order]:
    return [
        o
        for o in orders
        if o.serves_initiative == init.id
        and bar.id in o.serves_bars
        and (as_of is None or not o.drafted or o.drafted <= as_of)
    ]


def initiative_orders(init: Initiative, orders: list[Order], as_of: str | None) -> list[Order]:
    return [
        o
        for o in orders
        if o.serves_initiative == init.id and (as_of is None or not o.drafted or o.drafted <= as_of)
    ]


def is_open(bar: Bar, voc: Vocabulary, as_of: str | None = None) -> bool:
    """OPEN unless met or deliberately descoped.

    never-attempted counts as OPEN. This is co-mesh-drill.sh f-assemble fix #6
    at program altitude: a headline that counts only things that RAN reports a
    clean number for a program that ran nothing. `deferred` also stays open —
    a bar postponed by a planning document is not a bar answered.
    """
    if verdict_of(bar, voc, as_of) in voc.closes_a_bar:
        return False                       # `met` closes it on evidence
    return descoped_by(bar, as_of) is None  # `descoped` closes it by decision


def _open_bars(init: Initiative, voc: Vocabulary, as_of: str | None) -> list[Bar]:
    return [b for b in init.bars if is_open(b, voc, as_of)]


def landed_but_unmoved(bar: Bar, init: Initiative, orders: list[Order], voc: Vocabulary,
                       as_of: str | None) -> list[Order]:
    """Covering orders that LANDED while the bar they claimed never moved.

    The single most important row in a post-mortem, and invisible to either
    axis alone: coverage says the bar was claimed, verdict says nothing was
    ever recorded against it, order status says the work closed green.
    """
    if verdict_of(bar, voc, as_of) != NEVER:
        return []
    return [o for o in covering_orders(bar, init, orders, as_of) if o.status == "landed"]


# --------------------------------------------------------------------------
# honesty pass — what this renderer could NOT map, stated rather than dropped
# --------------------------------------------------------------------------


def unmappable(init: Initiative, orders: list[Order], as_of: str | None) -> list[str]:
    problems: list[str] = []
    known = {b.id for b in init.bars}
    for o in initiative_orders(init, orders, as_of):
        for b in o.serves_bars:
            if b not in known:
                problems.append(
                    f"order {o.id} serves bar {b!r}, which {BARS_TOML.name} does not declare "
                    f"for initiative {init.id}"
                )
    if init.spec and not (REPO / init.spec).exists():
        problems.append(
            f"initiative spec {init.spec} does not resolve in this tree — the bars below "
            "cite sections of a document the repo cannot show you"
        )
    for b in init.bars:
        if not b.derives_from:
            problems.append(f"bar {b.id} declares no `derives_from` — it cites no spec section")
        if b.banded and not b.noise_band:
            # Step 0 of the near-miss protocol is "is the delta inside the
            # lane's band?" (RUNBOOK §6). Without the band it is a judgement
            # call every time, which is where "miss by a couple percent" turns
            # into a stall instead of a could-not-judge.
            problems.append(
                f"bar {b.id} has a floor/target band but no `noise_band` — the near-miss "
                "protocol cannot mechanically tell a miss from weather"
            )
    return problems


# --------------------------------------------------------------------------
# rendering
# --------------------------------------------------------------------------


def _wrap(text: str, width: int, indent: str) -> list[str]:
    import textwrap

    flat = " ".join((text or "").split())
    if not flat:
        return []
    return textwrap.wrap(flat, width=width, initial_indent=indent, subsequent_indent=indent)


def _cause_line(bar: Bar, voc: Vocabulary, as_of: str | None) -> str:
    dsc = descoped_by(bar, as_of)
    if dsc:
        return f"descoped {dsc.on} by {dsc.by}"
    defs = deferrals(bar, as_of)
    if defs:
        d = defs[-1]
        return f"deferred {d.on} by {d.by} — never re-entered"
    debt = yellow_debt(bar, voc, as_of)
    if debt:
        due = "OVERDUE" if overdue_yellow(bar, voc, as_of) else f"review-by {debt.review_by}"
        return f"yellow since {debt.on} — {due}, debt {debt.debt_key}"
    # Count what HAPPENED, not how many rows there are. A bar whose only
    # transition IS its verdict (no separate `declared` row — legal, and the
    # common shape in this file) read "no transition since declared" while
    # carrying a recorded failure: the exact quiet lie the file exists to
    # prevent, one scale down. Fixed 2026-08-16.
    after = [t for t in transitions_asof(bar, as_of) if t.to != "declared"]
    if not after:
        return f"no transition since declared {bar.declared}"
    return f"last: {after[-1].to} {after[-1].on} by {after[-1].by}"


def render_coverage(init: Initiative, voc: Vocabulary, orders: list[Order],
                    as_of: str | None, out=sys.stdout) -> None:
    p = lambda s="": print(s, file=out)  # noqa: E731
    mine = initiative_orders(init, orders, as_of)
    asof_label = as_of or _dt.date.today().isoformat()

    p(f"initiative: {init.id} — {init.title}")
    p(f"spec:       {init.spec or '(none declared)'}")
    p(f"declared:   {init.declared}    as-of: {asof_label}    "
      f"bars: {len(init.bars)}    orders serving it: {len(mine)}")
    p()

    uncovered = [b for b in init.bars if not covering_orders(b, init, orders, as_of)]
    open_bars = _open_bars(init, voc, as_of)
    p(f"UNCOVERED BARS = {len(uncovered)} of {len(init.bars)}"
      + (f"   ({', '.join(b.id for b in uncovered)})" if uncovered else ""))
    p("   ^ the headline. A bar no order names cannot have been met by accident.")
    p(f"OPEN BARS = {len(open_bars)} of {len(init.bars)}   "
      "(never-attempted counts as open — f-assemble fix #6)")
    yellow = [b for b in init.bars if yellow_debt(b, voc, as_of)]
    if yellow:
        od = [b for b in yellow if overdue_yellow(b, voc, as_of)]
        p(f"YELLOW (above floor, below target) = {len(yellow)}   "
          f"({', '.join(b.id for b in yellow)})"
          + (f"   >> OVERDUE: {', '.join(b.id for b in od)}" if od else ""))
        p("   ^ shipped on a debt, not a pass. Still OPEN. Only the operator "
          "moves a target (§18.6).")
    p()

    p(f"{'bar':<24} {'coverage':<12} {'verdict':<17} cause / orders")
    p("-" * 100)
    for b in init.bars:
        cov = covering_orders(b, init, orders, as_of)
        v = verdict_of(b, voc, as_of)
        cov_label = f"covered({len(cov)})" if cov else "UNCOVERED"
        p(f"{b.id:<24} {cov_label:<12} {v:<17} {_cause_line(b, voc, as_of)}")
        for line in _wrap(b.one_line, 76, " " * 24):
            p(line)
        if cov:
            p(" " * 24 + "orders: " + ", ".join(f"{o.id}({o.status})" for o in cov))
        stuck = landed_but_unmoved(b, init, orders, voc, as_of)
        if stuck:
            p(" " * 24 + ">> LANDED-BUT-UNMOVED: "
              + ", ".join(o.id for o in stuck)
              + " closed `landed` while this bar recorded no evidence event")
        if b.evidence_note:
            for line in _wrap(b.evidence_note, 74, " " * 26):
                p(line)
    p()

    counts = {k: 0 for k in voc.verdict}
    for b in init.bars:
        counts[verdict_of(b, voc, as_of)] += 1
    p("verdict summary: " + "  ".join(f"{k} {counts[k]}" for k in voc.verdict))
    p("Verdicts are four, not two (ARCH §18.2). never-attempted means NO evidence")
    p("event was ever recorded against the bar — not that it passed quietly.")
    p("met-floor is the fifth column and the odd one out: a performance state,")
    p("measured and above the floor, that leaves the bar OPEN and owing a tune.")

    _render_honesty(init, orders, as_of, p)


def _render_honesty(init: Initiative, orders: list[Order], as_of: str | None, p) -> None:
    p()
    p("what this view could not map (absence stated, never defaulted — #6):")
    problems = unmappable(init, orders, as_of)
    unattributed = [o for o in orders
                    if not o.attributed and (as_of is None or not o.drafted or o.drafted <= as_of)]
    if not problems:
        p("  - every declared bar maps; every `serves:` bar id is declared")
    for x in problems:
        p(f"  - {x}")
    p(f"  - {len(unattributed)} order(s) in this tree carry no `serves:` at all "
      "(unattributed — a legal, visible state, not a defect to hide)")


def render_postmortem(init: Initiative, voc: Vocabulary, orders: list[Order],
                      as_of: str | None, out=sys.stdout) -> None:
    p = lambda s="": print(s, file=out)  # noqa: E731
    mine = initiative_orders(init, orders, as_of)
    asof_label = as_of or _dt.date.today().isoformat()

    p("=" * 100)
    p(f"POST-MORTEM — {init.id} — {init.title}")
    p(f"as of {asof_label}   (status: {init.status})")
    p("=" * 100)
    p()

    # ---- per initiative ---------------------------------------------------
    uncovered = [b for b in init.bars if not covering_orders(b, init, orders, as_of)]
    met = [b for b in init.bars if verdict_of(b, voc, as_of) == "met"]
    dates = sorted(o.drafted for o in mine if o.drafted)
    span_days = ""
    if dates:
        try:
            span_days = (
                f"   ({(_dt.date.fromisoformat(asof_label) - _dt.date.fromisoformat(init.declared)).days} days)"
            )
        except ValueError:
            span_days = ""
    p("## the initiative")
    p(f"  bars declared        {len(init.bars)}")
    p(f"  bars covered         {len(init.bars) - len(uncovered)}")
    yellow = [b for b in init.bars if yellow_debt(b, voc, as_of)]
    overdue = [b for b in yellow if overdue_yellow(b, voc, as_of)]
    p(f"  bars met             {len(met)}"
      + (f"   ({', '.join(b.id for b in met)})" if met else ""))
    p(f"  bars YELLOW          {len(yellow)}"
      + (f"   ({', '.join(b.id for b in yellow)})" if yellow else "")
      + (f"   OVERDUE: {', '.join(b.id for b in overdue)}" if overdue else ""))
    p(f"  bars UNCOVERED       {len(uncovered)}"
      + (f"   ({', '.join(b.id for b in uncovered)})" if uncovered else ""))
    p(f"  orders serving it    {len(mine)}"
      + (f"   first {dates[0]}, last {dates[-1]}" if dates else ""))
    p(f"  elapsed              {init.declared} -> {asof_label}{span_days}")
    if init.notes:
        for line in _wrap(init.notes, 92, "  note: "):
            p(line)
    p()

    # ---- scope drift ------------------------------------------------------
    p("## scope drift — the bar set at start vs at the end")
    added = [b for b in init.bars if b.declared > init.declared and
             (as_of is None or b.declared <= as_of)]
    dropped = [(b, deferrals(b, as_of)[-1]) for b in init.bars if deferrals(b, as_of)]
    killed = [(b, descoped_by(b, as_of)) for b in init.bars if descoped_by(b, as_of)]
    p(f"  declared at start    {len([b for b in init.bars if b.declared <= init.declared])}")
    p(f"  added mid-flight     {len(added)}"
      + (f"   ({', '.join(b.id for b in added)})" if added else ""))
    p(f"  deferred, not judged {len(dropped)}")
    for b, t in dropped:
        p(f"      {b.id:<22} deferred {t.on} by {t.by}")
        for line in _wrap(t.note, 84, " " * 10):
            p(line)
    p(f"  descoped by decision {len(killed)}")
    for b, t in killed:
        p(f"      {b.id:<22} descoped {t.on} by {t.by}")
    p()

    # ---- per bar ----------------------------------------------------------
    p("## per bar — declared when, every transition, the artifact that caused it")
    for b in init.bars:
        v = verdict_of(b, voc, as_of)
        cov = covering_orders(b, init, orders, as_of)
        p()
        p(f"  {b.id}  [{v}]  {'covered by ' + ', '.join(o.id for o in cov) if cov else 'NO ORDER'}")
        for line in _wrap(b.one_line, 88, "      "):
            p(line)
        for line in _wrap("derives from: " + b.derives_from, 88, "      "):
            p(line)
        if b.bar:
            for line in _wrap("bar: " + b.bar, 88, "      "):
                p(line)
        if b.floor:
            for line in _wrap(f"floor: {b.floor}   [basis: {b.floor_basis}]", 88, "      "):
                p(line)
        elif b.target:
            p("      floor: (none) — target-only, red/green by construction")
        if b.target:
            for line in _wrap("target: " + b.target, 88, "      "):
                p(line)
        if b.lane or b.noise_band:
            p(f"      lane: {b.lane or '(none)'}   noise band: {b.noise_band or '(unknown)'}")
        if b.kill:
            for line in _wrap("kill: " + b.kill, 88, "      "):
                p(line)
        p(f"      declared {b.declared}")
        ts = transitions_asof(b, as_of)
        if not [t for t in ts if t.to != "declared"]:
            p("      transitions: none after `declared` — the bar was never revisited")
        for t in ts:
            p(f"        {t.on}  {t.to:<16} <- {t.by}")
            if t.to == YELLOW:
                p(f"          review-by {t.review_by}   debt {t.debt_key}")
            for line in _wrap(t.note, 80, " " * 12):
                p(line)
        debt = yellow_debt(b, voc, as_of)
        if debt:
            od = overdue_yellow(b, voc, as_of)
            p(f"      >> {'OVERDUE YELLOW' if od else 'YELLOW'}: above floor, below target since"
              f" {debt.on}; review-by {debt.review_by}, debt filed as {debt.debt_key}."
              + (" The review date has passed — this is the escalation, not a state."
                 if od else " The bar stays OPEN; only the operator moves a target (§18.6)."))
        defs = deferrals(b, as_of)
        if defs and v == NEVER:
            p(f"      >> DEFERRED, NOT FAILED: no order ever reported a miss against this bar."
              f" It left the plan on {defs[-1].on} via {defs[-1].by} and never re-entered.")
        if v == NEVER and not cov and not [t for t in ts if t.to != "declared"]:
            p("      >> NEVER ORDERED: declared with the spec, then nothing. Not killed,"
              " not descoped, not deferred — no artifact ever touched it.")
        stuck = landed_but_unmoved(b, init, orders, voc, as_of)
        if stuck:
            p("      >> LANDED-BUT-UNMOVED: " + ", ".join(o.id for o in stuck)
              + " closed `landed` claiming this bar; no evidence event was recorded against it.")
        if b.evidence_note:
            for line in _wrap("note: " + b.evidence_note, 84, "      "):
                p(line)
    p()

    # ---- per order --------------------------------------------------------
    p("## per order — what it claimed to move, how it landed, whether the bar moved")
    p(f"  {'order':<38} {'status':<11} {'drafted':<11} bars claimed -> verdict now")
    p("  " + "-" * 96)
    for o in sorted(mine, key=lambda x: (x.drafted, x.id)):
        if o.serves_bars:
            claims = ", ".join(
                f"{bid}={verdict_of(_bar(init, bid), voc, as_of) if _bar(init, bid) else 'UNKNOWN-BAR'}"
                for bid in o.serves_bars
            )
        else:
            claims = "(initiative only — no bar claimed)"
        p(f"  {o.id:<38} {o.status:<11} {o.drafted:<11} {claims}")
        unmoved = [bid for bid in o.serves_bars
                   if _bar(init, bid) and verdict_of(_bar(init, bid), voc, as_of) == NEVER]
        if o.status == "landed" and unmoved:
            p(f"  {'':<38} >> landed, but {', '.join(unmoved)} recorded no evidence event")
    p()

    _render_honesty(init, orders, as_of, p)


def _bar(init: Initiative, bar_id: str) -> Bar | None:
    for b in init.bars:
        if b.id == bar_id:
            return b
    return None


# --------------------------------------------------------------------------
# self-test — a gate you have not watched fail is not a gate (#5)
# --------------------------------------------------------------------------

SELF_TEST_TOML = """
version = "t"
[vocabulary]
verdict = ["met", "met-floor", "failed", "could-not-judge", "never-attempted"]
transition = ["declared", "deferred", "descoped", "re-entered", "met", "met-floor", "failed", "could-not-judge"]
closes_a_bar = ["met", "descoped"]
[format]
unattributed = "(unattributed)"
[[initiative]]
id = "t"
title = "t"
spec = "quality/initiative-bars.toml"
declared = "2026-01-01"
status = "active"
[[initiative.bar]]
id = "B-met"
one_line = "x"
derives_from = "spec §1"
declared = "2026-01-01"
[[initiative.bar.transition]]
on = "2026-01-01"
to = "declared"
by = "spec"
[[initiative.bar.transition]]
on = "2026-01-05"
to = "met"
by = "order-a"
[[initiative.bar]]
id = "B-deferred"
one_line = "x"
derives_from = "spec §2"
declared = "2026-01-01"
[[initiative.bar.transition]]
on = "2026-01-01"
to = "declared"
by = "spec"
[[initiative.bar.transition]]
on = "2026-01-03"
to = "deferred"
by = "plan.md §5"
[[initiative.bar]]
id = "B-never"
one_line = "x"
derives_from = "spec §3"
declared = "2026-01-01"
[[initiative.bar.transition]]
on = "2026-01-01"
to = "declared"
by = "spec"
[[initiative.bar]]
id = "B-descoped"
one_line = "x"
derives_from = "spec §4"
declared = "2026-01-01"
[[initiative.bar.transition]]
on = "2026-01-01"
to = "declared"
by = "spec"
[[initiative.bar.transition]]
on = "2026-01-04"
to = "descoped"
by = "operator"
[[initiative.bar]]
id = "B-yellow"
one_line = "x"
derives_from = "spec §5"
declared = "2026-01-01"
floor = ">= 0.70 answer-equiv"
floor_basis = "incumbent path measured 0.70, quality/baselines/t.json 2026-01-01"
target = ">= 0.85 answer-equiv"
lane = "synth-prod"
noise_band = "+/-0.04-0.06 run-to-run (RUNBOOK §6)"
[[initiative.bar.transition]]
on = "2026-01-01"
to = "declared"
by = "spec"
[[initiative.bar.transition]]
on = "2026-01-06"
to = "met-floor"
by = "order-b"
review_by = "2026-02-01"
debt_key = "B-yellow"
note = "0.81 on holdout after a bounded 6-iteration tune; curve committed"
[[initiative.bar]]
id = "B-yellow-overdue"
one_line = "x"
derives_from = "spec §6"
declared = "2026-01-01"
floor = ">= 40ms"
floor_basis = "structural"
target = ">= 20ms"
lane = "latency-prod"
noise_band = "+/-3ms"
[[initiative.bar.transition]]
on = "2026-01-01"
to = "declared"
by = "spec"
[[initiative.bar.transition]]
on = "2026-01-02"
to = "met-floor"
by = "order-c"
review_by = "2026-01-05"
debt_key = "B-yellow-overdue"
"""

BAD_VERDICT_TOML = SELF_TEST_TOML.replace(
    'verdict = ["met", "met-floor", "failed", "could-not-judge", "never-attempted"]',
    'verdict = ["passed", "failed"]')
BAD_TRANSITION_TOML = SELF_TEST_TOML.replace('to = "deferred"', 'to = "postponed"')

# --- the yellow negative cases ------------------------------------------
# Each one is a way yellow could quietly become a pass. All five must be a
# named error, never a default (#6) — that is what makes the band safe to
# hand a worker.
YELLOW_CLOSES_TOML = SELF_TEST_TOML.replace(
    'closes_a_bar = ["met", "descoped"]', 'closes_a_bar = ["met", "met-floor", "descoped"]')
YELLOW_HALF_TOML = SELF_TEST_TOML.replace(
    'verdict = ["met", "met-floor", "failed", "could-not-judge", "never-attempted"]',
    'verdict = ["met", "failed", "could-not-judge", "never-attempted"]')
FLOOR_NO_BASIS_TOML = SELF_TEST_TOML.replace(
    'floor_basis = "incumbent path measured 0.70, quality/baselines/t.json 2026-01-01"\n', "")
YELLOW_NO_FLOOR_TOML = SELF_TEST_TOML.replace(
    'floor = ">= 0.70 answer-equiv"\n', "").replace(
    'floor_basis = "incumbent path measured 0.70, quality/baselines/t.json 2026-01-01"\n', "")
YELLOW_NO_REVIEW_TOML = SELF_TEST_TOML.replace('review_by = "2026-02-01"\n', "")
YELLOW_NO_DEBT_TOML = SELF_TEST_TOML.replace('debt_key = "B-yellow"\n', "")


def _fake_order(oid: str, status: str, serves: str, drafted="2026-01-02") -> Order:
    return Order(id=oid, path=Path(oid), status=status, drafted=drafted, approved="x",
                 serves_raw=serves, serves_initiative=_serves_initiative(serves),
                 serves_bars=_serves_bars(serves))


def self_test() -> int:
    import io
    import tempfile

    failures: list[str] = []

    def check(name: str, cond: bool, detail: str = "") -> None:
        if cond:
            print(f"  pass  {name}")
        else:
            print(f"  FAIL  {name}  {detail}")
            failures.append(name)

    def load(text: str):
        with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as fh:
            fh.write(text)
            path = Path(fh.name)
        try:
            return load_declaration(path)
        finally:
            os.unlink(path)

    print("co-lineage --self-test")
    voc, inits, _ = load(SELF_TEST_TOML)
    init = inits[0]
    bars = {b.id: b for b in init.bars}

    # --- the four verdicts, each reachable --------------------------------
    check("verdict(B-met) == met", verdict_of(bars["B-met"], voc) == "met")
    check("verdict(B-never) == never-attempted", verdict_of(bars["B-never"], voc) == NEVER)
    check("a deferred bar is NOT judged — verdict stays never-attempted",
          verdict_of(bars["B-deferred"], voc) == NEVER,
          "deferral must not read as a verdict; that is how H0-latency hid")

    # --- yellow: a debt, not a pass ---------------------------------------
    check("verdict(B-yellow) == met-floor", verdict_of(bars["B-yellow"], voc) == YELLOW)
    check("a banded bar knows it is banded", bars["B-yellow"].banded)
    check("a bar with no floor is target-only", not bars["B-never"].banded)
    check("yellow carries its standing debt",
          (yellow_debt(bars["B-yellow"], voc) or Transition("", "", "")).debt_key == "B-yellow")
    check("yellow before its review-by is not overdue",
          overdue_yellow(bars["B-yellow"], voc, today="2026-01-10") is None)
    check("yellow past its review-by IS overdue — the escalation trigger",
          overdue_yellow(bars["B-yellow-overdue"], voc, today="2026-01-10") is not None)
    check("a bar tuned to met has no standing debt",
          yellow_debt(bars["B-met"], voc) is None)

    # --- the f-assemble rule: never-attempted counts as OPEN ---------------
    open_ids = {b.id for b in _open_bars(init, voc, None)}
    check("never-attempted counts as OPEN", "B-never" in open_ids)
    check("deferred counts as OPEN", "B-deferred" in open_ids)
    check("met closes a bar", "B-met" not in open_ids)
    check("descoped closes a bar (by decision, with a cause)", "B-descoped" not in open_ids)
    check("YELLOW COUNTS AS OPEN — shipped on a debt, not a pass",
          "B-yellow" in open_ids and "B-yellow-overdue" in open_ids,
          "a met-floor bar that closes is the whole failure this guard exists for")

    # --- the cause line reports what happened, not row count --------------
    # B-yellow-overdue's `declared` row plus one verdict row; B-never has a
    # `declared` row and nothing else. The 2026-08-16 bug read the first as
    # untouched because it counted rows.
    check("a bar with a recorded verdict does not read as untouched",
          "no transition" not in _cause_line(bars["B-yellow-overdue"], voc, None),
          _cause_line(bars["B-yellow-overdue"], voc, None))
    check("a bar with only a `declared` row does read as untouched",
          "no transition" in _cause_line(bars["B-never"], voc, None))

    # --- coverage is a separate axis from verdict -------------------------
    orders = [_fake_order("o-landed", "landed", "t B-never"),
              _fake_order("o-open", "open", "t B-deferred")]
    check("a bar with an order is covered",
          len(covering_orders(bars["B-never"], init, orders, None)) == 1)
    check("a bar with no order is uncovered",
          covering_orders(bars["B-met"], init, orders, None) == [])
    check("LANDED-BUT-UNMOVED fires when a landed order claims an unmoved bar",
          [o.id for o in landed_but_unmoved(bars["B-never"], init, orders, voc, None)] == ["o-landed"])
    check("LANDED-BUT-UNMOVED does not fire for an OPEN order",
          landed_but_unmoved(bars["B-deferred"], init, orders, voc, None) == [])

    # --- as-of ------------------------------------------------------------
    check("--as-of before the met transition reads never-attempted",
          verdict_of(bars["B-met"], voc, "2026-01-04") == NEVER)
    check("--as-of excludes orders drafted later",
          covering_orders(bars["B-never"], init, orders, "2026-01-01") == [])

    # --- serves: parsing, every legal state -------------------------------
    check("serves absent -> unattributed", _fake_order("x", "open", None).attributed is False)
    check("serves (unattributed) -> unattributed",
          _fake_order("x", "open", "(unattributed)").attributed is False)
    check("serves initiative only -> attributed, zero bars",
          _fake_order("x", "open", "t").attributed
          and _fake_order("x", "open", "t").serves_bars == [])
    check("serves initiative + bars -> both parsed",
          _fake_order("x", "open", "t B-met B-never").serves_bars == ["B-met", "B-never"])
    check("serves tolerates commas",
          _fake_order("x", "open", "t B-met, B-never").serves_bars == ["B-met", "B-never"])

    # --- unmappable is REPORTED, never dropped (#6) -----------------------
    ghost = [_fake_order("o-ghost", "landed", "t B-does-not-exist")]
    probs = unmappable(init, ghost, None)
    check("an undeclared bar id in `serves:` is reported, not dropped",
          any("B-does-not-exist" in x for x in probs), str(probs))

    # --- the negative cases: a closed set that is not closed must ERROR ---
    try:
        load(BAD_VERDICT_TOML)
        check("a verdict set without never-attempted is rejected", False,
              "load_declaration accepted it")
    except DataError as exc:
        check("a verdict set without never-attempted is rejected", NEVER in str(exc))
    try:
        load(BAD_TRANSITION_TOML)
        check("a transition value outside the closed set is rejected", False,
              "load_declaration accepted it")
    except DataError as exc:
        check("a transition value outside the closed set is rejected", "postponed" in str(exc))

    # --- the yellow negative cases: five ways to smuggle a pass -----------
    for label, text, needle in (
        ("met-floor in closes_a_bar is rejected", YELLOW_CLOSES_TOML, "closes_a_bar"),
        ("met-floor in only one of transition/verdict is rejected", YELLOW_HALF_TOML, "only one"),
        ("a floor with no floor_basis is rejected", FLOOR_NO_BASIS_TOML, "floor_basis"),
        ("met-floor on a bar with no floor is rejected", YELLOW_NO_FLOOR_TOML, "no `floor`"),
        ("a met-floor transition with no review_by is rejected", YELLOW_NO_REVIEW_TOML, "review_by"),
        ("a met-floor transition with no debt_key is rejected", YELLOW_NO_DEBT_TOML, "debt_key"),
    ):
        try:
            load(text)
            check(label, False, "load_declaration accepted it")
        except DataError as exc:
            check(label, needle in str(exc), str(exc))

    # --- the renderers run over the fixture --------------------------------
    for name, fn in (("coverage", render_coverage), ("postmortem", render_postmortem)):
        buf = io.StringIO()
        fn(init, voc, orders, None, out=buf)
        text = buf.getvalue()
        check(f"{name} renders and names the uncovered/never bars",
              "B-never" in text and NEVER in text)

    # --- the real file parses ---------------------------------------------
    try:
        rvoc, rinits, _ = load_declaration()
        check(f"{BARS_TOML.name} parses ({len(rinits)} initiative(s), "
              f"{sum(len(i.bars) for i in rinits)} bars)", True)
    except DataError as exc:
        check(f"{BARS_TOML.name} parses", False, str(exc))

    print()
    if failures:
        print(f"self-test: FAIL — {len(failures)} of the checks above did not hold")
        return 1
    print("self-test: pass — four verdicts reachable, never-attempted counts as open,")
    print("           closed sets reject out-of-set values, coverage and verdict independent,")
    print("           yellow stays OPEN and every way of turning it into a pass errors.")
    return 0


# --------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(prog="co-lineage.py", description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("command", nargs="?", choices=["coverage", "postmortem", "list"],
                    help="coverage = what is uncovered NOW; postmortem = what happened")
    ap.add_argument("initiative", nargs="?", help="initiative id (see `list`)")
    ap.add_argument("--as-of", metavar="YYYY-MM-DD",
                    help="render the state as of this date — transitions and orders after it "
                         "are excluded. The acceptance test for this tool renders "
                         "native-grounding --as-of 2026-08-11.")
    ap.add_argument("--self-test", action="store_true",
                    help="prove the four verdicts are reachable and the closed sets are closed")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()
    if not args.command:
        ap.print_help()
        return 2
    if args.as_of and not DATE_RE.match(args.as_of):
        print(f"co-lineage: --as-of {args.as_of!r} is not YYYY-MM-DD", file=sys.stderr)
        return 2

    try:
        voc, inits, _ = load_declaration()
    except DataError as exc:
        print(f"co-lineage: {exc}", file=sys.stderr)
        return 3

    orders = load_orders()

    if args.command == "list":
        print(f"{'initiative':<24} {'status':<11} {'bars':<6} {'declared':<11} spec")
        for i in inits:
            print(f"{i.id:<24} {i.status:<11} {len(i.bars):<6} {i.declared:<11} {i.spec}")
        unattributed = [o for o in orders if not o.attributed]
        print()
        print(f"{len(orders)} order file(s) in {FEATURES.relative_to(REPO)}; "
              f"{len(unattributed)} carry no `serves:` (unattributed)")
        return 0

    if not args.initiative:
        print("co-lineage: which initiative? (`co-lineage.py list`)", file=sys.stderr)
        return 2
    init = next((i for i in inits if i.id == args.initiative), None)
    if init is None:
        print(f"co-lineage: no initiative {args.initiative!r} in {BARS_TOML.name} — "
              f"known: {', '.join(i.id for i in inits) or '(none)'}", file=sys.stderr)
        return 2

    if args.command == "coverage":
        render_coverage(init, voc, orders, args.as_of)
    else:
        render_postmortem(init, voc, orders, args.as_of)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
