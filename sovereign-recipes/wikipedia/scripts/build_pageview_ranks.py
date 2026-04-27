#!/usr/bin/env python3
"""
Aggregate Wikimedia pageview hourly dumps for one calendar day and
emit the top-N en.wikipedia mainspace articles by total views, in the
format the corpus-engine `pageview_rank` filter expects:

    title,rank
    Main_Page,1
    Albert_Einstein,2
    ...

The hourly dumps live at `dumps.wikimedia.org/other/pageviews/` —
24 files per day, ~50–80 MB each compressed. We download, decompress,
filter to `en` (English Wikipedia desktop) + `en.m` (mobile), sum
across the day, sort descending, and gzip the top N.

Why one day, not one month: the head of the pageview distribution
is dominated by stable popular articles whose rank rarely shifts day
to day. A single day is more than sufficient signal to identify the
top-100K — the same articles surface week after week. Aggregating
a full month would buy slightly cleaner ordering at the long-tail
boundary but costs ~30× the bandwidth (1.2 GB → 36 GB) for no
meaningful retrieval-quality gain.

Output is gzipped so the bundled binary stays small (~600 KB
compressed for 100K entries, vs ~2.5 MB plain).

Usage:
    python build_pageview_ranks.py \\
        --date 2023-11-01 \\
        --out ../../../corpus-engine/assets/pageview_ranks_202311.csv.gz \\
        --top 100000

Notes:
    - Format spec: `domain_code page_title count_views total_response_size`
      where `domain_code` is e.g. `en` (en desktop), `en.m` (en mobile).
    - We collapse desktop + mobile into a single per-title sum since
      they're the same article from a search-quality perspective.
    - Title format: underscored, percent-encoded, no leading/trailing
      whitespace. The corpus-engine `PageviewRankFilter::from_csv_bytes`
      decoder normalizes case + collapses underscores at load time, so
      we keep the upstream format verbatim.
    - Filtering: skip titles starting with `Special:`, `File:`,
      `Template:`, `Category:`, `Wikipedia:`, etc. (non-mainspace).
      Also skip `Main_Page` if you want — leaving it in is fine, it
      maps to a real article.
    - Resumable across re-runs: the temp dir keeps decompressed
      hourly files until aggregation is complete, so a network blip
      mid-run resumes from where it stopped.
"""

import argparse
import csv
import gzip
import io
import os
import re
import sys
import time
import urllib.request
from collections import defaultdict
from datetime import datetime, timedelta
from typing import Dict


DUMP_BASE = "https://dumps.wikimedia.org/other/pageviews"

# Non-mainspace prefixes that show up in pageview dumps. We drop these
# because they're not articles a chat-grounded search would want to
# retrieve. Talk: pages, user pages, file pages, etc.
NON_MAINSPACE_PREFIXES = (
    "Special:",
    "File:",
    "Image:",
    "Template:",
    "Category:",
    "Help:",
    "Wikipedia:",
    "Portal:",
    "Book:",
    "Draft:",
    "Module:",
    "MediaWiki:",
    "User:",
    "User_talk:",
    "Talk:",
    "File_talk:",
    "Template_talk:",
    "Category_talk:",
    "Help_talk:",
    "Wikipedia_talk:",
    "Portal_talk:",
    "Book_talk:",
    "Draft_talk:",
    "Module_talk:",
)


def http_download(url: str, dest: str, ua: str, max_retries: int = 5) -> None:
    """Download `url` to `dest` with retry on transient errors."""
    if os.path.exists(dest) and os.path.getsize(dest) > 0:
        # Resumability: if the file already exists, trust it. The dump
        # files are immutable (Wikimedia archives by date), so a
        # truncated half-download is the only failure mode and we
        # detect it by re-running the parse later. Worst case the
        # operator deletes the temp dir and re-runs.
        return
    delay = 1.0
    for attempt in range(max_retries):
        req = urllib.request.Request(url, headers={"User-Agent": ua})
        try:
            with urllib.request.urlopen(req, timeout=120) as resp, open(dest, "wb") as f:
                while True:
                    chunk = resp.read(1 << 20)  # 1 MiB
                    if not chunk:
                        break
                    f.write(chunk)
            return
        except Exception as e:
            if attempt + 1 == max_retries:
                raise
            print(f"  retry {attempt + 1}/{max_retries} after error: {e}", file=sys.stderr)
            time.sleep(delay)
            delay *= 2


def hour_url(date: datetime, hour: int) -> tuple[str, str]:
    """Return (download_url, filename) for the hourly dump."""
    yyyy = date.strftime("%Y")
    yyyymm = date.strftime("%Y-%m")
    yyyymmdd = date.strftime("%Y%m%d")
    fname = f"pageviews-{yyyymmdd}-{hour:02d}0000.gz"
    url = f"{DUMP_BASE}/{yyyy}/{yyyymm}/{fname}"
    return url, fname


# Pageview line format: `domain_code page_title count_views total_response_size`
# Split on space; page_title may itself contain spaces but in practice
# is underscored, so the standard splitter works. Defensive against
# malformed lines in the dumps.
LINE_RE = re.compile(rb"^(\S+)\s+(\S+)\s+(\d+)\s+(\d+)\s*$")


