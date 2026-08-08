#!/usr/bin/env python3
"""Census the judge-prose pollution in committed chaos transcripts.

Reads `*.transcripts.jsonl` and classifies every `failed_once` holding as:

  anchored     the holding is wording of its own answer (emphasis-insensitive).
               `judge::anchor_scan_item` keeps these whatever produced them.
  at-risk      does NOT anchor AND is a claim about the answer/evidence rather
               than a claim about the world. If the specifics scan produced it,
               the fix drops it.
  world-claim  does not anchor, but is a world claim — the per-claim extractor's
               paraphrase. That path does not go through `anchor_scan_item`, so
               the fix cannot touch it.

Provenance is NOT recorded in a transcript row, so "at-risk" is a CEILING on
what the fix removes, not a count of it. The one turn whose raw scan output is
recoverable is `compound-killer-and-lugger`; see
`sovereign-core/src/runtime/grounding/testdata/README.md` for that exact replay.

Usage:  holdings_prose_census.py <dir-with-transcripts.jsonl>
"""

import json
import pathlib
import re
import sys

# Claim-about-the-answer, not claim-about-the-world.
META = re.compile(
    r"^(The assistant|The answer|The passage|The evidence|The text states|The sources)\b",
    re.I,
)
# Shapes only a judge writes: a critique preamble, or a quote with its verdict
# appended (which leaves the quote count odd once the outer quote is trimmed).
PROSE_SHAPES = (
    ("preamble", lambda t: t.endswith(":")),
    ("quote+commentary", lambda t: bool(re.search(r'"\s*[-—–]\s', t))),
    (
        "critique",
        lambda t: bool(
            re.search(
                r"\b(This is fabricated|The evidence does not|Misattribution"
                r"|Fabricated specific)\b",
                t,
                re.I,
            )
        ),
    ),
    ("unbalanced-quote", lambda t: t.count('"') % 2 == 1),
)

# Emphasis is presentation — mirrors judge::anchor_key.
def anchor_key(s: str) -> str:
    return " ".join(re.sub(r"[*_`]", "", s).lower().split())


def answer_body(row: dict) -> str:
    """The draft the gate audited: the released answer minus the note it appended."""
    return (row.get("answer") or "").split("\n---\n*Verification note")[0]


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__)
        return 2
    root = pathlib.Path(argv[1])
    files = sorted(root.glob("*.transcripts.jsonl"))
    if not files:
        print(f"no *.transcripts.jsonl under {root}", file=sys.stderr)
        return 1

    grand = {"anchored": 0, "at-risk": 0, "world-claim": 0}
    print(f"{'transcript':<46} {'held':>5} {'anch':>5} {'risk':>5} {'world':>6}")
    for path in files:
        counts = {"anchored": 0, "at-risk": 0, "world-claim": 0}
        detail = []
        for line in path.open():
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            body = anchor_key(answer_body(row))
            for holding in (row.get("epistemic_state") or {}).get("holdings") or []:
                if holding.get("verification") != "failed_once":
                    continue
                claim = holding.get("claim", "")
                if anchor_key(claim) in body:
                    counts["anchored"] += 1
                    continue
                shapes = [n for n, f in PROSE_SHAPES if f(claim.strip())]
                if META.match(claim.strip()) or shapes:
                    counts["at-risk"] += 1
                    detail.append((row.get("id"), shapes or ["meta-subject"], claim))
                else:
                    counts["world-claim"] += 1
        total = sum(counts.values())
        if not total:
            continue
        for k in grand:
            grand[k] += counts[k]
        print(
            f"{path.name.replace('.transcripts.jsonl', ''):<46} {total:>5} "
            f"{counts['anchored']:>5} {counts['at-risk']:>5} {counts['world-claim']:>6}"
        )
        for turn, shapes, claim in detail:
            print(f"      at-risk [{turn}] {'/'.join(shapes)}: {claim[:88]}")

    print()
    print(
        f"TOTAL failed_once={sum(grand.values())}  anchored={grand['anchored']}  "
        f"at-risk={grand['at-risk']}  world-claim={grand['world-claim']}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
