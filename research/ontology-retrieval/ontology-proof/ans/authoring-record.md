# Authoring record — ei7-ans declaration (A1)

**Status: RECONSTRUCTED 2026-09-23 from git metadata, not contemporaneous.**
The pre-reg's A1 asks for a record committed BEFORE the bank was written. The
declaration was in fact authored and edited across two commits on 2026-09-20,
and the bank followed the same night; no separate record was cut at the time.
This file is the honest reconstruction, and the gap is recorded under the
pre-reg's Deviations rather than smoothed over.

| field | value |
|---|---|
| Declaration | `research/ontology-retrieval/ontology-proof/ans/recipe.toml` |
| Author | Alex Bryan (the git author on both commits) |
| Commits | `6423999ad` 2026-09-20T23:47:18-07:00 — "ANS corpus + frozen bank — 34 K1 hoard-contents lists, 15 K0, truth held out in IGCH"; `dfbaa81f5` 2026-09-20T23:59:33-07:00 — "ANS corpus cut to the nine-work subset — 32 K1 + 15 K0, bank.toml written" |
| Edit span | 12m15s between the two commits (the recordable lower bound; authoring thought time is not recoverable from git) |
| Declaration line count | 116 |
| Derived from | the numismatics template (`sovereign-recipes/_templates/ontology-v1/numismatics/recipe.toml`) plus: added `hoard`, `coin.hoard`; dropped `sceatta` |
| Types declared | `hoard` (entity: findspot, found, buried) · `coin` (entity: ruler ref, mint ref, hoard ref, denomination, metal, weight, struck) · `ruler` (role of person) · `mint` (entity) · `attribution` (claim: subject coin, proposed_date, grades die-link / hoard-context / stylistic / metrological) |
| `svrn recipe validate` output (2026-09-23, current recipe) | `✓ Validation passed` — identity keys: hoard, coin, ruler, mint → canonical name (defaults); question shapes: enumerate [hoard, coin, ruler, mint]; relations and events: none declared; aggregate: none |

**What this record cannot say.** The wall-clock authoring minutes the pre-reg
asks for are not recoverable — the two commits bound the EDITING, not the
thinking, and the pre-reg's "under an hour" claim (claim 3, flexibility)
therefore has no measured operand for this corpus. If authoring-time is ever
a demo claim, it needs a clock started before the first edit, which is a
study-2 instrumentation item.
