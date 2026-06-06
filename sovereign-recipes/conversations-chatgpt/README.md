# conversations-chatgpt

Local-only corpus binding for the user's ChatGPT (OpenAI) chat-export
dump. Drives the Settings → Imports tab in `sovereign-desktop` and is
the sibling of `conversations-anthropic` — both pair with
`conversation-history` (the user's Sovereign-internal chats) under the
Atlas View "Conversations" rail group via the shared
`[display] category = "conversation"` field.

## Why

Conversation history is one of the highest-leverage retrieval surfaces
the product offers — "what did I discuss about runway in Q3", "how has
my view on X shifted", "have I ever talked about Y". Many users have
years of ChatGPT history; importing it makes that history searchable
alongside their Claude chats with no source-specific retrieval code.

## Setup (desktop)

Settings → Imports → "Import ChatGPT export" → pick the export `.zip`
OpenAI shipped from Settings → Data controls → Export data. The desktop
unzips `conversations.json` into
`~/.sovereign/conversations-chatgpt/conversations.json` and triggers the
install automatically.

## Setup (CLI)

1. Export your data from ChatGPT → Settings → Data controls → Export
   data. OpenAI emails a download link; the zip lands in `~/Downloads/`.
2. Symlink the export to the stable path the recipe expects (note the
   **separate** directory — both vendors name the file
   `conversations.json`):
   ```bash
   mkdir -p ~/.sovereign/conversations-chatgpt
   ln -sf ~/Downloads/<chatgpt-export>/conversations.json \
          ~/.sovereign/conversations-chatgpt/conversations.json
   ```
3. Install + ingest:
   ```bash
   sovereign recipe install sovereign-recipes/conversations-chatgpt
   sovereign corpus ingest conversations-chatgpt
   ```

## Privacy contract

This recipe sets `mesh_sharing = false` and `license = "private"`.
The corpus is **never** advertised to mesh peers, **never** uploaded,
**never** included in shipping fixtures. Same contract as
`conversations-anthropic`.

## Shape

- One `ExtractedDoc` per conversation (`source_id = conversation_id`).
- ChatGPT stores messages as a `mapping` **tree**, not a flat list. The
  extractor reconstructs the current thread by walking `parent` pointers
  up from `current_node` (branch-correct — handles edited turns), then
  renders each turn as a `### [YYYY-MM-DD HH:MM] {user|assistant}` block
  — the *same* format the Anthropic extractor emits.
- Private-Use-Area inline markers (entity / url annotations) are cleaned
  to readable text; `system`/`tool` turns and reasoning/tool content
  types are dropped in v1.
- Chunked as user-turn + assistant-reply pairs by the `threaded_turns`
  chunker, sharing the conversational enrichment domain with
  `conversations-anthropic`.
