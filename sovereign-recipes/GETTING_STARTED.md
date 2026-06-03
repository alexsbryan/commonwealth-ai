# Build your first recipe

A **recipe** is a single TOML file that tells the corpus engine how to turn a
data source into a searchable knowledge corpus. It declares one pipeline:

```
acquire → extract → filter → chunk → embed → index → (optional) enrich
```

You don't write code. You pick an acquirer, an extractor, and a chunker from a
fixed menu, point them at your data, and run the corpus engine. This guide takes
you from an idea to a queryable corpus.

> **Field reference:** every section, key, allowed value, and default is in
> [`SCHEMA.md`](./SCHEMA.md) — generated directly from the engine's source, so
> it's never out of date. Keep it open in a tab while you author.

---

## 0. Prerequisites

- A working `sovereign` CLI (`sovereign --version`). If `sovereign` isn't on your
  PATH, see the repo root README for the symlink one-liner.
- The Sovereign daemon running for the embed/index steps: `sovereign daemon start`.
- Your data source: a URL to a dump, a HuggingFace dataset, a local folder, or a
  REST API.

---

## 1. Two ways to use a recipe

| | **Local-only** (just for you) | **Contribute** (ship to everyone) |
|---|---|---|
| Where it lives | `~/.sovereign/recipes/<id>/recipe.toml` | `sovereign-recipes/<id>/recipe.toml` (this repo) |
| How it loads | picked up automatically, first | vendored into the app + served from the catalog |
| Rebuild needed? | **No** — edit the file, re-run | No for you; ships to others on release |
| Published? | Never. Private to your machine. | Yes, via a pull request |

Start local. You can promote a working recipe to the catalog later — the file is
identical.

### The resolution order (where a recipe is loaded from)

When you `corpus install <id>`, the engine looks in this order and stops at the
first hit:

1. `~/.sovereign/recipes/<id>/recipe.toml` — **your local recipes.** No network,
   no rebuild. This is the fast path and the home for local-only recipes.
2. `$SOVEREIGN_RECIPES_DIR/<id>/recipe.toml` — opt-in. Point this at your clone
   of `sovereign-recipes/` to hot-edit a *catalog* recipe and load it live:
   ```bash
   export SOVEREIGN_RECIPES_DIR=~/dev/commonwealth-ai/sovereign-recipes
   ```
