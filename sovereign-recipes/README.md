# sovereign-recipes

Declarative corpus recipe definitions for [corpus-engine](../corpus-engine). Each recipe is a TOML file that tells the engine how to acquire, extract, chunk, index, and optionally enrich a knowledge base.

## Recipes

| ID | Name | Compressed | Indexed | Enrichment | License | Mesh Sharing |
|----|------|-----------|---------|------------|---------|--------------|
| `wikipedia` | Wikipedia (English) | 13 GB | 60 GB | Field model (multi-domain) | CC-BY-SA-4.0 | Yes |
| `sep` | Stanford Encyclopedia of Philosophy | 1.4 GB | 6 GB | Field model (philosophy) | Copyright Stanford (edu/research) | No |
| `stackexchange` | Stack Exchange Q&A | 85 GB | 120 GB | No | CC-BY-SA-4.0 | Yes |
| `openalex` | OpenAlex Scholarly Papers | 330 GB | 500 GB | No | CC0-1.0 | Yes |
| `gutenberg` | Project Gutenberg Books | 9 GB | 25 GB | No | Public Domain | Yes |
| `crs_reports` | CRS Reports | 2 GB | 5 GB | No | Public Domain | Yes |

## Structure

```
sovereign-recipes/
├── registry.toml              # Recipe catalog (schema_version 1)
├── wikipedia/recipe.toml      # Each corpus gets its own directory
├── sep/recipe.toml
├── stackexchange/recipe.toml
├── openalex/recipe.toml
├── gutenberg/recipe.toml
└── crs_reports/recipe.toml
```

## How recipes are consumed

`corpus-engine` fetches recipes through its `RecipeRegistry`:

1. A **bundled snapshot** (`corpus-engine/registry_snapshot.toml`) is compiled into the crate via `include_str!` so the engine works fully offline.
2. When online, `RecipeRegistry::refresh()` fetches the latest `registry.toml` from this repository. Each entry has a `toml_url` pointing to the raw recipe file on GitHub.
3. **Resolution order**: local override on disk -> fetch from `toml_url` -> error.
4. When the `sha256` field is non-empty, fetched recipes are verified before use.
5. `cargo xtask update-registry-snapshot` (in the corpus-engine repo) refreshes the bundled snapshot.

## Recipe TOML schema

Each recipe defines a pipeline:

```toml
[corpus]
id = "wikipedia"
name = "Wikipedia (English)"
license = "CC-BY-SA-4.0"
mesh_sharing = true

[acquire]
type = "huggingface_dataset"     # or bulk_download, local_file
dataset = "wikimedia/structured-wikipedia"
subset = "20240901.en"

[extract]
type = "wikipedia_jsonl"         # format-specific extractor
section_level = true

[chunk]
type = "paragraph"               # or sentence, fixed, semantic
max_chars = 1024
overlap_chars = 128

[index]
embedding_model = "nomic-embed-text-v2"
embedding_dimensions = 768

[enrichment]                     # optional
enabled = true
domain = "multi"                 # philosophy, science, policy, etc.

[update]                         # optional
manifest_url = "https://updates.sovereign.dev/manifests/wikipedia-en.json"
auto_update = true
```

## Contributing a new recipe

1. Create a directory: `<corpus-id>/recipe.toml`
2. Add an entry to `registry.toml` with metadata (id, name, description, license, sizes, toml_url)
3. Test the recipe: `sovereign recipe test ./<corpus-id>/recipe.toml --sample-size 50 --no-embed`
4. Include the generated `TEST_REPORT.md` in the directory
5. Open a pull request

See the [corpus-engine README](../corpus-engine/README.md#recipe-test-harness) for details on the test harness and pass criteria.

## License

Recipe files are configuration, not code. Each recipe's `license` field describes the license of the **source data**, not the recipe file itself. The recipe TOML files in this repository are Apache-2.0.
