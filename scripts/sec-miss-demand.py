#!/usr/bin/env python3
# sec-miss-demand.py — F5's SECOND clause, read-tier.
#
# F5's first clause ("the corpus states what it cannot answer") is structural
# and already instrumented in quality/campaigns/financial-corpora.toml. The
# second clause is demand-side: of the concepts people actually ASK a typed SEC
# store for, how many miss for a reason we could FIX?
#
# WHY THIS IS A LOG READER AND NOT A STORE (operator constraint, verbatim:
# "we need to absolutely prioritize telemetry that offers as much signal with
# as little burden as possible otherwise it will just be another speedbump we
# route around"). The miss already happens in exactly one place and is already
# traced, so there is nothing to remember to run and nothing to keep current:
#   - no new store, no schema, no migration, no retention policy;
#   - no new cadence — it reads logs a run ALREADY produced (runs/*/evidence/
#     real-daemon.log, or any daemon log with `sec_facts=debug` on);
#   - no new step on anyone's path — pointing it at yesterday's run dir works.
#
# THE WRITER IS RUST. `corpus_engine::enrichment::atlas::analysis::sec_facts::
# resolve_concept` emits ONE event per concept ask, carrying the anchor field
# named by the `F5_DEMAND_ANCHOR` const in that module. That const is the
# cross-language contract: --self-test greps the Rust source for it, so
# renaming the anchor fails this script's self-test instead of silently
# zeroing the instrument. (Same split-brain shape as check-sec-corpus.py's
# fact-line grammar, made checkable rather than merely documented.)
#
# WHAT COUNTS AS FIXABLE. A miss is FIXABLE when the requested concept matches
# a tag the filer actually reports that our concept map does not yet type —
# i.e. a member of the corpus's own `_unmapped_concepts.json`. That is a
# DECLARED membership test, never a similarity guess (ARCH §18.3).
# Everything else is reported as `unclassified` and is NEVER folded into the
# numerator. In particular:
#   - `consolidated_only` is a STORE-level flag, not a per-miss classifier
#     (Coverage.consolidated_only, sec_facts/mod.rs). It says this SOURCE has
#     no dimension axis; it cannot tell you that THIS ask was a segment ask.
#     So a segment ask lands in `unclassified`, which is correct: a
#     consolidated-only miss is a SOURCE LIMIT TO DISCLOSE, never a gap to
#     close, and must stay out of any fixable-coverage denominator.
#
# ABSENCE IS REPORTED, NEVER DEFAULTED (ARCH §18.3). Zero asks in the logs is
# exit 3 with a named reason, not `value: 0.0` — an instrument that reports a
# perfect score for "nobody asked anything" is the silent-substitution smell.
#
# Verdicts: prints one JSON object on the last stdout line, in the shape
# co-lineage.py's `_parse_value` reads: {"value": <fixable rate>, ...}.
# Exit: 0 measured; 2 self-test failed; 3 nothing to measure (named).

import argparse
import json
import os
import re
import sys
from pathlib import Path

# The anchor field the Rust writer emits. Kept in ONE place on each side of
# the language boundary; --self-test proves the two spellings still agree.
ANCHOR = "f5_demand"
RUST_WRITER = Path("corpus-engine/src/enrichment/atlas/analysis/sec_facts/mod.rs")

ANSI = re.compile(r"\x1b\[[0-9;]*m")
# Fields are rendered by tracing's default formatter as `key=value`. The
# concept is emitted with Debug (`?requested`) precisely so a concept spelled
# with spaces — "gross profit" — arrives quoted and parseable.
REQUESTED = re.compile(r'requested="((?:[^"\\]|\\.)*)"')
# `outcome` arrives QUOTED — tracing renders a &str field value through
# `record_str`, which the fmt layer writes with quotes. Measured, not
# assumed: the Rust-side test
# `f5_demand_event_renders_the_grammar_the_reader_parses` renders a real
# event through a real subscriber and pins this exact spelling. The
# optional quotes keep an unquoted rendering (a `%`-sigil field, say)
# readable rather than silently unmatched.
OUTCOME = re.compile(r'outcome="?(\w+)"?')
CONSOLIDATED = re.compile(r"consolidated_only=(true|false)")


def normalize(s: str) -> str:
    """Concept-name normalization, reader half. Mirrors the Rust `normalize`
    plus the id-form substitution in `resolve_concept`: lowercase, and any
    run of non-alphanumerics collapsed to a single underscore. XBRL tags are
    CamelCase (`ResearchAndDevelopmentExpense`), so the tag side additionally
    gets its case boundaries split before this runs — see `tag_forms`."""
    return re.sub(r"[^a-z0-9]+", "_", s.lower()).strip("_")


def tag_tokens(tag: str) -> tuple:
    """An XBRL tag as its token sequence. CamelCase is the tag grammar, so
    the case boundaries ARE the token boundaries:
    `DeferredRevenueCurrent` -> ('deferred', 'revenue', 'current')."""
    split = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", tag)
    return tuple(t for t in normalize(split).split("_") if t)


