# scotus-opinions

Comprehensive Supreme Court opinions, 1791-present (~37k cases).
Sourced from [CourtListener](https://www.courtlistener.com/) (Free Law
Project), which consolidates the slip-opinion HTML scrape, govinfo's
USCOURTS-SCT bound volumes, the FLITE 1937-1975 historical set, and
Library of Congress pre-1937 material into a single deduplicated,
citation-graphed corpus.

## What this corpus is for

Controlling precedent on executive power. Pairs with `us-code`
(operative statutes), `olc-opinions` (executive legal reasoning), and
`federal-register-presidential` (the actions themselves) for the
legal-analysis stack.

## Two install paths

### End-user path (default — `[prebuilt]` from HuggingFace)

```bash
sovereign corpus install scotus-opinions
```

Downloads the maintainer-published snapshot from
`svrnmesh/scotus-opinions` on HuggingFace. Seconds, not minutes. No
API token. No rate limit. No CourtListener subscription. This is the
path 99% of users want.

(The `[prebuilt]` block in this recipe is unset until the maintainer
runs the first build — see "Maintainer path" below. Until then the
end-user install falls through to the maintainer path and fails on
the missing `api_token` parameter, which is the loud-failure mode we
want.)

### Maintainer path (one-time, comprehensive build)

```bash
# 1. Subscribe to CourtListener Membership Tier 2+ for one month
#       https://free.law/membership/
#    Tier 2 ($25/mo) gives 600 req/day — comfortable for the ~370
#    paginated requests this recipe issues. The subscription can be
#    cancelled after the build runs.
#
# 2. Build:
sovereign corpus install scotus-opinions --param api_token=<your-token>
#
# 3. Publish:
sovereign corpus snapshot publish scotus-opinions
#    Uploads the tar.zst to `svrnmesh/scotus-opinions` on HuggingFace
#    and prints the [prebuilt] block to paste into this recipe.
#
# 4. Cancel the CL subscription.
```

This is the one-and-done path. After the snapshot lands, every
subsequent end-user install rides the `[prebuilt]` download with no
CourtListener involvement at all.

## The rate-limit math (why a paid tier is needed for the build)

| Tier | Cost | Requests/day | Build feasible? |
|------|------|---|---|
| Free (authenticated) | $0 | 125 | ❌ would take 3 days at 1 req/sec |
| Membership tier 1 | $10/mo | 300 | ❌ doesn't fit ~370-request build |
| **Membership tier 2** | **$25/mo** | **600** | **✅ ~6 minutes** |
| Membership tier 3 | $50/mo | 1,000 | ✅ ~6 minutes |

Free-tier numbers per CourtListener's [REST API
docs](https://wiki.free.law/c/courtlistener/help/api/rest/v4/overview);
membership numbers from [free.law/membership](https://free.law/membership/).

The acquirer rate-limits itself to 1 req/sec; CourtListener allows
5/minute on free, so the per-minute and per-hour caps are nowhere
near the bottleneck. The daily cap is what makes a paid tier
necessary for the comprehensive build.

## Local slicing (skip the prebuilt + skip the subscription)

If you want a slice that fits CourtListener's free-tier daily cap,
scope by `start_date`:

```bash
# Free-tier-friendly: recent ~10 years, ~700 opinions, ~7 requests.
sovereign corpus install scotus-opinions \
    --param api_token=<your-token> \
    --param start_date=2015-01-01

# Even smaller: ~5 years, ~350 opinions, ~4 requests.
sovereign corpus install scotus-opinions \
    --param api_token=<your-token> \
    --param start_date=2020-01-01
```

This local slice replaces the prebuilt artifact entirely — useful for
testing the install pipeline or for a corpus scoped to a specific
era. It is NOT what the maintainer's published snapshot ships.

## Why CourtListener over the ERD's three-tier approach

The ERD called for stitching `supremecourt.gov` slip opinions, govinfo
USCOURTS bound volumes, and govinfo SCD historical into a single
corpus, plus building a citation-graph extractor from scratch.

Trade-off: federal-domain provenance vs. consolidated quality.

CourtListener:
- Has already done that stitching (and matched it against the Caselaw
  Access Project's Harvard-quality scans).
- Has already built the citation graph.
- Is a nonprofit (Free Law Project) with editorial accountability,
  used by major legal-tech projects and academia.
- Bulk data is Public Domain Mark certified.

The trade-off: if "federal-government-sourced" is read strictly as
"served from a federal domain," CourtListener is one redistribution
hop removed. The opinions THEMSELVES are federal works in the public
domain (17 USC §105); CourtListener is the redistribution mechanism.
The user-facing corpus card states this honestly.

## Pairing with other corpora

Once installed alongside the other three corpora, the runtime's
`search_corpus_indexes` fans queries across all four automatically.
The legal-analysis skill (a future PR) will be biased toward this
ordering for executive-action questions:

1. `federal-register-presidential` — what action was taken
2. `us-code` — what statute is being invoked
3. `olc-opinions` — the executive's own reasoning
4. `scotus-opinions` — what controlling precedent says

## Provenance & license

- **SCOTUS opinions themselves**: federal works, public domain
  (17 USC §105). Redistribution unrestricted.
- **CourtListener's bulk corpus**: Public Domain Mark; free to
  redistribute.
- **Mesh sharing**: enabled. Mesh peers can search this corpus
  remotely once any node has built (or downloaded) it.

## Known limitations

- **Titles are URLs, not case names.** v1 uses `absolute_url`. A
  cluster join to pick up `case_name`, `date_filed`, and `citation` is
  high-value follow-up — opinions are typically cited by case name,
  not URL.
- **Section structure flattened by paragraph chunker.** Slip-opinion
  syllabi, majority opinions, concurrences, and dissents all chunk at
  the same paragraph granularity. v2 should adopt a semantic chunker
  that respects opinion-section boundaries.
- **Citation graph not yet imported.** CourtListener pre-computes
  citation edges in their `opinions_cited` table. v1 ignores them; the
  atlas pipeline would re-extract from prose. v2 should fetch the
  citation table once at install time and inject the edges as Phase 1
  Relation atoms.

## v2: bulk-data path (zero-rate-limit, zero-subscription)

CourtListener publishes quarterly bulk dumps at
`https://com-courtlistener-storage.s3.us-west-2.amazonaws.com/bulk-data/`
— no auth, no rate limit, Public Domain Mark licensed. But: the
Opinions CSV is **51.7 GB compressed** across all 2,000+ courts (no
per-court split). Filtering to SCOTUS requires joining against the
dockets (4.7 GB) + opinion-clusters (2.3 GB) CSVs. Total ~60 GB
download. Realistic but not trivial — needs a CSV extractor with
join + filter capability that corpus-engine doesn't have today.

This is the eventual self-hostable build path that doesn't require a
CourtListener subscription. Until it lands, the maintainer-pays-for-
one-month-of-tier-2 model is the cheapest comprehensive build.
