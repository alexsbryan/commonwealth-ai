#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""How much of the evidence's TECHNICAL SUBSTANCE reaches the deliverable.

THE QUESTION IT ANSWERS. Section length was recalibrated on 2026-08-27 after a
457-word section was found to state a protocol's shape and drop its primitives
("somewhat high-level regarding internal primitives" — the judge). Deciding
what length restores them by re-asking that judge is circular, and the judge is
the proxy we just stepped off. This measures it directly, deterministically,
with no model call: of the named technical things the evidence establishes, how
many does the report actually name?

WHAT A "NAMED TECHNICAL THING" IS. A token that looks like an identifier a
protocol spec would define — CamelCase (`AgentCard`), dotted/slashed method
names (`tasks/send`), hyphenated standards (`JSON-RPC`), or an all-caps acronym
(`SSE`) — appearing in AT LEAST TWO distinct evidence chunks. The two-chunk
floor is what separates a real term from one source's coinage or an artifact of
one page's boilerplate.

IT FAILED ITS OWN VALIDATION AS A STRUCTURE MEASURE — READ THIS BEFORE USING
IT. Validated 2026-08-27 against a DUMB TRUNCATION control: the 11,270-word
`read-arch` report cut to its first 3,991 words names 37 terms (9.2%), while
the purpose-built 3,991-word `read-ethos` report names 34 (8.5%). Coverage
therefore tracks LENGTH, not the quality of what was kept — it cannot tell a
well-chosen 4,000 words from the first 4,000 words, and on this bed the
structured short report scored slightly WORSE than crude truncation. So:

  - Do NOT use coverage to choose a section length. It will always prefer the
    longer arm, which makes it an argument for length dressed as an argument
    for substance.
  - DO use the dropped-term list. "This arm does not name `AgentCard`,
    `DataPart`, `tasks/send`" is a concrete, checkable statement about what a
    reader will not find, whatever caused it.
  - The density column (terms per 1k words) is the one number the truncation
    control does not flatten, and it separates TRUNCATED from PADDED: a short
    report with high density kept specifics and ran out of room; a long report
    with low density is spending words on something other than the subject.

WHAT IT IS NOT, generally. Not a quality measure, and it must never be
reported as one. A report can name every term and explain none of them; naming
is necessary, not sufficient. Read it beside the deliverable, never instead of
it (§18.4 — and this instrument is why that section exists).

    substance_coverage.py <window.json|compose-input.json> <report.md> [<report.md> ...]
"""
import json, re, sys
from pathlib import Path
from collections import Counter

MIN_CHUNKS = 2                      # a term must survive in two sources
PATTERNS = [
    r"\b[a-z]+[A-Z][A-Za-z]+\b",            # camelCase
    r"\b[A-Z][a-z]+[A-Z][A-Za-z]*\b",       # PascalCase with an inner cap
    r"\b[a-z]+/[a-z][A-Za-z]+\b",           # tasks/sendSubscribe
    r"\b[A-Z]{2,}(?:-[A-Z0-9]+)+\b",        # JSON-RPC, HTTP-SSE
    r"\b[A-Z]{3,}\b",                       # SSE, OAuth-style acronyms
]
# Words that match the shapes above and carry no technical content.
STOP = {"THE", "AND", "FOR", "WITH", "THIS", "THAT", "FROM", "ARE", "NOT",
        "API", "AI", "ITS", "ALL", "ONE", "TWO", "NEW", "USE", "VIA", "PDF"}


def terms(text: str) -> set:
    out = set()
    for p in PATTERNS:
        for m in re.findall(p, text):
            if m.upper() in STOP or len(m) < 3:
                continue
            out.add(m)
    return out


def evidence_terms(path: Path) -> set:
    d = json.loads(path.read_text())
    chunks = (d.get("window") or d).get("chunks") or []
    seen = Counter()
    for c in chunks:
        for t in terms(c.get("content") or ""):
            seen[t] += 1
    return {t for t, n in seen.items() if n >= MIN_CHUNKS}


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    ev = evidence_terms(Path(argv[0]))
    if not ev:
        sys.exit("REFUSED: the evidence yielded no terms — wrong file, or an empty window")
    print(f"evidence establishes {len(ev)} technical terms "
          f"(seen in >= {MIN_CHUNKS} chunks)\n")
    hdr = f"{'report':<34}{'words':>8}{'named':>8}{'coverage':>10}{'per 1k words':>14}"
    print(hdr); print("-" * len(hdr))
    rows = []
    for a in argv[1:]:
        p = Path(a)
        if not p.is_file():
            print(f"{a:<34}{'MISSING — reported, not skipped':>40}")
            continue
        txt = p.read_text()
        words = len(txt.split())
        named = ev & terms(txt)
        cov = 100.0 * len(named) / len(ev)
        dens = 1000.0 * len(named) / max(words, 1)
        print(f"{p.parent.parent.name[:33]:<34}{words:>8,}{len(named):>8}"
              f"{cov:>9.1f}%{dens:>14.1f}")
        rows.append((p, words, named, cov))
    # What the SHORTEST report dropped that the longest kept — the actual
    # question, spelled out rather than left as a delta.
    if len(rows) >= 2:
        rows.sort(key=lambda r: r[1])
        short, long = rows[0], rows[-1]
        lost = sorted(long[2] - short[2])
        print(f"\n{short[0].parent.parent.name} ({short[1]:,} words) drops "
              f"{len(lost)} term(s) that {long[0].parent.parent.name} "
              f"({long[1]:,} words) names:")
        print("   " + ", ".join(lost[:40]) + (" …" if len(lost) > 40 else ""))
    return rows


if __name__ == "__main__":
    main(sys.argv[1:])
