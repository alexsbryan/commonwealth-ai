#!/usr/bin/env python3
"""Each service's OWN published list of the data it collects, taken from its
Privacy Policy and nothing else. This is the truth set D3 is scored against.

The pre-registration says "against each service's own published list"
(PRE-REG decision D3). The first instrument read the FIRST-COLUMN cells of a
service's policy TABLES, which is narrower: X, Facebook and YouTube each
publish their data categories as LISTS, so all three read could-not-judge on
2026-09-20 — a fact about the rule's reach, not about the atlas. This file
widens the truth to what the pre-registration wrote. The bar is untouched
(0.70 on at least 2 of 3).

The rule, stated so a reader can check the output rather than believe it:

* The source is `<service>/Privacy Policy.md` and no other document. A
  collection list in a Trackers Policy or a Data Processor Agreement is not
  the service's answer to "what do you collect about me".
* A heading qualifies when its text matches `HEADING` below — the same three
  phrasings the three services use for their collection sections. A qualifying
  heading's span runs to the next heading of the SAME OR HIGHER level, so a
  `###` subsection stays inside its `##` parent.
* Inside a span an ITEM is a list item, or a bolded lead-in — the label that
  opens a list item or a paragraph. Where a list item opens with a bolded
  lead-in the lead-in is the item, because that is the label and the rest of
  the line is its gloss.
* An item is normalised to lower-case alphanumerics with single spaces, with
  parentheticals removed first, by `decisions.norm` — ONE normaliser across
  both files, so gold and atom names cannot drift apart.
* An item longer than `decisions.MAX_TRUTH_WORDS` words after normalisation is
  PROSE, not a label, and is dropped — the same cap, from the same constant,
  that the table instrument applies to a table cell. Every dropped item is
  written to the gold file with its line, so the drop is checkable.
* Fewer than 8 items is `could-not-judge` for that service, said so and
  never rounded up to a score. Spotify publishes a TABLE, not a list, and reads
  could-not-judge here for exactly that reason — measured over all 27 services
  in this corpus, no service publishes both shapes, so the table instrument and
  this one judge disjoint populations and cannot be cross-checked on any one
  service. That is the reach the widening buys, and it is why the old rule read
  could-not-judge three times.

Verdicts are the queue's four, by name, never two: `passed` (a usable gold
list), `could-not-judge` (under the minimum), `failed` and `never-ran` are
not reachable here — a gold list is built or it is not.
"""
import argparse
import json
import os
import pathlib
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import decisions  # noqa: E402  — one normaliser and one prose cap, not two

# The three phrasings the services use, per the row that ordered this.
HEADING = re.compile(r"information (we|you)|data we collect|what (information|data)", re.I)
MIN_ITEMS = 8
POLICY = "Privacy Policy.md"

ATX = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
SETEXT_H1 = re.compile(r"^=+\s*$")
SETEXT_H2 = re.compile(r"^-{2,}\s*$")
LIST_ITEM = re.compile(r"^\s*(?:[*+-]|\d+[.)])\s+(.*)$")
THEMATIC = re.compile(r"^\s*(?:\*\s*){3,}$|^\s*(?:-\s*){3,}$|^\s*(?:_\s*){3,}$")
BOLD_LEAD = re.compile(r"^\s*\*\*(.+?)\*\*")
PARENTHETICAL = re.compile(r"\([^()]*\)")


def strip_parentheticals(text):
    prev = None
    while prev != text:
        prev = text
        text = PARENTHETICAL.sub(" ", text)
    return text


def headings(md):
    """Every heading as {line, level, text, body_from} — ATX and setext both.

    These policies are scraped to markdown and use setext underlines for their
    top two levels (`X Privacy Policy\n===`), so an ATX-only reader finds no
    collection heading at all in X's policy and silently returns nothing.
    """
    lines = md.splitlines()
    out = []
    i = 0
    while i < len(lines):
        m = ATX.match(lines[i])
        if m:
            out.append({"line": i + 1, "level": len(m.group(1)),
                        "text": decisions.cell(m.group(2)), "body_from": i + 1})
            i += 1
            continue
        nxt = lines[i + 1] if i + 1 < len(lines) else ""
        if lines[i].strip() and not lines[i].lstrip().startswith(("|", "#")):
            level = 1 if SETEXT_H1.match(nxt) else 2 if SETEXT_H2.match(nxt) else 0
            if level:
                out.append({"line": i + 1, "level": level,
                            "text": decisions.cell(lines[i]), "body_from": i + 2})
                i += 2
                continue
        i += 1
    return out


def spans(md):
    """(heading, first_line_index, end_line_index) for every qualifying heading."""
    hs = headings(md)
    total = len(md.splitlines())
    out = []
    for k, h in enumerate(hs):
        if not HEADING.search(h["text"]):
            continue
        end = total
        for later in hs[k + 1:]:
            if later["level"] <= h["level"]:
                end = later["line"] - 1
                break
        out.append((h, h["body_from"], end))
    return out


