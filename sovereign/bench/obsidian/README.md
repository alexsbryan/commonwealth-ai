# Obsidian-vault bench eval

Bench harness for scoring Phase-1 atlas atom extraction against
Alex's real vault at `/Users/user/Documents/Obsidian Vault`.

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
mentions instead of Phase-1 typed atoms — at the port the 5
argumentative axes (`mechanism`, `named_position`, `evidence`,
`opposition`, `concession`) briefly dropped to ~0 against the tiered
output.

**Shipped (v2, 2026-05-24, same push as the port):** the
typed-extension pass (`docs/specs/TYPED_EXTENSION_PASS.md`) runs at
the tail of every tiered build (`FolderTieredProvider::
finalize_corpus`) and writes a golden-compatible `atoms.json` —
Pass A extracts mechanism/named_position/evidence per RAPTOR leaf,
Pass B extracts opposition/concession per vault theme. (This
paragraph replaced a stale "v2 will add…" note on 2026-06-10 — the
pass had been shipped for two weeks while the README still promised
it.)

**First live-surface scoring (2026-06-11,
`--corpus watched-959ee8a8f330`, extraction of 2026-05-24, zero
prompt iterations) vs the literary_atlas-pinned 2026-06-07 baseline
(`baselines/golden/latest.json`):** mechanism 2/6 vs 4/6,
named_position 0/4 vs 3/4, evidence 1/5 vs 3/5, opposition 1/4 vs
2/4, concession 1/3 vs 2/3 — the pass restores the axes from 0 but
trails literary on every one; the headroom is prompt iteration
(`sovereign atlas typed-extension <corpus>` re-runs without a
rebuild). Caveat when reading the live run's *aggregate*: the
typed-extension `atoms.json` carries only its five kinds, so the
person/event/concept/question axes score 0 against it — that tier-2
signal lives in the SQLite sidecars (`chunk_entities`,
`conv_raptor_nodes`), which this golden's atoms reader doesn't see.
Compare per-axis, not aggregate, across the two surfaces.

**Score the live tiered surface** (what vault chat actually uses).
Corpus arguments accept the display name or any unique fragment —
not just the raw id (2026-06-11; ids themselves are now readable:
`obsidian-vault-959ee8a8f330`-style for new registrations):
```bash
sovereign bench obsidian --corpus "Obsidian Vault" --report /tmp/r.json
# re-run extraction after a prompt iteration, no rebuild needed
# (--force re-runs with unchanged inputs, for run-variance checks):
sovereign atlas typed-extension "Obsidian Vault"
```

**Or pin the legacy `literary_atlas` comparison surface**:
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
export SOVEREIGN_OBSIDIAN_VAULT="/Users/user/Documents/Obsidian Vault"
sovereign bench obsidian --report /tmp/obsidian-bench.json

# 2. flag (one-off, override env)
sovereign bench obsidian \
    --vault "/Users/user/Documents/Obsidian Vault" \
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
