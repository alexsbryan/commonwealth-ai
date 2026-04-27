#!/usr/bin/env python3
"""
Scrape `Wikipedia:Vital_articles/Level/5` subpages via the Wikipedia API
and emit a newline-delimited title list for the corpus-engine
`title_list` filter.

The Vital Articles Level 5 set is the community-curated list of
~50,000 articles an encyclopedia "should" cover for any user. It's
organised across topical subpages (People, History, Geography, Arts,
Science, …) — each is a wiki page whose body is largely a flat list
of `[[Title]]` links to mainspace articles. Rather than scrape rendered
HTML, we use the API's `links` action with `pllimit=max` to fetch the
links the curators put on each page and filter to namespace 0
(mainspace).

Output: one title per line, lexically sorted, deduplicated. The Rust
filter (`TitleListFilter::from_bytes`) normalizes case + collapses
underscores at load time, so the on-disk format can preserve whatever
the API returns verbatim.

Usage:
    python build_vital_articles.py \\
        --out ../../../corpus-engine/assets/vital_articles_l5.txt

Environment:
    Set WIKI_USER_AGENT to a contact-string per Wikimedia's policy
    (e.g. "sovereign-recipes/0.1 (you@example.com)"). Default is set
    below; please customise for your fork.

Notes:
    - The L5 page tree uses arbitrarily deep subpages. We discover
      subpages by walking the namespace 4 (Wikipedia:) titles that
      start with "Vital articles/Level/5" via `prefixsearch`.
    - Some subpages organise links in tables; the API's `links` action
      returns every wiki link irrespective of formatting, so we don't
      need an HTML parser.
    - We exclude links to other meta pages, redirects-to-self, and
      anything outside namespace 0. Curator-added "Sub-list:" anchors
      and category links are handled by the namespace filter.
"""

import argparse
import os
import sys
import time
import urllib.parse
import urllib.request
import json
from typing import Iterator, List, Set


WIKI_API = "https://en.wikipedia.org/w/api.php"
DEFAULT_UA = "sovereign-recipes/0.1 (https://github.com/alexsbryan/sovereign-recipes; contact via PR)"


def http_get_json(params: dict, ua: str, max_retries: int = 5) -> dict:
    """GET to the API with retry on transient errors. Wikipedia is
    generally fine with sustained throughput when the UA identifies
    the caller — we sleep 100ms between calls as a courtesy."""
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


def discover_subpages(ua: str) -> List[str]:
    """Walk all `Wikipedia:Vital articles/Level 5/*` subpages via the
    `allpages` API restricted to namespace 4 with the right prefix.

    Note: the canonical path uses a space ("Level 5"), not a slash
    ("Level/5"). The slashed form exists too — as redirect shells —
    but `prop=links` on a redirect doesn't follow through, which is
    why the obvious-looking prefix yielded 0 links per page. Stick to
    the canonical form."""
    pages: List[str] = []
    apcontinue = None
    while True:
        params = {
            "action": "query",
            "list": "allpages",
            "apprefix": "Vital articles/Level 5",
            "apnamespace": 4,  # Wikipedia: meta namespace
            "aplimit": "max",
            "format": "json",
            "formatversion": 2,
        }
        if apcontinue:
            params["apcontinue"] = apcontinue
        data = http_get_json(params, ua)
        for entry in data.get("query", {}).get("allpages", []):
            pages.append(entry["title"])
        cont = data.get("continue")
        if not cont or "apcontinue" not in cont:
            break
        apcontinue = cont["apcontinue"]
        time.sleep(0.1)
    pages.append("Wikipedia:Vital articles/Level 5")
    pages = sorted(set(pages))
    return pages


def fetch_namespace_zero_links(page_title: str, ua: str) -> Iterator[str]:
    """Yield every namespace-0 link from `page_title`. Uses
    `prop=links` with `plnamespace=0` so we don't need to filter
    server-side junk (file: links, category: links, talk: pages)."""
    plcontinue = None
    while True:
        params = {
            "action": "query",
            "titles": page_title,
            "prop": "links",
            "plnamespace": 0,
            "pllimit": "max",
            "format": "json",
            "formatversion": 2,
        }
        if plcontinue:
            params["plcontinue"] = plcontinue
        data = http_get_json(params, ua)
        for page in data.get("query", {}).get("pages", []):
            for link in page.get("links", []):
                yield link["title"]
        cont = data.get("continue")
        if not cont or "plcontinue" not in cont:
            break
        plcontinue = cont["plcontinue"]
        time.sleep(0.1)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, help="Path to write the title list (one per line)")
    parser.add_argument("--user-agent", default=os.environ.get("WIKI_USER_AGENT", DEFAULT_UA))
    parser.add_argument(
        "--max-pages",
        type=int,
        default=0,
        help="Cap the number of subpages scraped (0 = no cap). Useful for development.",
    )
    args = parser.parse_args()

    print(f"Discovering Vital Articles L5 subpages…", file=sys.stderr)
    subpages = discover_subpages(args.user_agent)
    if args.max_pages > 0:
        subpages = subpages[: args.max_pages]
    print(f"  found {len(subpages)} subpages", file=sys.stderr)

    titles: Set[str] = set()
    for i, page in enumerate(subpages):
        print(f"[{i + 1}/{len(subpages)}] {page}", file=sys.stderr)
        before = len(titles)
        for link in fetch_namespace_zero_links(page, args.user_agent):
            titles.add(link)
        added = len(titles) - before
        print(f"   +{added} (cumulative {len(titles)})", file=sys.stderr)

    # The L5 hierarchy includes a few umbrella pages whose links point
    # at *other curator subpages* via lower-level redirects we don't
    # follow. Drop any title that still starts with "Wikipedia:" or
    # "Talk:" — those slipped past the namespace filter only when the
    # curator inserted a redirect from mainspace to a meta page, which
    # is rare but worth defending against.
    titles = {
        t
        for t in titles
        if not t.startswith("Wikipedia:")
        and not t.startswith("Talk:")
        and not t.startswith("Category:")
    }

    out_path = args.out
    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as f:
        f.write(
            "# Wikipedia Vital Articles Level 5\n"
            "# Generated from https://en.wikipedia.org/wiki/Wikipedia:Vital_articles/Level/5\n"
            f"# {len(titles)} titles\n"
            "#\n"
            "# Regenerate via sovereign-recipes/wikipedia/scripts/build_vital_articles.py.\n"
            "# The corpus-engine title_list filter normalizes case + underscores at load time;\n"
            "# whitespace and # comments are skipped.\n"
        )
        for t in sorted(titles):
            f.write(t)
            f.write("\n")
    print(f"wrote {len(titles)} titles to {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