def aggregate_hour(path: str, sums: Dict[str, int]) -> tuple[int, int]:
    """Stream-decompress `path` and add per-title view counts for
    en.wikipedia mainspace into `sums`. Returns (total_lines,
    accepted_lines) for progress reporting."""
    total = 0
    accepted = 0
    with gzip.open(path, "rb") as f:
        for raw in f:
            total += 1
            m = LINE_RE.match(raw)
            if not m:
                continue
            domain = m.group(1).decode("ascii", errors="replace")
            # `en` = desktop, `en.m` = mobile, `en.zero` = obsolete
            # zero-rated mobile (rare, fold in for completeness).
            if domain not in ("en", "en.m", "en.zero"):
                continue
            try:
                title = m.group(2).decode("utf-8")
            except UnicodeDecodeError:
                continue
            if any(title.startswith(p) for p in NON_MAINSPACE_PREFIXES):
                continue
            try:
                count = int(m.group(3))
            except ValueError:
                continue
            sums[title] += count
            accepted += 1
    return total, accepted


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--date", required=True, help="Day to aggregate, YYYY-MM-DD")
    parser.add_argument("--out", required=True, help="Output .csv.gz path")
    parser.add_argument("--top", type=int, default=100_000, help="Top N titles to keep")
    parser.add_argument(
        "--hours",
        type=int,
        default=24,
        help="Number of hourly files to aggregate (default 24 = full day). Lower = faster but noisier ranks.",
    )
    parser.add_argument(
        "--tmp-dir",
        default=None,
        help="Where to keep the downloaded hourly files. Default: <system temp>/pageview_ranks_<date>",
    )
    parser.add_argument(
        "--user-agent",
        default=os.environ.get(
            "WIKI_USER_AGENT",
            "sovereign-recipes/0.1 (https://github.com/alexsbryan/sovereign-recipes; contact via PR)",
        ),
    )
    parser.add_argument(
        "--keep-tmp",
        action="store_true",
        help="Don't delete the temp dir on success (useful for debugging or repeated runs).",
    )
    args = parser.parse_args()

    try:
        target_date = datetime.strptime(args.date, "%Y-%m-%d")
    except ValueError as e:
        print(f"--date must be YYYY-MM-DD, got '{args.date}': {e}", file=sys.stderr)
        return 2

    if args.hours < 1 or args.hours > 24:
        print("--hours must be in [1, 24]", file=sys.stderr)
        return 2

    tmp = args.tmp_dir or os.path.join(
        os.environ.get("TMPDIR", "/tmp"),
        f"pageview_ranks_{target_date.strftime('%Y%m%d')}",
    )
    os.makedirs(tmp, exist_ok=True)
    print(f"temp dir: {tmp}", file=sys.stderr)

    # Step 1: download every hourly file we need. Sequential so we
    # don't hammer dumps.wikimedia.org — they don't rate-limit us
    # explicitly but a polite single-stream is the right neighbour
    # behaviour.
    paths = []
    for h in range(args.hours):
        url, fname = hour_url(target_date, h)
        path = os.path.join(tmp, fname)
        size = os.path.getsize(path) if os.path.exists(path) else 0
        if size > 0:
            print(f"  [{h + 1}/{args.hours}] cached {fname} ({size // 1024} KB)", file=sys.stderr)
        else:
            print(f"  [{h + 1}/{args.hours}] downloading {fname}", file=sys.stderr)
            http_download(url, path, args.user_agent)
            print(f"    {os.path.getsize(path) // 1024} KB", file=sys.stderr)
        paths.append(path)

    # Step 2: stream each file and aggregate. Single dict in memory;
    # ~6.7M unique titles × ~100 bytes per entry ≈ 700 MB peak — fine
    # on a dev machine. If memory becomes a concern we'd swap to
    # SQLite-backed counting.
    sums: Dict[str, int] = defaultdict(int)
    grand_total = 0
    grand_accepted = 0
    for i, p in enumerate(paths):
        print(f"aggregating {os.path.basename(p)} ({i + 1}/{len(paths)})", file=sys.stderr)
        total, accepted = aggregate_hour(p, sums)
        grand_total += total
        grand_accepted += accepted
        print(
            f"   lines={total:,} en-mainspace={accepted:,} unique-titles={len(sums):,}",
            file=sys.stderr,
        )

    # Step 3: rank.
    print(f"sorting {len(sums):,} titles by views…", file=sys.stderr)
    ranked = sorted(sums.items(), key=lambda kv: kv[1], reverse=True)
    keep = ranked[: args.top]

    # Step 4: write gzipped CSV.
    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)
    with gzip.open(args.out, "wt", encoding="utf-8", compresslevel=9, newline="") as f:
        f.write("# Top-N en.wikipedia mainspace pageview ranks\n")
        f.write(f"# date={args.date} hours={args.hours} total_lines={grand_total} accepted_lines={grand_accepted}\n")
        f.write("# Regenerate via sovereign-recipes/wikipedia/scripts/build_pageview_ranks.py.\n")
        writer = csv.writer(f)
        writer.writerow(["title", "rank"])
        for rank, (title, _count) in enumerate(keep, start=1):
            writer.writerow([title, rank])
    print(
        f"wrote {len(keep):,} entries to {args.out} ({os.path.getsize(args.out) // 1024} KB)",
        file=sys.stderr,
    )

    # Optional: clean up temp dir.
    if not args.keep_tmp:
        for p in paths:
            try:
                os.remove(p)
            except OSError:
                pass
        try:
            os.rmdir(tmp)
        except OSError:
            pass

    return 0


if __name__ == "__main__":
    sys.exit(main())