def is_fixable(requested: str, untyped: list) -> bool:
    """Does this ask name a tag the filer reports but our map does not type?

    TWO DECLARED STEPS, mirroring `resolve_concept`'s own two — never a
    similarity guess (ARCH §18.3):

    1. token-sequence EQUALITY with an untyped tag;
    2. the ask is a token-boundary PREFIX of an untyped tag, and is at least
       two tokens long. XBRL compounds a qualifier onto the end
       (`DeferredRevenue` + `Current`), so `deferred revenue` naming
       `DeferredRevenueCurrent` is the ordinary case, not a stretch. The
       two-token floor is what keeps a bare `revenue` — which prefixes
       dozens of unrelated tags — from being scored fixable.

    Token-boundary, never substring: `revenue` is not a prefix of
    `deferred_revenue_current`, so it does not match here.
    """
    ask = tuple(t for t in normalize(requested).split("_") if t)
    if not ask:
        return False
    for tag in untyped:
        toks = tag_tokens(tag)
        if ask == toks:
            return True
        if len(ask) >= 2 and len(ask) < len(toks) and toks[:len(ask)] == ask:
            return True
    return False


def parse_events(paths) -> list:
    """Every f5_demand event in these logs, oldest file first."""
    events = []
    for p in paths:
        try:
            text = Path(p).read_text(errors="replace")
        except OSError as e:
            print(f"# unreadable: {p}: {e}", file=sys.stderr)
            continue
        for line in text.splitlines():
            if ANCHOR not in line:
                continue
            line = ANSI.sub("", line)
            m = REQUESTED.search(line)
            o = OUTCOME.search(line)
            if not m or not o:
                # A line carrying the anchor but not the fields is a WRITER
                # change this reader has not caught up with — say so rather
                # than dropping it (ARCH §18.3).
                print(f"# anchor without fields, skipped: {line[:160]}",
                      file=sys.stderr)
                continue
            c = CONSOLIDATED.search(line)
            events.append({
                "requested": m.group(1),
                "outcome": o.group(1),
                "consolidated_only": (c.group(1) == "true") if c else None,
                "source": str(p),
            })
    return events


def untyped_tags(corpus_dir: Path) -> list:
    """The filer's own tags our map does not type, from the corpus's
    `_unmapped_concepts.json`. Returns an empty list when the artifact is
    absent — the caller distinguishes that from "no untyped tags".

    `unmapped` is the shipped key (scripts/sec_facts.py's writer, and the
    real artifact under ~/.svrnmesh/.../raw/_unmapped_concepts.json).

    FOUND, NOT ASSUMED. The artifact lands under a downloads directory
    whose name comes from the recipe, not under the index dir, and
    hard-coding that path is the class of guess that has cost this
    initiative a run at a time. So `--corpus` may name any ancestor and
    this searches beneath it, newest first (a re-install under the same
    single-instance corpus id replaces the previous filer's list, and the
    misses being classified belong to the LATEST install)."""
    direct = [corpus_dir / "_unmapped_concepts.json",
              corpus_dir / "raw" / "_unmapped_concepts.json"]
    found = [p for p in direct if p.exists()]
    if not found:
        found = sorted(corpus_dir.rglob("_unmapped_concepts.json"),
                       key=lambda p: p.stat().st_mtime, reverse=True)
    if not found:
        return []
    print(f"# untyped-tag list: {found[0]}", file=sys.stderr)
    return list(json.loads(found[0].read_text()).get("unmapped", []))


def classify(events: list, untyped: list) -> dict:
    asks = len(events)
    misses = [e for e in events if e["outcome"] == "unmapped"]
    fixable, unclassified = [], []
    for e in misses:
        (fixable if is_fixable(e["requested"], untyped) else unclassified).append(e)

    def tally(rows):
        out = {}
        for r in rows:
            out[r["requested"]] = out.get(r["requested"], 0) + 1
        return dict(sorted(out.items(), key=lambda kv: (-kv[1], kv[0])))

    return {
        "asks": asks,
        "misses": len(misses),
        "fixable": len(fixable),
        "unclassified": len(unclassified),
        "fixable_by_concept": tally(fixable),
        "unclassified_by_concept": tally(unclassified),
    }


