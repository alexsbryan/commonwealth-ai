# GLiNER typing oracles — the P2.1 gate

Ground truth for `sovereign-gliner/examples/typing_audit.rs`, which runs
v1 and GLiNER2 over the same chunks **through the production seam**
(`LabeledEntityExtractor`) and asks whether they assign the same label.

## Why a separate golden

The enrichment-eval goldens (`bench/{obsidian,literary,philosophy}/`)
score the **atlas**, after Phase-1's LLM has judged the extractor's
output. That is the right instrument for "is the enriched corpus good"
and the wrong one for "does this NER model type things correctly" — the
LLM sits between the two and the signal does not survive it.

These files score the extractor directly, and only on the axis P2.1
makes a claim about: `Person`. Everything else is covered by the audit's
head-to-head table, which needs no ground truth at all and does not
assume v1 is right.

## Provenance

`typing_oracle_obsidian.json` is **transcribed**, entry by entry, from
`bench/obsidian/golden.toml`'s `[[expected_person_atoms]]` and
`[[forbidden_person_atoms]]` — a golden the operator already reviewed.
Each entry carries its `source`. Nothing here was invented alongside the
thing it grades.

`typing_oracle_sep.json` is philosopher surnames from the Stanford
Encyclopedia of Philosophy corpus. `BonJour` and `Sosa` lead the list
because they are the two names the 2026-08-02 eyeball found GLiNER2
typing as `Work`.

## Running it

Fixtures are JSONL with a `content`/`text`/`chunk` field, built by
`research/enrichment-spikes/scripts/dump_chunks.py`. They live under
that spike's gitignored `data/` — vault fixtures are the operator's
personal notes and are deliberately not committed.

```
cargo build --release -p sovereign-gliner \
    --features corpus-engine/treesitter --example typing_audit

./target/release/examples/typing_audit \
    --fixture research/enrichment-spikes/data/<fixture>.jsonl \
    --oracle  sovereign/bench/gliner/typing_oracle_sep.json \
    --out     research/enrichment-spikes/findings/typing_audit_<name>.json
```

## Read the mention row, not the entity row

The audit reports two accuracies and they can disagree completely:

- **entity level** — the dominant label per surface form.
- **mention level** — every row, which is what `chunk_entities` stores.

On 2026-08-03 the sep fixture scored v1 17/17 and GLiNER2 17/17 at the
entity level, and 99.7% vs **67.3%** at the mention level. The entity row
is an aggregate over a mention-level store; it is not a measurement of
that store. Note `f42cf7ec`.
