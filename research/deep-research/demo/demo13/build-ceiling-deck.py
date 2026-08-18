#!/usr/bin/env python3
"""T6a phase 1b + t6b pilot — build the ceiling deck (the perfect-acquisition
arm).

The operator's question (2026-08-18): can the rest of the system — draft,
verifier, render — take perfect article acquisition and turn it into an
equivalent-or-better RACE score? The ceiling deck answers it with zero loop
code: each hit's body IS perplexity's official article for that task (the
pinned A/B input, sha256 b1ce5783…), served by the mock backend's term-index
search (the body is the match surface). The loop then drafts, verifies, and
renders against this evidence — the downstream stack, isolated.

Caveat (named in the pre-registration): the deck feeds the ANSWER, not the
sources — an upper-bound acquisition, not a realistic one.

Deck v2 (the t6b pilot declaration, operator steer 2026-08-18): each task
gains hit B — one Wikipedia page (the URL the landed demo12 runs fetched for
that task; pinned in WIKI_URLS). Fetched ONCE at build time, the body
(HTML-tag-stripped text) lands in this directory and its sha256 is recorded
in deck-sources.json. Re-running this builder RE-FETCHES; the flown deck is
the committed one.

Output: deck.toml + task-<id>.md + task-<id>-w.md bodies + deck-sources.json.
"""
import hashlib
import html
import json
import re
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ARTICLES = (HERE.parent.parent / "drb" / "overall-derivation" / "inputs"
            / "perplexity-subset-articles.jsonl")

# The pinned A/B input — refuse loudly on drift (§18.3, never silent).
PINNED_SHA_PREFIX = "b1ce5783"

# Hit B per task — the Wikipedia page the landed demo12 runs fetched
# (the run manifests' sources.fetched, collected 2026-08-18).
WIKI_URLS = {
    56: "https://en.wikipedia.org/wiki/Auction#Other_features",
    58: "https://en.wikipedia.org/wiki/Horizontal_gene_transfer#Abstract",
    59: "https://en.wikipedia.org/wiki/Bird_migration",
    62: "https://en.wikipedia.org/wiki/Quantum_computing#Physical_realizations",
    65: "https://en.wikipedia.org/wiki/Control_engineering#Control_theory",
    69: "https://en.wikipedia.org/wiki/Communication_protocol#Ossification",
    78: "https://en.wikipedia.org/wiki/Parkinson%27s_disease#Signs_and_symptoms",
    83: "https://en.wikipedia.org/wiki/Tablet_computer#Modern_tablets",
    90: "https://en.wikipedia.org/wiki/Self-driving_car#Commercialization",
    95: "https://en.wikipedia.org/wiki/Diamond_Sutra#History",
}

STOP = set("""a an the of and or in on for to with by from as is are was were
be been at that this these those it its their there then than which who whom
what when where why how not no nor so such if but also into over under
between among through during before after above below up down out off again
further once here all any both each few more most other some only own same
""".split())


def sha256_prefix(path: Path) -> str:
    h = hashlib.sha256(path.read_bytes()).hexdigest()
    return h[:8]


def fetch_text(url: str) -> str:
    """Fetch a page once and reduce it to text (tag-strip + unescape). A
    failed fetch refuses loudly — the deck must never silently carry a
    missing second origin."""
    req = urllib.request.Request(url, headers={
        "User-Agent": "commonwealth-ai-deep-research-ceiling-deck/1.0 "
                      "(local benchmark fixture build; one fetch per build)"})
    with urllib.request.urlopen(req, timeout=60) as r:            # noqa: S310
        raw = r.read().decode("utf-8", errors="replace")
    raw = re.sub(r"<(script|style)[^>]*>.*?</\1>", " ", raw, flags=re.S | re.I)
    raw = re.sub(r"<[^>]+>", " ", raw)
    text = html.unescape(raw)
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"\n\s*\n+", "\n", text)
    if len(text.strip()) < 2000:
        raise SystemExit(f"exit 3: fetched {url} yields {len(text)} chars — "
                         "too small to be a real second origin, refusing")
    return text.strip()


def main():
    got = sha256_prefix(ARTICLES)
    if not got.startswith(PINNED_SHA_PREFIX):
        raise SystemExit(f"exit 3: subset-articles sha256 {got}… != pinned "
                         f"{PINNED_SHA_PREFIX}… — refusing to build the deck")
    rows = [json.loads(l) for l in open(ARTICLES, encoding="utf-8")]
    assert [r["id"] for r in rows] == [56, 58, 59, 62, 65, 69, 78, 83, 90, 95]

    sources = []
    deck_lines = ["# T6a phase 1b + t6b pilot ceiling deck (v2) — built by "
                  "build-ceiling-deck.py", "version = 1", ""]
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
        deck_lines.append(f"snippet = {json.dumps(r['prompt'][:120])}")
        deck_lines.append(f"match = {json.dumps(uniq)}")
        deck_lines.append(f"body = \"{body_file}\"")
        deck_lines.append('custody = "personal"')
        deck_lines.append("")
        # hit B — the Wikipedia second origin (t6b pilot declaration)
        wiki = WIKI_URLS[tid]
        wiki_body_file = f"task-{tid}-w.md"
        wiki_text = fetch_text(wiki)
        (HERE / wiki_body_file).write_text(wiki_text, encoding="utf-8")
        wiki_sha = hashlib.sha256(wiki_text.encode("utf-8")).hexdigest()
        sources.append({"id": tid, "url": wiki, "body": wiki_body_file,
                        "sha256": wiki_sha, "chars": len(wiki_text)})
        deck_lines.append("[[hit]]")
        deck_lines.append(f"url = \"{wiki}\"")
        deck_lines.append(f"title = \"wikipedia: drb-{tid}\"")
        deck_lines.append(f"snippet = \"second origin for task {tid}\"")
        deck_lines.append("match = []")
        deck_lines.append(f"body = \"{wiki_body_file}\"")
        deck_lines.append('custody = "public-web"')
        deck_lines.append("")
    (HERE / "deck.toml").write_text("\n".join(deck_lines), encoding="utf-8")
    (HERE / "deck-sources.json").write_text(
        json.dumps(sources, indent=2) + "\n", encoding="utf-8")
    print(f"deck v2 built: {HERE}/deck.toml ({len(rows) * 2} hits, "
          f"{len(rows)} pinned articles + {len(rows)} wikipedia second "
          "origins); shas in deck-sources.json")


if __name__ == "__main__":
    main()
