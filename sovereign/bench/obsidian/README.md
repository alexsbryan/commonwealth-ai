# Obsidian-vault bench eval

Bench harness for scoring Phase-1 atlas atom extraction against
Alex's real vault at `/Users/alexsbryan/Documents/Obsidian Vault`.

## Why this exists

`literary_atlas` was tuned on cleanly-authored long-form prose
(Brothers Karamazov, Dubliners). The root essays in Alex's vault
are different: argumentative non-fiction across institutional
economics, urban planning, AI investment, sports as designed
system, market design, music criticism — heterogeneous topics, heavy
on named institutions / mechanisms / dollar figures, often opening
with conversational scaffolding ("Based on our discussion…").

The bench measures where literary-style prompting falls down on
non-fiction vault prose.

## Pipeline pairing (2026-05-23 vault port)

The legacy `obsidian_atlas` pipeline was retired when live vault
chat moved to the tiered RAPTOR + GLiNER surface (see
`sovereign/docs/TIERED_RETRIEVAL.md` + `PROGRESSIVE_ENRICHMENT.md`).
The tiered pipeline emits per-note RAPTOR trees and `chunk_entities`
mentions instead of Phase-1 typed atoms; the 5 argumentative axes
(`mechanism`, `named_position`, `evidence`, `opposition`,
`concession`) drop to ~0 against the tiered output. v2 will add a
typed-extension pass over RAPTOR summaries that restores these axes.

**For bench scoring today**, pin the corpus to `literary_atlas`:
```bash
sovereign enrich init obsidian-vault --source "$VAULT" --pipeline literary_atlas --force
sovereign enrich build obsidian-vault
sovereign bench obsidian --report /tmp/r.json
```

**For live vault chat**, do nothing — registering the vault via
`LocalCorpusManager::register` + ingest routes through
`FolderTieredProvider` automatically. The bench corpus and the live
corpus are independent SQLite namespaces; running one does not
disturb the other.

## Files

- `golden.toml` — atlas-atom golden (consumed by
  `sovereign enrich eval`). Entries reference real entities,
  concepts, and events in the vault as of 2026-05-14. Sampled from
  ~10 root essays (Ostrom Summary, Joan Robinson, The Rules Are The
  Game, Pharmacy Benefit, AI Bullcase, Beyond GDP, Jane Jacobs Urban
  Ranking, LVT, Stock Buybacks, Incompleteness).
- `questions.toml` — Q/A retrieval bank (consumed by
  `sovereign eval run`). 12 questions across concept_lookup /
  argument_reconstruction / numerical_fact / cross_note_synthesis.

## Scope

The bench runs over **root essays only**. The vault's COMMONWEALTH/
subtree is work notes for this very codebase — circular to score
against it. `_sovereign-index/` is sovereign-managed output and
excluded by the walker globs anyway.

```
vault root      ← bench scope
COMMONWEALTH/   ← excluded ([meta].excluded_paths in golden.toml)
_sovereign-index/  ← walker excludes
.obsidian/      ← walker excludes
```

## Running

The bench command does NOT bake in the vault path — repo
artifacts stay portable. Two ways to point at the vault:

```bash
# 1. environment variable (recommended for repeated iteration)
export SOVEREIGN_OBSIDIAN_VAULT="/Users/alexsbryan/Documents/Obsidian Vault"
sovereign bench obsidian --report /tmp/obsidian-bench.json

# 2. flag (one-off, override env)
sovereign bench obsidian \
    --vault "/Users/alexsbryan/Documents/Obsidian Vault" \
    --report /tmp/obsidian-bench.json
```

The first run requires a `enrich init` + `enrich build` pass
against the corpus; the bench prints the commands if the corpus
isn't built yet.

## Authoring posture

This golden is grounded in **one author's vault**. The drift cost
is real — when Alex rewrites an essay enough that a golden entry
no longer holds, the entry comes out rather than being kept as
dead weight. Treat scores as comparable within a window of ~1 month
of golden authoring date (printed in `[meta].template`); past that,
revisit the entries.

The unconscious-author-bias risk (Alex authors both the vault and
the golden) is acknowledged but unmitigated in v1 — a true
held-out subset would require a peer-authored partial golden. v2
follow-up.

## What this bench does NOT measure (yet)

- **Frontmatter tag coverage.** `extract_stage.rs::extract_markdown`
  strips frontmatter before chunking. Tags / aliases never reach
  metadata today. Tracked as a separate gap.
- **Wikilink graph edges.** `follow_wiki_links: true` is declared
  on the vault config but no sidecar graph builder runs yet.
- **Cross-vault generalisation.** This golden is specific to Alex's
  vault content. Calibration mode against another vault requires a
  separate golden authored against that vault's notes.
