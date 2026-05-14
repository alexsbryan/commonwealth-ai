# us-code

The codification of federal statutes — every active title of the
United States Code as a citable annual edition. Sourced from govinfo's
USCODE bulk data in USLM XML form
([uslm 1.x / 2.0 schemas](https://github.com/usgpo/uslm)).

## What this corpus is for

The statutory side of the legal-analysis stack. Pairs with
`federal-register-presidential` (the actions), future `olc-opinions`
(executive legal reasoning), and `scotus-opinions` (controlling
precedent) to let the legal-analysis skill answer "is this executive
action legal?" — by retrieving the operative statute alongside the
executive's reasoning and the courts' rulings.

## Scope

- **All 54 active titles** of the U.S. Code as the annual edition for
  `--param year=YYYY` (default `2024`). Some titles are reserved /
  repealed; govinfo ships placeholder packages for those so the recipe
  stays a single URL list.
- **Whole-section granularity.** The XML extractor emits one
  `ExtractedDoc` per `<section>` (e.g. `15 USC §1`), with the
  USLM `identifier` attribute as the title (e.g. `/us/usc/t15/s1`).
  Sub-section structure (subsections, paragraphs, clauses) is folded
  into the section's content; citation-level retrieval still
  resolves to the section.
- **One annual edition at a time.** Govinfo's USCODE is published as
  immutable year-end snapshots. Install multiple years side-by-side
  if a downstream consumer needs to ask "what did 15 USC §1 say when
  this OLC opinion was written in 2019?":
  ```bash
  sovereign corpus install us-code --param year=2019
  ```
  Each install lands as its own corpus directory.

Out of scope (intentionally):

- **The Constitution itself.** The ERD considered folding it in as
  "Title 0"; v1 skips this. The govinfo CDOC collection carries the
  text and is a v2 follow-on if/when a separate
  `constitution-of-the-united-states` recipe makes sense.
- **OLRC's continuously-updated USLM** (the current-text version
  published by the Office of Law Revision Counsel at
  uscode.house.gov/download). Govinfo's annual editions are
  citable; OLRC's continuous text is current but uncitable. A hybrid
  recipe (govinfo for stable editions + an OLRC currency-marker
  manifest) is v2.
- **Statutes-at-large** (Public Laws not yet codified into the Code).
  Recently enacted material lives in the Federal Register and in the
  Statutes at Large; not in this corpus.

## How to build it

```bash
sovereign daemon start
sovereign corpus install us-code                    # default: year=2024
sovereign corpus install us-code --param year=2023  # citable older edition
sovereign corpus status                              # watch progress
```

Wall-clock time:

- Acquisition: ~1 GB compressed across 54 ZIPs, dominated by Title 26
  (Internal Revenue Code, ~400 MB on its own). 10–30 min on broadband.
- Extraction + chunk + embed: ~150k sections × paragraph chunking →
  ~300k chunks. With enrichment off (the default), this is the embed
  cost only — roughly 15–45 min on typical hardware.

End-to-end: 30 min – 1 hr for a first build.

## Format details

- **USLM 1.x vs 2.0**: GPO and OLRC are mid-transition. The
  `xml_sections` extractor matches on element local-name (ignores
  namespace), so both schema versions round-trip through this recipe
  without recipe-side awareness. Year 2024 packages still ship USLM
  1.x in most titles; 2026+ will be USLM 2.0.
- **`<section>` is the canonical unit.** Some legal-tech setups treat
  sub-sections or paragraphs as the retrieval unit; section is
  coarser but matches how case law and OLC opinions cite statutes
  (citations are almost always `15 USC §1`, not `15 USC §1(b)(2)`).
- **Title 0 / Constitution**: not included (see Scope above).

## Known limitations

- **Section-only granularity.** Sub-section citations
  (`15 USC §78j(b)`) resolve to the whole `15 USC §78j` chunk. If
  retrieval quality on sub-section citations becomes a measured gap
  for the legal-analysis skill, swap in a finer-grained chunker on
  this recipe.
- **No delta updates.** The recipe re-downloads the full annual
  edition each install. Annual editions are immutable, so a quarterly
  re-install is the cleanest way to track the current edition.
- **Title 35 sometimes 404s.** Title 35 (Patents) is published every
  year but occasionally rolls out late. If a build fails on title 35,
  comment that line out of `[acquire].urls` and re-run.

## License

Federal works are not subject to copyright in the United States
(17 USC §105). The retrieved USLM text, metadata, and any derivative
index are public domain. Redistribution — including by publishing
the prebuilt snapshot to HuggingFace and by mesh-sharing this
corpus's chunks — is unrestricted.
