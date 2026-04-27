#!/usr/bin/env python3
"""
Build a pageview-rank CSV from Wikimedia's monthly pageview-complete
dump. Produces the gzipped two-column `title,rank` file shipped under
`corpus-engine/assets/pageview_ranks_YYYYMM.csv.gz` and referenced by
the Wikipedia recipe's pageview_rank filter.

This script is intentionally not run on every build — the dumps are
~50 GB and processing takes ~30 min. Regenerate when the Wikipedia HF
snapshot ships a new date and commit the resulting `.csv.gz` alongside
a recipe version bump.

Usage:
    python build_pageview_ranks.py \\
        --dump-dir ~/wikimedia/pageviews/202311 \\
        --out ../../corpus-engine/assets/pageview_ranks_202311.csv.gz \\
        --top 100000 \\
        --lang en

Inputs:
    --dump-dir   Directory with hourly `pageviews-YYYYMMDD-HH0000.gz`
                 files for one month (downloaded from
                 https://dumps.wikimedia.org/other/pageview_complete/).
    --out        Output `.csv.gz` path.
    --top        Keep the top N article titles (default 100_000). The
                 file is sorted by rank ascending; the 1-line CSV header
                 is `title,rank`.
    --lang       Wikipedia language code (default `en`). Filters the
                 hourly dump's first column.

Pipeline:
    1. For each hourly file, sum views per (lang, namespace=0,
       non-redirect) article title.
    2. Aggregate across the month.
    3. Rank by total views descending; emit top N as `title,rank`.
    4. Gzip-compress.

Notes:
    - Wikimedia's pageview format columns:
      `wiki_code article_title view_count bytes_returned`.
    - `wiki_code` for English is `en`; we filter on prefix.
    - Redirect detection: pageview-complete dumps include a column for
      content type — articles with `redirect=true` are excluded so
      view counts attribute to the canonical title only.
    - Title normalization: titles in the dump use underscores
      (`Albert_Einstein`). The Rust filter normalizes case + collapses
      underscores at load time, so we keep the source format
      verbatim — easier to diff against the upstream dump.

Author note:
    This is a placeholder skeleton. Real implementation reads the
    dump format, runs aggregation in chunks (the data does not fit in
    memory), and writes the output. Committed so the regeneration
    pipeline is documented and reviewable; not invoked at build time.
"""

import argparse
import csv
import gzip
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dump-dir", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--top", type=int, default=100_000)
    parser.add_argument("--lang", default="en")
    args = parser.parse_args()

    if not args.dump_dir.is_dir():
        print(f"error: --dump-dir {args.dump_dir} not a directory", file=sys.stderr)
        return 1

    print(
        f"Aggregating pageviews from {args.dump_dir} (lang={args.lang}, top={args.top})…",
        file=sys.stderr,
    )

    # Real pipeline goes here. The skeleton below documents the
    # expected shape so reviewers can see what the output looks like
    # before the real run; replace with the aggregation when needed.
    sample = [
        ("Main_Page", 1),
        ("Albert_Einstein", 2),
        ("Photosynthesis", 3),
    ]

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with gzip.open(args.out, "wt", encoding="utf-8", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(["title", "rank"])
        for title, rank in sample[: args.top]:
            writer.writerow([title, rank])
    print(f"wrote {args.out} (placeholder; replace with real aggregation)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