3. The published catalog (`registry.toml` → each entry's `toml_url`).
4. The copy bundled into the binary at build time (offline fallback).

---

## 2. Author the recipe

Copy a template to start — don't write from a blank file:

```bash
mkdir -p ~/.sovereign/recipes/my-corpus
cp sovereign-recipes/_templates/annotated/recipe.toml \
   ~/.sovereign/recipes/my-corpus/recipe.toml
```

Open it and fill in the four required blocks. Every option is in
[`SCHEMA.md`](./SCHEMA.md); the common shape is:

```toml
[corpus]
id   = "my-corpus"              # must match the directory name
name = "My Corpus"
description = "One sentence a stranger could understand."
license = "CC-BY-4.0"           # license of the SOURCE DATA
mesh_sharing = false            # may peers replicate this index? (respect the license)

[acquire]
type = "bulk_download"          # see SCHEMA.md → AcquirerConfig for all types
url  = "https://example.org/dump.jsonl.gz"
resume = true

[extract]
type = "jsonl"                  # match your data's format (jsonl, parquet, html, email, …)
content_field = "text"
title_field   = "title"

[chunk]
type = "paragraph"              # paragraph | sentence | fixed | semantic | passthrough
max_chars = 2048
overlap_chars = 256

[index]
fts = true
vector = true                   # embedding_model/dimensions auto-detect from the loaded model
```

**Picking the pieces** (full menus in `SCHEMA.md`):

- **`[acquire]`** — `bulk_download` (a URL/dump), `huggingface_dataset`,
  `local_file` (a folder you already have), `http_api` (paginated REST), `web_crawl`.
- **`[extract]`** — match your bytes: `jsonl`, `parquet`, `csv`, `html`,
  `html_sections`, `email`, `markdown`, `plaintext`, `wikipedia_jsonl`, … Each
  has its own fields (e.g. `content_field`, `content_column`).
- **`[chunk]`** — `paragraph` is the right default. Use `passthrough` to keep
  documents whole, `semantic` for embedding-aware splits.
- **`[[filter]]`** (optional, repeatable) — drop documents before chunking
  (`boilerplate`, `title_list`, `pageview_rank`, `knowledge_density`).
- **`[enrichment]`** (optional, advanced) — build an atlas over the corpus. Leave
  it off for your first recipe.

---

## 3. Validate (no download)

```bash
sovereign recipe validate ~/.sovereign/recipes/my-corpus/recipe.toml
```

This parses the TOML, checks every field against the schema, and validates
regexes and parameter declarations — without touching the network. Fix anything
it reports. Exit code `0` means the shape is sound.

---

## 4. Test on a sample

```bash
sovereign recipe test ~/.sovereign/recipes/my-corpus/recipe.toml \
  --sample-size 50 \
  --output ~/.sovereign/recipes/my-corpus/TEST_REPORT.md
```

This actually runs acquire → extract → filter → chunk on the first ~50 items and
writes a Markdown report: how many documents survived each stage, sample chunks,
and any warnings. It's the fastest way to see whether your extractor/chunker
choices produce sensible text. Iterate here until the chunks look right.

- `--params key=value[,…]` / `--params-file <json>` supply values for any
  `[parameters]` your recipe declares.
- `--offline` skips the live registry refresh.

---

## 5. Build the full corpus

```bash
sovereign corpus install my-corpus
```

Runs the whole pipeline and embeds + indexes every chunk (daemon must be
running). The index lands at `~/.sovereign/indexes/my-corpus/`. Then query it:

```bash
sovereign chat            # retrieval now includes your corpus
```

---

## 6. (Optional) Enrich into an atlas

If your recipe has an `[enrichment]` block:

```bash
sovereign enrich build --corpus-id my-corpus
```

This runs the atlas phases (extract → cluster → name → resolve → tensions →
gaps) and produces `atoms.json` / `edges.json` under
`~/.sovereign/enrichment/my-corpus/`. Enrichment is LLM-heavy — start without it.

---

## 7. Contribute it to the catalog

When a local recipe earns its keep, promote it:

1. Move the directory into this repo: `sovereign-recipes/my-corpus/recipe.toml`.
2. Add a `[[recipes]]` entry to [`registry.toml`](./registry.toml) — copy a
   nearby entry and set `id`, `name`, `description`, `license`, the size
   estimates, and `catalog_status` (`featured` | `preview` | `hidden`).
3. If the recipe should ship inside the app's offline bundle, add it to the
   `RecipeId` enum in `corpus-engine/src/recipe_builtin.rs` (the
   `bundled_recipe_covers_every_snapshot_entry` test tells you if you missed a
   step).
4. `sovereign recipe test … --output TEST_REPORT.md` and commit the report.
5. Open a pull request.

`sovereign recipe publish <path>` does step 1–2 into your **local** registry
(`~/.sovereign/recipes/registry.toml`) for testing; add `--submit-pr` to draft
the upstream PR via `gh`.

---

## Troubleshooting

- **`validate` fails with "unknown variant"** — a `type = "…"` value isn't in the
  menu. Check the relevant enum in `SCHEMA.md` for the exact strings.
- **`test` extracts 0 documents** — wrong extractor for the format, or wrong
  field names (`content_field` / `content_column`). Re-read your data's shape.
- **Chunks are huge or tiny** — tune `[chunk] max_chars` / `overlap_chars`.
- **`corpus install` can't embed** — the daemon isn't running
  (`sovereign daemon start`) or no embedding model is loaded.
