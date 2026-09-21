# Spike 3 — extraction census (fine print, adapted `contracts` ontology)

2026-09-19. Corpus id `spike-fineprint-census`. Numbers: `census.json`; 8 random examples per type: `census.txt`;
spot checks: `extra.txt`. Scripts: `build_corpus.py`, `census.py`, `sketch_census.py`.

## What ran

- Input: Reddit (8), Spotify (7), LinkedIn (7) = 22 .md files, 101,458 words. The `markdown` extractor reads ONE file,
  so `build_corpus.py` concatenates them into `fineprint.md` (`# <Service> — <Doc>`, own headings demoted).
- Recipe: `recipe.toml`, 94 changed lines vs the template (`diff | grep -c '^[<>]'`; about half comments/guidance).
  `sovereign recipe validate recipe.toml --offline` -> "Validation passed" (`validate.log`).
- Model: `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL` (daemon slot `primary`, from `~/.svrnmesh/logs/daemon.err`).
- Commands and wall clock:
  - `sovereign corpus install recipe.toml --wait=900` — 34 s, 699 chunks.
  - `sovereign enrich init spike-fineprint-census --from-corpus spike-fineprint-census` — <5 s, 449 chapters, pipeline `custom_atlas`.
  - 4-chapter probe: ~20 s/chapter -> 449 chapters ~2.5 h, over budget. Selected 108 chapters / 42,643 words: every
    Privacy + Trackers Policy chapter >= 40 words (77) and ToS chapters >= 250 words (31).
  - `sovereign enrich build ... --chapters <108>` — killed by me after 19.8 min at 28/108: 9 chapters failed with daemon
    `503 local_queue_full` (another session on the shared daemon) and the key chapters sorted last.
  - Dropped the 9 failure rows from the checkpoint (backup `checkpoint.before-resume.jsonl`), then
    `enrich extract --chapters <89, privacy-first> --resume` — 34.0 min, 88 ok / 1 failed (503); `enrich extract --finalize`;
    `enrich extract --retry-failed` — 37 s, 1 ok. Final: 108/108 sections extracted (~288k prompt + ~99k completion tokens for the 90).
  - `enrich build --skip extract`: cluster + name (33 clusters) + resolve ~9 min; killed during tensions-classify
    (994 candidate pairs = 994 LLM calls). Then `enrich build --skip extract --skip cluster --skip name --skip resolve --skip tensions`
    -> gaps (236), report, backfill, <1 min.

## Census (resolved atlas, 108 sections)

atoms 1,239: Entity 427, Claim 320, Question 174, State 154, Relation 149, Event 15.
edges 1,416: Involves 879, Grounds 474, Transition 63. No Tension edges (phase not run).
Declared type lives in `data.entity_type` (entities) / `data.claim_kind` (claims); declared attributes in `data.attributes`.

| declared type | phase-1 sketches | resolved atoms | attributes filled |
|---|---|---|---|
| data_type | 99 | 56 | service 46, source 54 |
| defined_term | 84 | 68 | definition 67, agreement 62 |
| organization | 70 | 57 | — |
| service | 57 | 26 | — |
| agreement | 54 | 35 | service 25 |
| recipient | 51 | 22 | service 16 |
| purpose | 25 | 24 | service 16 |
| party | 43 | **0** | — |
| obligation (claim) | 317 | 320 | deontic 8, valid 9, deadline 4; subject 272 |
| undeclared (person 83, work 23, concept 22, place 9, institution 2) | | 139 | |

Obligation deontic split: forbid 7, require 1, permit 0, **unset 312 (97.5%)**. Subject filled 272/320 (85%); 47 dropped as
`unresolved_claim_subject`. Subjects resolve to `organization` atoms ("user" 67, "LinkedIn" 45, "Spotify USA Inc." 39, "Reddit" 29,
"we" 24, "the user" 12, "you" 9): the party role never materialises as a type.

