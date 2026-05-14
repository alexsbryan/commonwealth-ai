# federal-register-presidential

Presidential Documents (executive orders, proclamations, determinations,
memoranda, notices) and OIRA-significant Final Rules from the Federal
Register, sourced through the canonical
[federalregister.gov v1 API](https://www.federalregister.gov/developers/documentation/api/v1).

## What this corpus is for

The "what executive action was taken" surface for legal-analysis work.
Pairs with future statutory and case-law corpora (US Code, OLC opinions,
SCOTUS opinions) to let the legal-analysis skill answer questions like
"is this executive action legal?" — by retrieving the operative document
itself, the claimed legal authority, and the relevant interpretive case
law in one search.

## Scope

- **Presidential Documents** — every executive order, proclamation,
  determination, memorandum, and notice the Federal Register has
  published. The API's `conditions[type][]=PRESDOCU` filter is the
  canonical way to enumerate these (~8,500 across history).
- **OIRA-significant Final Rules** — the small fraction of Final Rules
  the Office of Information and Regulatory Affairs designates as
  "significant" under EO 12866. ~9,000 across history; vastly more
  representative of "rules that materially affect policy" than the
  unfiltered Final Rule firehose (which is dominated by routine
  technical adjustments).
- **Date range** — defaults to the last ~10 years (2016-01-01 to today).
  Override with `--param start_date=YYYY-MM-DD --param end_date=YYYY-MM-DD`
  at install time.

Out of scope (intentionally):

- Non-significant Final Rules. The unfiltered RULE firehose is dominated
  by routine technical adjustments and would dilute retrieval.
- Proposed Rules, Notices, Public Inspection documents. These rarely
  ship operative legal authority and are noise for the use case.
- Sub-regulatory guidance (agency interpretive letters, FAQs). Not
  published in the Federal Register.

## Source format

The recipe follows each document's `raw_text_url` — the GPO's
designed-for-access plain-text format, wrapped in a trivial HTML
envelope (`<html><body><pre>…</pre></body></html>`). The `html`
extractor's tag stripper removes the envelope; the underlying text is
the GPO's canonical plain-text dump, the same shape the GPO has shipped
for decades.

The API also exposes `full_text_xml_url` (FedReg's structured XML form
with `<PRESDOCU>`/`<DETERM>`/`<HD>` semantics). XML is structurally
cleaner; switching to it is a worthwhile follow-up once a generic XML
extractor with XPath selectors lands in `corpus-engine`. `body_html_url`
is explicitly rejected — that's display HTML with navigation chrome,
not a designed-for-access surface.

## How to build it

This corpus has **no `[prebuilt]` block yet** — no one has produced a
publishable snapshot. To build locally:

```bash
# 1. Start the daemon if it isn't running.
sovereign daemon start

# 2. Submit the install. The daemon owns the ingest task; the CLI is
#    a thin client.
sovereign corpus install federal-register-presidential

# 3. (Optional) Override the date range.
sovereign corpus install federal-register-presidential \
    --param start_date=2020-01-20 \
    --param end_date=2024-01-20

# 4. Watch progress.
sovereign corpus status
```

The ingest runs through the standard pipeline: acquire (paginated API
pulls + per-document follow) → extract (HTML wrapper stripped, plain
text emitted) → chunk (paragraph, 2048 char max) → embed (whatever
Embed slot is loaded) → index (LanceDB + Tantivy) → enrich (atlas
extraction via the `referential_atlas` pipeline).

Expect ~17,500 documents to acquire from the default 10-year window,
producing roughly 200k chunks at the default chunker setting. Wall-clock
time is dominated by:

- The acquisition rate limit (1 req/sec → ~18 min just for the search
  pages, plus ~10s of minutes for the per-document follow at
  concurrency 4).
- The enrichment phase (atlas LLM calls scale with chunk count).

Realistic end-to-end: 1–3 hours on a developer workstation with an
Embed slot loaded and the enrichment model warm. Tune `[parameters]`
to shrink the date range for a quicker first run.

## After the first build: publishing a `[prebuilt]` artifact

Once a build produces a clean index, use
`sovereign corpus snapshot publish federal-register-presidential` to
tar+zstd it, compute the SHA-256, and upload to HuggingFace under
`svrnmesh/federal-register-presidential`. Then update this recipe with:

```toml
[prebuilt]
hf_repo = "svrnmesh/federal-register-presidential"
hf_filename = "federal-register-presidential-<embed-model>-<date>.tar.zst"
sha256 = "<computed sha>"
compatible_embedding_model = "<embed-model-name>"
```

Future `sovereign corpus install federal-register-presidential` calls
will short-circuit to the snapshot download instead of re-pulling the
API. Cadence is open: weekly delta refreshes are appropriate, since the
Federal Register publishes daily.

## Known limitations

- **Document metadata is lossy through the plain-text envelope.** The
  API exposes rich per-document JSON (president, agencies, EO number,
  significance flag) but the current recipe only persists the rendered
  text. A follow-up could wire a sidecar JSON acquire step and propagate
  the metadata into `ExtractedDoc.metadata` for richer retrieval
  filtering.
- **Filtering of significant Rules trusts the API.** We pass
  `conditions[significant]=1`; the recipe does not double-check the
  flag at extract time. If OIRA's designation policy changes shape, the
  recipe will silently follow.
- **No author-side citation graph.** Atlas Phase 1 captures statutory
  references as Relation atoms; building a corpus-internal EO/proc
  citation graph is future enrichment work.

## License

Federal works are not subject to copyright in the United States
(17 USC §105). The retrieved text, metadata, and any derivative index
are public domain. Redistribution — including by publishing the
prebuilt snapshot to HuggingFace and by mesh-sharing this corpus's
chunks — is unrestricted.
