#!/usr/bin/env python3
"""
Fetch Wikipedia Vital Articles for **levels 1-4** and emit one
newline-delimited title list per level.

Companion to `build_vital_articles.py`, which scrapes the much larger
Level 5 set via `prop=links`. L1-L4 pages are short enough that the
links action picks up too much chrome (footer "see also" links, prose
mentions, navbar entries, talk-page redirects). For these tiers we
parse the wikitext directly: only `[[Title]]` patterns appearing in
bulleted list items (`* …`) are accepted, mirroring how the curator
community formats the canonical roster.

Output (one file per level):
    vital_articles_l1.txt   ~10 titles
    vital_articles_l2.txt   ~100 titles
    vital_articles_l3.txt   ~1,000 titles
    vital_articles_l4.txt   ~10,000 titles

The same `# comment`-tolerant format as `vital_articles_l5.txt` so
both consumers (the corpus-engine `title_list` filter for ingestion
filtering, and the post-install triage prior for atlas centrality)
share one parser.

Usage:
    python build_vital_articles_tiered.py \\
        --out-dir ../data \\
        [--level 1 2 3 4]   # default: all four

Environment:
    Set WIKI_USER_AGENT to a contact-string per Wikimedia's policy.

Notes on the wikitext-parse heuristic:
    - L1-L4 pages contain a `=Level N vital articles=` (or similar)
      heading followed by a flat or sectioned bulleted list. We
      extract `[[Title]]` from any line starting with `*` (after
      stripping `{{Icon|…}}`, `{{...}}` template wrappers).
    - We deliberately ignore links inside `=Headings=`, navbox
      templates (`{{...}}`), prose paragraphs, and `<ref>...</ref>`
      blocks — those are chrome, not the vital roster.
    - L1's single page lists 10 articles. L2 splits across 11
      topical subpages. L3 across 12. L4 across ~50 (deeper topical
      decomposition).
    - The L5 script is intentionally NOT changed: prop=links on L5
      already gives the right answer (L5 pages are nothing but flat
      link lists by design) and switching to wikitext parsing would
      take orders of magnitude longer for negligible quality gain.
"""

import argparse
import os
import re
import sys
import time
import urllib.parse
import urllib.request
import json
from typing import Iterator, List, Set


WIKI_API = "https://en.wikipedia.org/w/api.php"
DEFAULT_UA = "sovereign-recipes/0.1 (https://github.com/your-org/sovereign-recipes; contact via PR)"

# Subpage suffixes that aren't part of the curated roster — drop
# these before fetching wikitext. "Article alerts", "Draft",
# "Sandbox", "Nav bar", and dated talk-archive subpages all show up
# in the prefix discovery but contain no vital titles.
NON_ROSTER_SUFFIXES = (
    "/Article alerts",
    "/Draft",
    "/Sandbox",
    "/Nav bar",
    "/Candidates",
    "/Removed",
    "/Archive",
)


def http_get_json(params: dict, ua: str, max_retries: int = 5) -> dict:
    qs = urllib.parse.urlencode(params)
    url = f"{WIKI_API}?{qs}"
    delay = 1.0
    for attempt in range(max_retries):
        req = urllib.request.Request(url, headers={"User-Agent": ua})
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return json.loads(resp.read().decode("utf-8"))
        except Exception as e:
            if attempt + 1 == max_retries:
                raise
            print(f"  retry {attempt + 1}/{max_retries} after error: {e}", file=sys.stderr)
            time.sleep(delay)
            delay *= 2


def discover_subpages_for_level(level: int, ua: str) -> List[str]:
    """List all `Wikipedia:Vital articles/Level N/*` pages plus the
    parent `Wikipedia:Vital articles/Level N`. Roster-only filtering
    happens after fetch."""
    pages: List[str] = []
    apcontinue = None
    while True:
        params = {
            "action": "query",
            "list": "allpages",
            "apprefix": f"Vital articles/Level {level}",
            "apnamespace": 4,
            "aplimit": "max",
            "format": "json",
            "formatversion": 2,
        }
        if apcontinue:
            params["apcontinue"] = apcontinue
        data = http_get_json(params, ua)
        for entry in data.get("query", {}).get("allpages", []):
            t = entry["title"]
            if any(t.endswith(s) or s in t for s in NON_ROSTER_SUFFIXES):
                continue
            pages.append(t)
        cont = data.get("continue")
        if not cont or "apcontinue" not in cont:
            break
        apcontinue = cont["apcontinue"]
        time.sleep(0.1)
    pages = sorted(set(pages))
    return pages