# COPIED FROM A REAL RENDER, not composed here. These four lines are the
# shape `tracing_subscriber::fmt` actually produced for this event in
# corpus-engine's `f5_demand_event_renders_the_grammar_the_reader_parses`
# — including `outcome="resolved"` quoted and `resolved=Some("...")`,
# both of which an invented fixture got wrong on the first attempt and
# which would have made this reader measure a clean zero forever.
SELF_TEST_LOG = """\
2026-08-18T01:54:06.666369Z DEBUG sec_facts: sec_facts: concept ask f5_demand=true requested="gross profit" outcome="resolved" resolved=Some("gross_profit") consolidated_only=true store_concepts=20
2026-08-18T01:54:06.666401Z DEBUG sec_facts: sec_facts: concept ask f5_demand=true requested="Research and Development Expense" outcome="resolved" resolved=Some("research_and_development_expense") consolidated_only=true store_concepts=20
2026-08-18T01:54:06.666430Z DEBUG sec_facts: sec_facts: concept ask f5_demand=true requested="deferred revenue" outcome="unmapped" resolved=None consolidated_only=true store_concepts=20
2026-08-18T01:54:06.666455Z DEBUG sec_facts: sec_facts: concept ask f5_demand=true requested="Services revenue" outcome="unmapped" resolved=None consolidated_only=true store_concepts=20
2026-08-18T01:54:06.666480Z  INFO sec_facts: typed fact store installed corpus=sec-filings-company
"""

SELF_TEST_TAGS = {
    "covered_by_map": ["GrossProfit", "ResearchAndDevelopmentExpense"],
    # `unmapped` is the shipped key — the fixture uses the SAME spelling the
    # real artifact does, so a key rename fails here rather than in the field.
    "unmapped": ["DeferredRevenueCurrent", "ContractWithCustomerLiability"],
}


def self_test(tmp: Path) -> int:
    """Validate the instrument before the result (ARCH §18.4). Four checks,
    each with a failing input named in its message."""
    fails = []
    log = tmp / "self-test.log"
    log.write_text(SELF_TEST_LOG)
    events = parse_events([log])
    if len(events) != 4:
        fails.append(f"parse: expected 4 anchored events, got {len(events)} "
                     f"(the un-anchored INFO line must not be counted)")
    if events and events[0]["requested"] != "gross profit":
        fails.append("parse: a concept spelled with a SPACE did not survive "
                     f"the field grammar: {events[0]['requested']!r}")

    (tmp / "_unmapped_concepts.json").write_text(json.dumps(SELF_TEST_TAGS))
    untyped = untyped_tags(tmp)
    got = classify(events, untyped)
    # POSITIVE arm: a miss on a tag the filer DOES report is fixable.
    if got["fixable"] != 1 or "deferred revenue" not in got["fixable_by_concept"]:
        fails.append("classify POSITIVE: 'deferred revenue' matches the "
                     "filer's untyped DeferredRevenueCurrent and must count "
                     f"fixable; got {got['fixable_by_concept']}")
    # NEGATIVE arm: a segment ask is NOT fixable and must never enter the
    # numerator, even though the store is consolidated_only.
    if got["unclassified"] != 1 or "Services revenue" not in got["unclassified_by_concept"]:
        fails.append("classify NEGATIVE: 'Services revenue' is a segment ask "
                     "— a source limit to disclose, never a gap to close — "
                     f"and must stay unclassified; got {got}")
    # The cross-language contract: the Rust writer still spells the anchor
    # the way this reader greps for it.
    if RUST_WRITER.exists():
        src = RUST_WRITER.read_text()
        if f'F5_DEMAND_ANCHOR: &str = "{ANCHOR}"' not in src:
            fails.append(
                f"contract: {RUST_WRITER} no longer declares "
                f'F5_DEMAND_ANCHOR = "{ANCHOR}". The writer was renamed and '
                "this reader would silently measure zero.")
    else:
        fails.append(f"contract: writer source not found at {RUST_WRITER} "
                     "(run from the repo root)")

    for f in fails:
        print(f"FAIL {f}")
    if not fails:
        print("self-test: 4 checks passed (parse, spaced concept, fixable "
              "positive arm, segment negative arm, writer-anchor contract)")
    return 2 if fails else 0


def main(argv) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("logs", nargs="*", help="daemon/app log files to read")
    ap.add_argument("--corpus", help="installed corpus dir holding "
                                     "_unmapped_concepts.json")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--json", action="store_true",
                    help="print the full breakdown, not just the value line")
    args = ap.parse_args(argv)

    if args.self_test:
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            return self_test(Path(d))

    if not args.logs:
        print("nothing to measure: no log files given", file=sys.stderr)
        return 3
    events = parse_events(args.logs)
    if not events:
        print(f"nothing to measure: no '{ANCHOR}' events in "
              f"{len(args.logs)} log(s). Either no SEC concept was asked for, "
              f"or the run did not carry sec_facts=debug — absence of the "
              f"event is NOT a coverage score.", file=sys.stderr)
        return 3

    untyped = set()
    if args.corpus:
        untyped = untyped_tags(Path(os.path.expanduser(args.corpus)))
        if not untyped:
            print(f"# no untyped-tag list under {args.corpus} — every miss "
                  f"will report as unclassified", file=sys.stderr)
    else:
        print("# no --corpus given: fixability cannot be decided, so every "
              "miss reports as unclassified", file=sys.stderr)

    got = classify(events, untyped)
    if args.json:
        print(json.dumps(got, indent=2))
    got["value"] = got["fixable"] / got["asks"]
    got["artifact"] = args.logs[0]
    print(json.dumps(got))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
