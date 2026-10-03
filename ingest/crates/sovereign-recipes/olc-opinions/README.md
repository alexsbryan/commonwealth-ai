# olc-opinions

The Department of Justice Office of Legal Counsel's published opinions —
the executive branch's published legal reasoning. Sourced from
[CourtListener](https://www.courtlistener.com/), the open-data legal
project run by Free Law Project.

## What this corpus is for

The "what's the executive's own legal reasoning" surface for legal-analysis
work. Without OLC opinions, the "both sides" framing of a legal question
is structurally biased toward critics: the executive's published
arguments are invisible.

## Scope

- **DOJ-published OLC opinions** — the canonical ~1,400 opinions DOJ
  has published at justice.gov/olc/opinions over the decades.
- **FOIA-released OLC opinions** — opinions DOJ chose not to publish
  but that have been compelled out via FOIA. CourtListener aggregates
  the Knight First Amendment Institute's FOIA-release set into the
  same court corpus. This materially reduces the publication-set
  bias the ERD's known-limitations section flagged.
- **Date range**: comprehensive. CourtListener's OLC coverage starts
  in the 1940s (earliest published opinions) and runs to present.

Out of scope:

- **Classified OLC opinions.** Not in any public corpus.
- **OLC working drafts, marginalia.** Not published; only finalized
  opinions appear in the corpus.
- **Memoranda and informal advice.** Only published opinions; not the
  full universe of OLC's daily work.

## Why CourtListener instead of scraping justice.gov directly

Two reasons:

1. **Mechanical**: justice.gov/olc/opinions is a Drupal-paginated HTML
   listing. The recipe's `http_api` acquirer follows JSONPath
   selectors, not HTML/CSS selectors. Scraping it would require
   extending `http_api` with HTML-follow support (planned for v2) or
   writing a custom OLC acquirer.
2. **Substantive**: scraping the DOJ-published listing inherits its
   selection bias. CourtListener's OLC corpus folds in the
   FOIA-release set, which is the cleaner answer to the ERD's
   "non-random subset" concern.

If "federal-government-sourced" is read strictly as "served from a
federal domain," CourtListener doesn't qualify (it's a nonprofit
redistributor). But the underlying opinions ARE federal works in the
public domain (17 USC §105) — CourtListener is the redistribution
mechanism, not the source. The corpus card is honest about this
provenance.

## How to build it

```bash
# 1. Register for a CourtListener API token (free, no email
#    verification beyond a confirmation):
#       https://www.courtlistener.com/sign-in/
#
# 2. Install:
sovereign corpus install olc-opinions --param api_token=<your-token>

# 3. Watch progress:
sovereign corpus status
```

Wall-clock time:

- ~14 paginated requests (1,400 opinions / 100 per page) at 1 req/sec
  → ~15 seconds for acquisition.
- Extraction + chunking + embedding: ~1,400 opinions × ~30 chunks each
  = ~40k chunks. With enrichment off (the default), 5–10 min on
  typical hardware.

End-to-end: under 15 minutes for a first build.

## Provenance & license

- **OLC opinions themselves**: federal works, public domain
  (17 USC §105). Redistribution unrestricted.
- **CourtListener's bulk corpus**: "free of known copyright
  restrictions" with Public Domain Mark certification (per their
  [bulk data docs](https://wiki.free.law/c/courtlistener/help/api/bulk-data/bulk-legal-data)).
  Free to redistribute.
- **Mesh sharing**: enabled. Once a node builds this corpus, it can
  share search results with other mesh peers.

## Known limitations

- **Titles are URLs, not case names.** v1 uses `absolute_url` as the
  per-opinion title. Joining the `cluster` resource for human-readable
  `case_name` is a worthwhile follow-up (~50 extra API requests, a
  cluster cache, and recipe-side schema work to thread the join). For
  retrieval, the URL anchor is sufficient.
- **No PDF fallback.** CourtListener's `plain_text` field is empty for
  some older opinions where OCR hasn't run. Those are silently skipped
  rather than failing the install. v2 could chase
  `download_url` for those and run the PDF extractor we already wire.
- **Pre-2020 retrieval quality varies.** Older opinions have noisier
  OCR. CourtListener gradually re-OCRs as their pipeline improves;
  re-installing every few months picks up improvements.
- **Single-tier corpus.** All OLC opinions in one corpus, no
  per-administration subset. For "Trump-era OLC vs Biden-era OLC"
  retrieval framing, post-ingest filtering on `date_created` is the
  workable path; recipe-level scoping is v2.
