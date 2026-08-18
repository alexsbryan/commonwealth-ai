#!/usr/bin/env python3
"""T6a phase 1b — build the ceiling deck (the perfect-acquisition arm).

The operator's question (2026-08-18): can the rest of the system — draft,
verifier, render — take perfect article acquisition and turn it into an
equivalent-or-better RACE score? The ceiling deck answers it with zero loop
code: each hit's body IS perplexity's official article for that task (the
pinned A/B input, sha256 b1ce5783…), served by the mock backend's term-index
search (the body is the match surface). The loop then drafts, verifies, and
renders against this evidence — the downstream stack, isolated.

Caveat (named in the pre-registration): the deck feeds the ANSWER, not the
sources — an upper-bound acquisition, not a realistic one.

Output: deck.toml + task-<id>.md bodies in this directory.
"""
import json
import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
ARTICLES = (HERE.parent.parent / "drb" / "overall-derivation" / "inputs"
            / "perplexity-subset-articles.jsonl")

# The pinned A/B input — refuse loudly on drift (§18.3, never silent).
PINNED_SHA_PREFIX = "b1ce5783"

STOP = set("""a an the of and or in on for to with by from as is are was were
be been at that this these those it its their there then than which who whom
what when where why how not no nor so such if but also into over under
between among through during before after above below up down out off again
further once here all any both each few more most other some only own same
""".split())


def sha256_prefix(path: Path) -> str:
    import hashlib
    h = hashlib.sha256(path.read_bytes()).hexdigest()
    return h[:8]


def main():
    got = sha256_prefix(ARTICLES)
    if not got.startswith(PINNED_SHA_PREFIX):
        raise SystemExit(f"exit 3: subset-articles sha256 {got}… != pinned "
                         f"{PINNED_SHA_PREFIX}… — refusing to build the deck")
    rows = [json.loads(l) for l in open(ARTICLES, encoding="utf-8")]
    assert [r["id"] for r in rows] == [56, 58, 59, 62, 65, 69, 78, 83, 90, 95]

    deck_lines = ["# T6a phase 1b ceiling deck — built by build-ceiling-deck.py",
                  "version = 1", ""]
    for r in rows:
        tid = r["id"]
        body_file = f"task-{tid}.md"
        (HERE / body_file).write_text(r["article"], encoding="utf-8")
        # distinctive prompt terms (the body is the real match surface —
        # these help round-1 frontier queries land on the right hit)
        words = [w for w in re.findall(r"[A-Za-z][A-Za-z-]{3,}", r["prompt"])
                 if w.lower() not in STOP]
        uniq = list(dict.fromkeys(words))[:20]
        deck_lines.append("[[hit]]")
        deck_lines.append(f"url = \"https://ceiling.drb/task-{tid}\"")
        deck_lines.append(f"title = \"drb-{tid} reference content\"")
        deck_lines.append(f"snippet = \"{r['prompt'][:120]}\"")
        deck_lines.append(f"match = {json.dumps(uniq)}")
        deck_lines.append(f"body = \"{body_file}\"")
        deck_lines.append('custody = "personal"')
        deck_lines.append("")
    (HERE / "deck.toml").write_text("\n".join(deck_lines), encoding="utf-8")
    print(f"deck built: {HERE}/deck.toml ({len(rows)} hits, "
          f"{sum(1 for _ in HERE.glob('task-*.md'))} bodies)")


if __name__ == "__main__":
    main()
