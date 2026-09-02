# Where we are — ontology-v1

**~45%** `████▌░░░░░` · `318dd5281` · 11574 tests / 0 fail · arch-gate clean · 2026-09-02

Declare an ontology in a recipe's TOML and have it survive end to end — extraction,
resolution, retrieval, desktop — with no Rust written by the author.

## Landed

- **P1** — `[enrichment.ontology] version = 1` → policies, language registry, `recipe new` / `recipe migrate`
- **P2 · P2b** — prompt-byte pins; policy-aware parser, schema generator, `CustomOntology`, projection
- **P7** — refs resolve against base kinds; eight templates; content-judged descriptor; interview; validation card

## In flight

- **P0** — crate split, so the daemon stops depending on the whole CLI crate. Structurally landed; lint red on 4 crate-boundary visibility errors.

## Ahead

- **P3** — derivation. Blocks P4/P5/P6. Inherits a known `TypeIndex`-vs-`validate` inconsistency.
- **Stop-rule measurement** — the first time a real model runs against a declared ontology. Bar and three-arm design pre-registered.
- **P4 · P5 · P6** — the rest of wave 3.
- **`--quick` bench** — once, at the very end of the program.

## Watch

- The stop rule is **unmeasured**. Everything so far proves the *pipeline*, not the model.
- `resolution.rs` is at 5217 lines against a 5223 ceiling.
- P0.4b — no daemon route reaches `enrich_now` for an installed recipe corpus without folder ingest.

---

*Hand-maintained. Update the percentage and move one bullet when something lands —
if an update takes longer than that, it belongs in the commit message, not here.
Orders live in `.sovereign/features/ontology-v1-p*/order.md` (local, gitignored).*