def raw_items(md, first, end):
    """(raw text, 1-based line, source kind) for every item in one span."""
    lines = md.splitlines()
    found = []
    for n in range(first, min(end, len(lines))):
        line = lines[n]
        if not line.strip() or THEMATIC.match(line) or line.lstrip().startswith(("|", "#")):
            continue
        m = LIST_ITEM.match(line)
        body, kind = (m.group(1), "list-item") if m else (line, "paragraph")
        bold = BOLD_LEAD.match(body)
        if bold:
            found.append((bold.group(1), n + 1, "bold-lead-in"))
        elif kind == "list-item":
            found.append((body, n + 1, "list-item"))
    return found


def build(service, src):
    """The gold list for one service, or a could-not-judge that says why."""
    path = pathlib.Path(src, service, POLICY)
    rel = os.path.relpath(path, os.path.join(HERE, "..", ".."))
    if not path.exists():
        return {"service": service, "source": rel, "verdict": "could-not-judge",
                "reason": f"{service} publishes no {POLICY} in this corpus",
                "headings_matched": [], "items": [], "item_count": 0,
                "prose_dropped": [], "min_items": MIN_ITEMS}
    md = path.read_text()
    matched, items, prose, seen = [], [], [], {}
    for h, first, end in spans(md):
        matched.append({"line": h["line"], "level": h["level"], "text": h["text"],
                        "span_ends_line": end})
        for raw, line, kind in raw_items(md, first, end):
            text = decisions.norm(strip_parentheticals(decisions.cell(raw)))
            if not text:
                continue
            if len(text.split()) > decisions.MAX_TRUTH_WORDS:
                prose.append({"item": text, "line": line, "from": kind})
                continue
            if text in seen:
                continue
            seen[text] = line
            items.append({"item": text, "line": line, "from": kind, "raw": raw.strip()})
    if not matched:
        reason = (f"no heading in {service}'s {POLICY} matches the collection-section rule "
                  f"/{HEADING.pattern}/")
    elif len(items) < MIN_ITEMS:
        reason = (f"{len(items)} items under {len(matched)} matching heading(s), "
                  f"fewer than the {MIN_ITEMS} a list must publish to be judged")
    else:
        reason = (f"{len(items)} published items under {len(matched)} matching heading(s) "
                  f"({len(prose)} prose items dropped)")
    return {"service": service, "source": rel,
            "verdict": "passed" if len(items) >= MIN_ITEMS else "could-not-judge",
            "reason": reason, "headings_matched": matched, "items": items,
            "item_count": len(items), "prose_dropped": prose, "min_items": MIN_ITEMS}


def write(gold, out_dir):
    os.makedirs(out_dir, exist_ok=True)
    p = os.path.join(out_dir, f"{gold['service']}.json")
    json.dump(gold, open(p, "w"), indent=1, ensure_ascii=False)
    return p


def load(service, gold_dir):
    """The normalised items of a service's gold list, or None when unusable."""
    p = os.path.join(gold_dir, f"{service}.json")
    if not os.path.exists(p):
        return None
    g = json.load(open(p))
    return g if g.get("verdict") == "passed" else g


# ── self-test ────────────────────────────────────────────────────────────────

FIXTURE = """Some Service Privacy Policy
===========================

Preamble prose that is not an item.

1\\. Information We Collect
--------------------------

**1.1 Information you provide us.**

*   **Email address.** Used to sign you in and to reach you about the service.
*   **Payment information.** Your card number and your billing address.
*   Date of birth
*   [Device identifiers](https://example.test/ids)

**Usage information.** We collect information about your activity, including:

*   Posts and other content you post, including the date, the application and the
    version of the client, together with every broadcast you have ever created.
*   Search terms

### A subsection stays inside its parent

*   Approximate location
*   Precise location (when you turn it on)
*   Contacts

2\\. How We Use Information
--------------------------

*   Not an item: this heading does not match the rule.
"""

THIN = """Thin Service Privacy Policy
===========================

What information do we collect?
===============================

*   Email address
*   Phone number
"""


