# `sovereign/bench/conversation/` — conversation-history retrieval bank

> **STATUS: SCAFFOLD — do not read a number from this bank as coverage.**
> Verified 2026-08-04. The questions are still the authored stubs
> (`questions.toml:12-14` says so itself) and `expected_sources` are
> placeholders like `"conv:runway-discussion"`. Worse, the committed
> `baselines/questions-synth/latest.json` was captured against corpus
> `conversations-personal` while the bank today declares
> `conversations-anthropic`, with retrieved items scoring 0.0 against
> unrelated content — so a diff against it is meaningless in both
> directions.
>
> The practical consequence: **conversation retrieval has no working
> gate anywhere in this repo** (note `d2af7720`). The sibling
> `conversation-private/` bank has real questions but is gitignored
> wholesale, so it can never carry a shareable baseline. Populating
> this bank per the derive-from-corpus flow below, then adding its
> corpus to a `sovereign-ci-bench.sh` retrieval lane, is what would
> close that.

Bench coverage for the conversation-history retrieval surface — the use
case where the user asks "what did I discuss with the CFO about runway
in Q3", "how has my view on X shifted", "have I ever talked about Y".
Corresponds to the `conversations-anthropic` recipe; questions are
authored against the user's own claude.ai export (local-only, never
published — see `sovereign-recipes/conversations-anthropic/README.md`).

## Privacy contract

Every entity in `questions.toml` is role-tokenized
(`<cfo-acme>` / `<advisor-1>` / `<vendor-billing>`) — the bench file
contains zero real names. The mapping from real entities to role
tokens lives in `~/.sovereign/conversations/entity-map.json`
(gitignored) and is derived from the corpus's atlas atoms.json,
which the obsidian_atlas pipeline produces during enrichment. Dates
are relative buckets (`q3-2025`, `~6mo-ago`) rather than exact ISO
dates where the date itself would identify a third-party interaction.

If a question cannot be authored without leaking a real name, it does
not belong in this bank. Use the local-only debug logs (`target/`)
instead.

## Derive-from-corpus flow

The bench is meant to be derived from your own ingested corpus, not
authored cold. End-to-end:

```bash
# 1. Symlink the claude.ai export
mkdir -p ~/.sovereign/conversations
ln -sf ~/Downloads/data-*/conversations.json \
       ~/.sovereign/conversations/conversations.json

# 2. Install + ingest (recipe has obsidian_atlas enrichment enabled)
sovereign recipe install sovereign-recipes/conversations-anthropic
sovereign corpus install conversations-anthropic
# Wait for enrichment to complete (LLM extraction over every conv).

# 3. Surface classified Person/Org entities from atoms.json
sovereign corpus scrub conversations-anthropic --min-salience 0.3
# → ~/.sovereign/conversations/entity-candidates.json
# (Ranked by salience. The atlas pipeline already classified types.)

# 4. (Manual review) Curate candidates into entity-map.json. The
#    candidates file is the input; entity-map.json is the curated
#    output that survives into apply mode. EntityMap JSON shape is
#    documented at corpus-engine/src/pii.rs.

# 5. Sanitize this bench file against your curated map
sovereign corpus scrub --apply-to sovereign/bench/conversation/questions.toml \
                       --map ~/.sovereign/conversations/entity-map.json
# Creates .bak; rewrites tokens in place. The scrubbed TOML is what
# goes to source control — the entity map never does.

# 6. Run the bench
sovereign bench all --filter conversation/questions --synth
```

## Six question archetypes

| Archetype | Question shape | What it probes |
|---|---|---|
| **entity_recall** | "What did I discuss with `<entity>` about `<topic>`?" | Cross-conversation aggregation around a known person/org. |
| **decision_trace** | "When did I decide `<X>` and why?" | Temporal grounding + causal-chain reconstruction across turns. |
| **trend** | "How has my view on `<topic>` shifted?" | Multi-conversation diff over time. Hardest class — exposes whether retrieval can compose chronological evidence. |
| **cross_conv_synth** | "Summarize my thinking about `<topic>` across past chats." | Breadth recall; tolerates lower per-source fidelity in exchange for coverage. |
| **negative** | "Have I ever discussed `<X>`?" (where X is constructed-not-in-corpus, via `EntityMap::unmapped_person`) | False-positive resistance. Conversation history is where hallucinated retrieval bites hardest — answer must be "no" or "I don't have a record". |
| **temporal_slice** | "What was on my mind in `<month-bucket>`?" | Temporal-only retrieval (no entity anchor). Tests that `created_at` filtering survives the embed→FTS→atlas hop. |

## Attribution-aware question classes

Conversation chunks carry per-span authorship (see
`corpus-engine/src/chunkers/threaded_turns.rs::AttributedChunk`). Three
attribution modes are valid for any archetype above:

- **user** — retrieval restricted to spans the user authored
  (`Attribution::User`). Best for "what was *I* trying to figure
  out about X" questions where assistant verbiage would inflate
  precision.
- **assistant** — restricted to model-generated spans. "What answer
  did the model give about X."
- **both** — full chunk, attribution surfaced to synthesis prompt so
  it can compose sources by author. The default.

Each question declares `attribution_mode = "user"|"assistant"|"both"`.
Scoring must not give semantic-equivalence credit between
user-authored and assistant-authored spans — a model's restatement of
a user's question is not the same retrieval event as the user
*asking* it.

## Bench loop

This bank wires into `sovereign bench all --synth`. See
`sovereign/bench/README.md` for the cross-corpus matrix +
per-corpus baseline conventions. Baselines for this bank live under
`baselines/questions/` and (for `--synth` mode)
`baselines/questions-synth/`.

```bash
sovereign bench all --filter conversation/questions
sovereign bench all --filter conversation/questions --synth
sovereign bench all --filter conversation/questions --update-baseline
```

## Scaffold status (2026-05-16)

This bank is a **stub** — the recipe + ingest path are wired
(corpus-engine `anthropic_export` extractor + `threaded_turns`
chunker), but the question set below is a hand-authored seed of ~12
items meant to exercise every archetype × attribution combination
once. Expand toward 50-100 questions after one full `--synth` baseline
pass exposes which archetypes have noisy scoring vs. solid signal.
