# conversations-personal

Local-only corpus binding for the user's claude.ai chat-export dump.

## Why

Conversation history is one of the highest-leverage retrieval surfaces
the product offers — "what did I discuss with the CFO about runway in
Q3", "how has my view on X shifted over the last six months", "have I
ever talked about Y". This corpus is the canonical local fixture for
bench iteration on that surface. See
`sovereign/bench/conversation/README.md`.

## Setup

1. Download your data from claude.ai → Settings → Privacy → Export
   data. The zip will land in `~/Downloads/data-<uuid>-…/`.
2. Symlink the export to the stable path the recipe expects:
   ```bash
   mkdir -p ~/.sovereign/conversations
   ln -sf ~/Downloads/data-*/conversations.json \
          ~/.sovereign/conversations/conversations.json
   ```
3. Install + ingest:
   ```bash
   sovereign recipe install sovereign-recipes/conversations-personal
   sovereign corpus ingest conversations-personal
   ```

## Privacy contract

This recipe sets `mesh_sharing = false` and `license = "private"`.
The corpus is **never** advertised to mesh peers, **never** uploaded,
**never** included in shipping fixtures.

Bench artifacts derived from this corpus (question banks, baselines)
must be sanitized via `corpus_engine::pii::scrub_pii` before being
committed to the repo. The bench banks in
`sovereign/bench/conversation/` already enforce role-token entities
(`<cfo-acme>`, `<advisor-1>`) — the raw corpus remains the only place
real names live.

## Shape

- One `ExtractedDoc` per conversation (`source_id = conv_uuid`)
- Each doc rendered as `### [YYYY-MM-DD HH:MM] {user|assistant}` turn
  blocks, flattened by `created_at` (branch handling deferred to v2)
- Chunked as user-turn + assistant-reply pairs by the
  `threaded_turns` chunker. Per-span authorship available via the
  chunker's `AttributedChunk` surface for atlas + attribution-aware
  retrieval.