def fetch_wikitext(title: str, ua: str) -> str:
    params = {
        "action": "parse",
        "page": title,
        "prop": "wikitext",
        "format": "json",
        "formatversion": 2,
    }
    data = http_get_json(params, ua)
    return data.get("parse", {}).get("wikitext", "")


# Match `[[Target]]` or `[[Target|Display]]` — capture Target only.
# Stops at `]]` or `|` so `[[Foo|bar]]` → `Foo`.
WIKILINK_RE = re.compile(r"\[\[([^\[\]\|#]+?)(?:\||\]\])")


def extract_list_link_titles(wikitext: str) -> Iterator[str]:
    """Yield wikilink targets from bulleted (`*`) or numbered (`#`)
    list items only. L1-L3 pages use `*`; L4 uses `#` (the curators
    moved to ordered lists at L4 to encode rank/quota signal).

    Defensive against:
      - Heading lines (`=…=`, `==…==`) — skipped wholesale.
      - Template wrappers (`{{Icon|FA}} [[Foo]]`) — the regex still
        finds the bracket pattern, we just need to not pick up
        template arguments.
      - File/Image/Category links — start with a namespace prefix
        we filter out.
      - Self-links (`[[Wikipedia:Vital articles/...]]`) — namespace
        prefix filter handles these too.
    """
    for raw in wikitext.splitlines():
        line = raw.strip()
        if not (line.startswith("*") or line.startswith("#")):
            continue
        for m in WIKILINK_RE.finditer(line):
            target = m.group(1).strip()
            if not target:
                continue
            # Drop namespaced links (File:, Wikipedia:, Category:, …)
            if ":" in target and not target.startswith(" "):
                head = target.split(":", 1)[0]
                if head in (
                    "File",
                    "Image",
                    "Category",
                    "Wikipedia",
                    "Help",
                    "Template",
                    "Portal",
                    "Talk",
                    "User",
                    "MediaWiki",
                    "Module",
                ):
                    continue
            # Strip section anchors that snuck past the regex.
            if "#" in target:
                target = target.split("#", 1)[0]
            yield target


def harvest_level(level: int, ua: str) -> Set[str]:
    print(f"== Level {level} ==", file=sys.stderr)
    subpages = discover_subpages_for_level(level, ua)
    print(f"  {len(subpages)} roster subpages", file=sys.stderr)
    titles: Set[str] = set()
    for i, page in enumerate(subpages):
        wt = fetch_wikitext(page, ua)
        before = len(titles)
        for t in extract_list_link_titles(wt):
            titles.add(t)
        added = len(titles) - before
        print(
            f"  [{i + 1}/{len(subpages)}] {page} → +{added} (cumulative {len(titles)})",
            file=sys.stderr,
        )
        time.sleep(0.1)
    return titles


def write_list(level: int, titles: Set[str], out_dir: str) -> str:
    path = os.path.join(out_dir, f"vital_articles_l{level}.txt")
    os.makedirs(out_dir, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(
            f"# Wikipedia Vital Articles Level {level}\n"
            f"# Generated from https://en.wikipedia.org/wiki/Wikipedia:Vital_articles/Level/{level}\n"
            f"# {len(titles)} titles\n"
            "#\n"
            "# Regenerate via sovereign-recipes/wikipedia/scripts/build_vital_articles_tiered.py.\n"
            "# Companion to vital_articles_l5.txt (which is generated by build_vital_articles.py).\n"
        )
        for t in sorted(titles):
            f.write(t)
            f.write("\n")
    return path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", required=True, help="Where to write vital_articles_lN.txt files")
    parser.add_argument("--user-agent", default=os.environ.get("WIKI_USER_AGENT", DEFAULT_UA))
    parser.add_argument(
        "--level",
        type=int,
        nargs="*",
        default=[1, 2, 3, 4],
        help="Which levels to fetch (default: 1 2 3 4)",
    )
    args = parser.parse_args()

    bad = [l for l in args.level if l not in (1, 2, 3, 4)]
    if bad:
        print(f"--level values must be in [1, 4]; got {bad}", file=sys.stderr)
        return 2

    for lv in args.level:
        titles = harvest_level(lv, args.user_agent)
        path = write_list(lv, titles, args.out_dir)
        print(f"wrote {len(titles)} titles to {path}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())
