# UAP / Blue Book — recipe-author E2E demo

Drives the **recipe-author agent** end to end to author the `uap-blue-book`
corpus from `sovereign/docs/specs/UFO.md`, through the **real daemon Runtime
loop** (the same `handle_recipe_author_turn` the desktop chat uses) — not a
side-channel. Spec: `~/.claude/plans/crystalline-imagining-newell.md`.

Files here:
- `charter.md` — the partner's domain framing (what the corpus is).
- `script.txt` — scripted partner turns (frame → draft → validate → test → fix → checkpoint).
- `fixtures/cases.jsonl` — 14 synthetic Blue Book case-metadata records (Level 1).

## What was built (code)

- **Daemon Runtime can now drive recipe-author** (the "bypass"):
  - `POST /v1/conversations` accepts `{"skill_id":"recipe-author"}` →
    `seed_conversation` → `insert_empty_conversation` (`sovereign-server/src/routes.rs`,
    `sovereign-core/src/runtime.rs::seed_conversation`).
  - `POST /v1/conversations/:id/messages` routes through `handle_message_any`,
    which sends recipe-author conversations into the agent loop (drains
    `handle_message_stream`, whose `:1509` dispatch already exists) and leaves
    generic chat on the old path (`runtime.rs::handle_message_any`).
  - The daemon Runtime registers the recipe-author tool catalog
    (`sovereign-server/src/main.rs`), so the loop has a non-empty toolset.
- **Headless driver:** `sovereign recipe-agent live-trial --via-runtime` drives
  the turns over that conversation API (`recipe_agent_live_trial.rs`).

## Prerequisites for a live run

1. Rebuild so the daemon + CLI carry the changes, then restart the daemon
   (inside the `dev-toolbox` toolbox, per the daemon-restart note):
   ```
   cargo build --release --bins
   sovereign daemon restart
   ```
2. A capable **Primary / 35B chat model** must be loaded (recipe-author needs
   tool-calling discipline). Confirm: `curl -s localhost:9741/v1/models`.

## P0c — verify the daemon reaches the agent loop

```
CID=$(curl -s localhost:9741/v1/conversations -d '{"skill_id":"recipe-author"}' \
      -H 'content-type: application/json' | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])')
curl -s "localhost:9741/v1/conversations/$CID/messages" -H 'content-type: application/json' \
     -d '{"content":"Propose a recipe shape for a local JSONL corpus."}' | python3 -m json.tool
```
PASS = a substantive agent reply (not a `NotImplemented` / empty-catalog error),
and the daemon log shows `recipe_author_loop: dispatch begin` with `tools > 0`.

## L1 (P4) — the demo run + acceptance

```
sovereign recipe-agent live-trial --via-runtime \
  --charter sovereign/bench/uap/charter.md \
  --script  sovereign/bench/uap/script.txt \
  --title   "UAP Blue Book" \
  --sample-size 10
```
This provisions a RecipeProject, creates a `skill_id=recipe-author` conversation,
and feeds the scripted turns to the daemon's agent loop. The harness's post-trial
block then validates + test-extracts the authored recipe.

**Acceptance:**
1. The agent authored `~/.svrnmesh/recipes/uap-blue-book/recipe.toml`:
   `acquire.type = local_file` (the fixtures dir) · `extract.type = jsonl`
   (`content_field = narrative`) · `chunk.type = paragraph` · `enrichment.type =
   investigation` with the 7 entity types, 6 relationship types, and one
   `threshold` pattern (and **no** circular_flow / role_overlap).
2. `recipe validate` passes; `recipe test --sample-size 10` extracts ≥ 1 doc.
3. Investigation enrichment produces the graph:
   ```
   sovereign enrich investigation build uap-blue-book
   ```
   Then `~/.svrnmesh/.../investigation/{entities,relationships,pattern_findings}.json`
   carry the declared entity/relationship types, and the threshold finding fires
   for **Wright-Patterson AFB** (5 of the 14 cases occur near it; threshold > 3).

It's real inference → non-deterministic. Assert on structure (valid recipe, ≥1
doc, findings present), not exact wording. If the agent under-declares the
enrichment, tighten `script.txt` and re-run.

## L2 (P5) — described_asset PDF layer (separate recipe)

A recipe declares exactly one `[extract]`, so PDFs are a **second** recipe
(`uap-blue-book-scans`): `local_file` + `described_asset` over a small fixture of
Blue Book PDF scans + the same investigation block. Add `fixtures-pdf/` + a
`charter-pdf.md` / `script-pdf.txt` pair and run the same `--via-runtime` command
against them. Not yet built.

## L3 (P6) — real NARA / AWS-ODR pull (PLAN ONLY, not built)

Production acquisition for the full corpus:
- **Metadata** (catalog): `http_api` over the NARA Catalog API for Record Groups
  **RG 341** (Blue Book) and **RG 615** (modern), paginated; or `bulk_download`
  of the published index. Extract with `jsonl` / `column_aware`.
- **Documents** (content): `bulk_download` of the **Blue Book scans on AWS Open
  Data** + The Black Vault mirrors; a second `described_asset` recipe (the L2
  shape at scale).
- **Dual-format** is two recipes (or a `[catalog]` + content-recipe split, à la
  `wikipedia-catalog` + `wikipedia-article`) — never one `[extract]`.
- **Salience tiering** (UFO §Salience) gates the 48-hour build: cold tail → T1
  (metadata-searchable), hot set (~1.5k cases: eval-split ∪ unidentified ∪
  notable ∪ fresh) → T3 deep atlas. The ranker is engine work, **not** recipe-
  author output (see UFO landing map). Out of scope for this exercise.
