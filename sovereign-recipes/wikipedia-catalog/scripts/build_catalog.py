#!/usr/bin/env python3
"""
Build the JSONL the corpus-engine `wikipedia_catalog` extractor consumes.

Two input sources are supported (the abstract dump was the original
plan; the structured-wikipedia ZIP is what we ended up using because
Wikimedia deprecated the abstract dumps):

  1. `--structured-zip <path>` (preferred) — reads
     `wikimedia/structured-wikipedia` JSONL shards out of the ZIP the
     `wikipedia` recipe already pulls (cached at
     `~/.sovereign/indexes/_downloads/wikipedia.zip`). Each shard
     record has:
       - `name`        → title
       - `url`         → article URL
       - `description` → short abstract (≈ Wikidata description)
       - `sections[]`  → table-of-contents
     Records that are pure REDIRECTs (single Abstract section with a
     list-item REDIRECT) are skipped.

  2. `--local-file <path>` / `--dump-url <url>` (legacy) — reads an
     `enwiki-latest-abstract.xml(.gz)` produced by the old Wikimedia
     `abstract-dump` job. Wikimedia stopped publishing this dump
     in early 2026, so this path only works against a pre-archived
     copy.

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
  # Recommended path — reuse the structured-wikipedia ZIP we already have:
  python build_catalog.py \\
      --structured-zip ~/.sovereign/indexes/_downloads/wikipedia.zip \\
      --out ../data/wikipedia_abstracts.jsonl.gz \\
      [--limit 100000]   # smoke-test mode — only emit first N articles

  # Legacy abstract-dump path (dump file no longer published):
  python build_catalog.py \\
      --local-file enwiki-latest-abstract.xml.gz \\
      --out ../data/wikipedia_abstracts.jsonl.gz

Environment:
  Set WIKI_USER_AGENT to your contact string per Wikimedia's policy
  (only matters for the legacy `--dump-url` fetch).
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
import zipfile
from typing import Iterator, Optional


DEFAULT_DUMP_URL = (
    "https://dumps.wikimedia.org/enwiki/latest/enwiki-latest-abstract.xml.gz"
)
DEFAULT_UA = (
    "sovereign-recipes/0.1 "
    "(https://github.com/your-org/sovereign-recipes; contact via PR)"
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


# ── structured-wikipedia ZIP path ───────────────────────────────────
#
# The ZIP ships one JSONL shard per chunk of mainspace
# (`enwiki_namespace_0_*.jsonl`). Each record has the structured
# schema documented at https://enterprise.wikimedia.com — for the
# catalog we only need title + url + description + section names.
#
# REDIRECT entries collapse to one Abstract section whose only
# `has_parts` is a `list_item` whose `value` starts with "REDIRECT ";
# we drop those so the catalog index doesn't waste embeddings on
# titles that don't have actual content.
SHARD_RE = re.compile(r"^enwiki_namespace_0_\d+\.jsonl$")


def iter_structured_zip(zip_path: str) -> Iterator[dict]:
    """Stream every mainspace article record from the structured-wikipedia ZIP.

    Yields the same `{title, url, abstract, sections}` shape that
    `iter_docs` produces from the abstract dump, so downstream
    `write_jsonl_gz` doesn't care which input path we took.
    """
    with zipfile.ZipFile(zip_path) as z:
        shard_names = sorted(
            n for n in z.namelist() if SHARD_RE.match(os.path.basename(n))
        )
        if not shard_names:
            raise RuntimeError(
                f"{zip_path}: no `enwiki_namespace_0_*.jsonl` shards found"
            )
        for shard in shard_names:
            print(f"  reading {shard}…", file=sys.stderr)
            with z.open(shard) as f:
                # Each line is a complete article JSON record.
                for line in f:
                    if not line.strip():
                        continue
                    try:
                        rec = json.loads(line)
                    except json.JSONDecodeError as e:
                        # Skip malformed lines rather than aborting
                        # the whole 17 GB run on one bad row.
                        print(
                            f"  warning: bad JSON in {shard}: {e}",
                            file=sys.stderr,
                        )
                        continue
                    title = (rec.get("name") or "").strip()
                    url = (rec.get("url") or "").strip()
                    if not title or not url:
                        continue
                    if _is_redirect(rec):
                        continue
                    abstract = (rec.get("description") or "").strip()
                    sections = _section_names(rec.get("sections") or [])
                    yield {
                        "title": title,
                        "url": url,
                        "abstract": abstract,
                        "sections": sections,
                    }


def _section_names(sections: list) -> list:
    """Pull readable section names, dropping the synthetic Abstract
    wrapper (its only purpose is to anchor the lead) and any sections
    without a name."""
    out = []
    for s in sections:
        name = (s.get("name") or "").strip()
        if not name or name == "Abstract":
            continue
        out.append(name)
    return out


def _is_redirect(rec: dict) -> bool:
    """A REDIRECT-only article has exactly one Abstract section whose
    body is a list with a single REDIRECT list_item. They have no
    real content — keeping them would waste an embed slot."""
    sections = rec.get("sections") or []
    if len(sections) != 1:
        return False
    s = sections[0]
    if (s.get("name") or "") != "Abstract":
        return False
    parts = s.get("has_parts") or []
    if len(parts) != 1:
        return False
    p = parts[0]
    if p.get("type") != "list":
        return False
    items = p.get("has_parts") or []
    if not items:
        return False
    first = items[0]
    val = (first.get("value") or "").strip()
    return val.upper().startswith("REDIRECT ")


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
    parser.add_argument(
        "--structured-zip",
        default=None,
        help=(
            "Use the wikimedia/structured-wikipedia ZIP "
            "(typically ~/.sovereign/indexes/_downloads/wikipedia.zip). "
            "Replaces both --dump-url and --local-file when set."
        ),
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

    if args.structured_zip:
        zip_path = os.path.expanduser(args.structured_zip)
        print(f"reading structured-wikipedia ZIP {zip_path}…", file=sys.stderr)
        docs = iter_structured_zip(zip_path)
    elif args.local_file:
        print(f"reading {args.local_file}…", file=sys.stderr)
        stream = stream_local(args.local_file)
        docs = iter_docs(stream)
    else:
        print(f"streaming {args.dump_url}…", file=sys.stderr)
        stream = stream_dump(args.dump_url, args.user_agent)
        docs = iter_docs(stream)
    n = write_jsonl_gz(args.out, docs, args.limit)
    size_mb = os.path.getsize(args.out) / (1024 * 1024)
    print(
        f"wrote {n:,} articles to {args.out} ({size_mb:.1f} MB)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