Side files: `ontology.json` — version 1, pipeline custom_atlas, 9 declared types, policies shape/assertion/identity/change/derivation/
prose/navigation. `schema_validation.json` — 105 sections, entity orphan fraction 56% (241/427), 97% of entities in the low-confidence
buckets. `resolution_failures.json` — 105 drops: unresolved_claim_subject 47, unresolved_relation_participant 39, unresolved_attribute_ref 18,
unresolved_claim_attribution 1. `_summary.json` is stale (atom_count 0, left from the install-time structural atlas).

Per-service yield (by `attributes.service`): data_type Spotify 16 / Reddit 17 / LinkedIn 13 / none 10; recipient 3 / 6 / 7 / none 6;
purpose 3 / 10 / 3 / none 8. Spot check against Spotify's own tables: data categories 10/10 by name; recipient categories 4/13 by
canonical name, 7/13 counting aliases.

## Failure modes

1. **Every claim is an "obligation".** The response schema makes `claim_kind` a required one-value enum, so 320/320 claims are typed
   obligation, including plain facts ("received 1,498,726 Right to Know requests in 2025").
2. **Deontic almost never filled (8/320).** It lives in `attributes.deontic`, but the prompt's claim example lists only `valid` and `deadline`.
3. **`party` vanishes** (43 sketches -> 0 atoms; `role_of = organization` folds it); "we"/"you"/"user"/"the user"/"Member(s)" stay unmerged.
4. **83 junk `person` atoms** minted for names that did not resolve ("Privacy Policy", "we", "you", "Age Check Data", "Spotify" x3); 40+
   normalized names exist more than once across types ("user" 7, "reddit" 6, "spotify" 5, "cookies" 5).
5. **Over-merge by head noun.** Recipient "partners" swallowed Authentication / Technical service / Payment / Advertising / Marketing partners
   as aliases; "cookies" swallowed 13 cookie kinds; defined_term "mature content" took Content / Your Content / User Content. This kills recipient recall.
6. **Section context is lost.** The phase-1 prompt carries only the section title ("1. Overview"), no parent heading, so the service is known
   only if the body names it: service attr missing on 10/56 data_types, 6/22 recipients, 8/24 purposes; 2 recipients carry the wrong service after
   a cross-service merge ("Courts and Authorities": first seen in Spotify, service = LinkedIn). 5 chapters merged same-titled sections from different documents.
7. **Coarse sections under-extract.** LinkedIn's privacy body is one 5,819-word chapter and yielded 7 entities; `entities_introduced` is capped at
   15 per section (Spotify "3. Personal data we collect" hit the cap).
8. **`service` is a grab-bag** (26): Google Chrome, iOS, Android, Privacy Center, "the Services", "paid Services" beside Reddit/Spotify/LinkedIn.
   Heading-like names: defined_term 9, agreement 6, service 3, data_type 3.
9. **Operational:** shared-daemon `503 local_queue_full` fails chapters with no retry inside `enrich build`; tensions-classify scales with candidate pairs (994 for 320 claims).

## Judgement

- "List every kind of data service X collects": **usable with caveats** where the policy is table/heading structured (Spotify 10/10, Reddit 17 plausible,
  `source` filled 54/56); weak for LinkedIn (one long section); 18% of data_types carry no service.
- "Who does X share data with": **not usable as is** — 22 atoms for three services, Spotify 3, recall 4/13 by name because distinct recipients merge into "partners"/"service providers".
- Must / must-not / may questions: not answerable from the deontic attribute; the claim text itself reads well.

## Not done / not verified

341 of 449 chapters not extracted (non-privacy documents, short ToS sections); tensions-classify and Tension edges; no precision labelling beyond the
random samples in `census.txt`; recall checked against Spotify only; single run, single model, no variance estimate. Created outside the work area (required by
install): `~/.svrnmesh/recipes/spike-fineprint-census/`, `~/.svrnmesh/indexes/spike-fineprint-census/`, `~/.svrnmesh/enrichment/spike-fineprint-census/`.
The `pod/` subdirectory here (corpus id `spike-fineprint-census-pod`, started 13:50) is NOT from this run — another session wrote it.
