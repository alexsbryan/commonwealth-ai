#!/usr/bin/env python3
"""
Convert Wikimedia's English-Wikipedia abstract dump into one JSONL
file the corpus-engine `wikipedia_catalog` extractor consumes.

Source:
  https://dumps.wikimedia.org/enwiki/latest/enwiki-latest-abstract.xml.gz
  (~1 GB compressed, ~6.8M articles, refreshed weekly)

Why an offline conversion step:
  - Parsing the XML in Rust would pull in another XML dep + a slower
    streaming parser; the abstract dump is published once a week, so
    a one-shot Python conversion is the cheaper engineering path.
  - The output JSONL is what the catalog corpus actually ships
    (or is fetched by recipe install), keeping the runtime ingest
    fast (no XML decoding on every install).

Output JSONL — one line per article:
  {
    "title": "Albert Einstein",
    "url": "https://en.wikipedia.org/wiki/Albert_Einstein",
    "abstract": "Albert Einstein was a German-born theoretical ...",
    "sections": ["Early life", "Career", "Personal life", ...]
  }

The `sections` list is the article's table-of-contents anchors —
useful for the catalog vector index because matching on a section
title gives the searcher a strong signal that the article covers
the queried sub-topic, not just contains the keyword.

Usage:
  python build_catalog.py \\
      --dump-url https://dumps.wikimedia.org/enwiki/latest/enwiki-latest-abstract.xml.gz \\
      --out ../data/wikipedia_abstracts.jsonl.gz \\
      [--limit 100000]   # smoke-test mode — only emit first N articles

Environment:
  Set WIKI_USER_AGENT to your contact string per Wikimedia's policy.
"""

import argparse
import gzip
import io
import os
import re
import sys
import urllib.request
import json
import xml.etree.ElementTree as ET
from typing import Iterator, Optional


DEFAULT_DUMP_URL = (
    "https://dumps.wikimedia.org/enwiki/latest/enwiki-latest-abstract.xml.gz"
)
DEFAULT_UA = (
    "sovereign-recipes/0.1 "
    "(https://github.com/alexsbryan/sovereign-recipes; contact via PR)"
)


def stream_dump(url: str, ua: str) -> Iterator[bytes]:
    """Stream the gzipped XML dump from `url`, yielding decompressed
    chunks. Single-pass (no temp file) so we don't need ~1 GB of
    spare disk just to land the archive."""
    req = urllib.request.Request(url, headers={"User-Agent": ua})
    with urllib.request.urlopen(req, timeout=120) as resp:
        with gzip.GzipFile(fileobj=resp) as gz:
            while True:
                buf = gz.read(1 << 20)  # 1 MiB
                if not buf:
                    return
                yield buf


def stream_local(path: str) -> Iterator[bytes]:
    """Stream a local `enwiki-latest-abstract.xml(.gz)` for offline
    development. Magic-byte sniff handles plain or gzipped."""
    with open(path, "rb") as f:
        magic = f.read(2)
        f.seek(0)
        if magic == b"\x1f\x8b":
            with gzip.GzipFile(fileobj=f) as gz:
                while True:
                    buf = gz.read(1 << 20)
                    if not buf:
                        return
                    yield buf
        else:
            while True:
                buf = f.read(1 << 20)
                if not buf:
                    return
                yield buf


# Each <doc> entry in the abstract dump looks like:
#   <doc>
#     <title>Wikipedia: Albert Einstein</title>
#     <url>https://en.wikipedia.org/wiki/Albert_Einstein</url>
#     <abstract>Albert Einstein (...) was a German-born physicist ...</abstract>
#     <links>
#       <sublink linktype="nav"><anchor>Early life</anchor>
#         <link>https://en.wikipedia.org/wiki/Albert_Einstein#Early_life</link>
#       </sublink>
#       ...
#     </links>
#   </doc>
#
# We use ElementTree's iterparse for streaming so the whole 6.8M-doc
# tree is never in memory at once.
TITLE_PREFIX = "Wikipedia: "


def iter_docs(stream: Iterator[bytes]) -> Iterator[dict]:
    """Stream-parse the XML dump, yielding one dict per <doc>."""
    # iterparse needs a file-like object; wrap the bytes iterator.
    class _ChunkReader(io.RawIOBase):
        def __init__(self, it):
            self._it = it
            self._buf = b""

        def readable(self):
            return True

        def readinto(self, b):
            while not self._buf:
                try:
                    self._buf = next(self._it)
                except StopIteration:
                    return 0
            n = min(len(b), len(self._buf))
            b[:n] = self._buf[:n]
            self._buf = self._buf[n:]
            return n

    reader = io.BufferedReader(_ChunkReader(stream))
    for event, elem in ET.iterparse(reader, events=("end",)):
        if elem.tag != "doc":
            continue
        title_el = elem.find("title")
        url_el = elem.find("url")
        abstract_el = elem.find("abstract")
        title = (title_el.text or "").strip() if title_el is not None else ""
        if title.startswith(TITLE_PREFIX):
            title = title[len(TITLE_PREFIX):]
        url = (url_el.text or "").strip() if url_el is not None else ""
        abstract = (abstract_el.text or "").strip() if abstract_el is not None else ""
        sections = []
        links_el = elem.find("links")
        if links_el is not None:
            for sub in links_el.iterfind("sublink"):
                anchor = sub.find("anchor")
                if anchor is not None and anchor.text:
                    sections.append(anchor.text.strip())
        # Drop degenerate entries (no title or no url).
        if title and url:
            yield {
                "title": title,
                "url": url,
                "abstract": abstract,
                "sections": sections,
            }
        # Free the parsed element so memory stays flat.
        elem.clear()


def write_jsonl_gz(out_path: str, docs: Iterator[dict], limit: int) -> int:
    """Write `docs` as gzipped JSONL. Returns count written."""
    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    n = 0
    with gzip.open(out_path, "wt", encoding="utf-8") as out:
        for doc in docs:
            out.write(json.dumps(doc, ensure_ascii=False, separators=(",", ":")))
            out.write("\n")
            n += 1
            if n % 100_000 == 0:
                print(f"  wrote {n:,} so far…", file=sys.stderr)
            if limit and n >= limit:
                break
    return n


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--dump-url",
        default=DEFAULT_DUMP_URL,
        help="Wikimedia abstract dump URL (default: latest enwiki).",
    )
    parser.add_argument(
        "--local-file",
        default=None,
        help="Use a previously-downloaded abstract dump instead of fetching.",
    )
    parser.add_argument("--out", required=True, help="Output JSONL.gz path.")
    parser.add_argument(
        "--limit",
        type=int,
        default=0,
        help="Stop after N articles (0 = no limit). Useful for smoke tests.",
    )
    parser.add_argument(
        "--user-agent",
        default=os.environ.get("WIKI_USER_AGENT", DEFAULT_UA),
    )
    args = parser.parse_args()

    if args.local_file:
        print(f"reading {args.local_file}…", file=sys.stderr)
        stream = stream_local(args.local_file)
    else:
        print(f"streaming {args.dump_url}…", file=sys.stderr)
        stream = stream_dump(args.dump_url, args.user_agent)
    n = write_jsonl_gz(args.out, iter_docs(stream), args.limit)
    size_mb = os.path.getsize(args.out) / (1024 * 1024)
    print(
        f"wrote {n:,} articles to {args.out} ({size_mb:.1f} MB)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