def self_test():
    fixtures = _fixture_root()
    g = build("Some Service", fixtures)
    items = [i["item"] for i in g["items"]]
    assert g["verdict"] == "passed", g
    # the bolded lead-in is the item, not the whole list line it opens
    assert "email address" in items and "payment information" in items, items
    assert not any(i.startswith("email address used to sign") for i in items), items
    # a bare list item with no lead-in is itself; a link keeps its text, loses its url
    assert "date of birth" in items and "device identifiers" in items, items
    # a bolded lead-in on a PARAGRAPH counts too
    assert "usage information" in items, items
    # a parenthetical is stripped before normalisation
    assert "precise location" in items, items
    # a `###` subsection is inside its `##` parent's span
    assert "contacts" in items, items
    # the non-matching heading's list is outside every span
    assert not any("not an item" in i for i in items), items
    # prose is dropped, counted, and still readable with its line
    assert len(g["prose_dropped"]) == 1, g["prose_dropped"]
    assert g["prose_dropped"][0]["line"] == 18, g["prose_dropped"]
    assert len(items) == 10 and g["item_count"] == 10, items
    assert [i["line"] for i in g["items"]][:2] == [9, 11], g["items"][:2]

    # planted failing input: a policy whose matching heading publishes 2 items,
    # which is under the minimum and is a could-not-judge, never a 2-item score
    thin = build("Thin Service", fixtures)
    assert thin["verdict"] == "could-not-judge", thin
    assert thin["item_count"] == 2 and "fewer than the 8" in thin["reason"], thin

    # planted failing input: a service with no Privacy Policy is could-not-judge,
    # never an empty gold list scored as zero recall
    missing = build("No Such Service", fixtures)
    assert missing["verdict"] == "could-not-judge" and missing["item_count"] == 0, missing
    assert "publishes no" in missing["reason"], missing

    # Instrument validation on real text, not a fixture. Two halves, because the
    # first thing tried here does NOT work and the reason is the finding: Spotify
    # is the service the 2026-09-19 spike measured 10 of 10 against, and its ten
    # categories are a TABLE, so the list rule cannot recover them and must not
    # pretend to. Measured across all 27 services in this corpus (2026-09-20), no
    # service publishes both shapes: the table instrument judges 2 (BeReal,
    # Spotify), the list instrument judges 7, and the two sets are disjoint. So
    # there is no service on which the two can be cross-checked, and this widening
    # reaches a population the old rule could not reach at all.
    root = pathlib.Path(HERE, "..", "spikes", "fineprint-data", "ota-pga-versions")
    spotify = build("Spotify", root)
    table, _, _ = decisions.data_table_truth(decisions.service_tables(root, "Spotify"))
    assert len(table) == 10, (len(table), table)                  # the spike's pin, untouched
    assert spotify["item_count"] == 0, spotify["items"]           # a table is not a list...
    assert spotify["verdict"] == "could-not-judge", spotify       # ...and says so, never a 0.0

    # The other half: the list rule must reach the labels a reader sees. These
    # four are bolded lead-ins in X's own Privacy Policy, greppable at the lines
    # the gold file records, and they are the shape D3 asks about.
    x = build("X", root)
    assert x["verdict"] == "passed", x["reason"]
    got = {i["item"] for i in x["items"]}
    for label in ("biometric information", "location information", "device information",
                  "log information"):
        assert label in got, (label, sorted(got))
    # and the span stops at the next heading of the same level, so section 2's
    # list of USES never lands in a list of what is COLLECTED
    assert [h["line"] for h in x["headings_matched"]] == [66], x["headings_matched"]
    assert x["headings_matched"][0]["span_ends_line"] == 133, x["headings_matched"]
    print(f"self-test: passed (X yields {x['item_count']} published items; "
          f"Spotify's {len(table)}-cell table yields no list, said so)")


def _fixture_root():
    root = pathlib.Path(HERE, "..", "spikes", "fineprint-data", "ota-pga-versions")
    import tempfile
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="gold-lists-self-test-"))
    for svc, md in (("Some Service", FIXTURE), ("Thin Service", THIN)):
        (tmp / svc).mkdir()
        (tmp / svc / POLICY).write_text(md)
    for svc in ("Spotify",):
        (tmp / svc).symlink_to(root / svc)
    return tmp


# ── main ─────────────────────────────────────────────────────────────────────

def main(a):
    for service in a.service:
        gold = build(service, a.src)
        p = write(gold, a.out)
        print(f"{service:<12} {gold['verdict']:<16} {gold['item_count']:>3} items  {gold['reason']}")
        for h in gold["headings_matched"]:
            print(f"    h{h['level']} line {h['line']:>4}  {h['text']}")
        for i in gold["items"]:
            print(f"      {i['line']:>4}  {i['item']}")
        if gold["prose_dropped"]:
            print(f"    dropped as prose: {len(gold['prose_dropped'])}")
        print(f"    -> {p}")


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--service", action="append", default=[])
    ap.add_argument("--src", default=os.path.join(HERE, "..", "spikes", "fineprint-data",
                                                  "ota-pga-versions"))
    ap.add_argument("--out", default=os.path.join(HERE, "gold"))
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        self_test()
    elif not a.service:
        ap.error("--service <S> is required (repeatable)")
    else:
        main(a)
