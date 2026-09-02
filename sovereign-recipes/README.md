# sovereign-recipes

Declarative **corpus recipes** for the Sovereign knowledge engine. Each recipe is
one TOML file that tells the engine how to turn a data source into a searchable
corpus, through a single pipeline:

```
acquire → extract → filter → chunk → embed → index → (optional) enrich
```

No code — you pick an acquirer, extractor, and chunker from a fixed menu and
point them at your data.

## Start here

- **[GETTING_STARTED.md](./GETTING_STARTED.md)** — build your first recipe, end to end.
- **[SCHEMA.md](./SCHEMA.md)** — every section, key, allowed value, and default.
  Generated from the engine source (`corpus-engine/src/recipe.rs`) and gated by a
  test, so it never drifts.
- **[`_templates/annotated/recipe.toml`](./_templates/annotated/recipe.toml)** —
  a heavily commented file to copy as your starting point.

## This repo is the single source of truth

These recipes are the **only** authored copy. corpus-engine vendors this tree at
build time (`build.rs` → `OUT_DIR` → `include_str!`) to bundle an offline copy
into the binary and the desktop app — that bundle is a build artifact regenerated
from this tree on every build, so there is no second copy to keep in sync.

`registry.toml` is the catalog: one `[[recipes]]` entry per corpus, with metadata
and a `catalog_status` (`featured` | `preview` | `hidden`) that drives the desktop
picker. It is the authoritative list of what's in the catalog — browse it rather
than a hand-maintained table here.

```
sovereign-recipes/
├── registry.toml              # catalog (schema_version 1) — the source of truth
├── SCHEMA.md                  # generated field reference
├── GETTING_STARTED.md         # tutorial
├── _templates/                # copy-paste starting points
│   ├── annotated/             #   fully-commented general template
│   ├── narrative-markdown/    #   template for stable markdown docs
│   └── ontology-v1/           #   `svrn recipe new --ontology <name>` — declared-type recipes
│       ├── numismatics/       #     coins, mints, rulers, graded attributions
│       └── governance/        #     a charter + dated decisions, rules that supersede
├── wikipedia/recipe.toml      # one directory per corpus
├── sep/recipe.toml
└── …
```

## How a recipe is resolved at runtime

When you `sovereign corpus install <id>`, the engine takes the first hit:

1. `~/.svrnmesh/recipes/<id>/recipe.toml` — your local + local-only recipes. No
   network, no rebuild. Drop a file here and it just works.
2. `$SOVEREIGN_RECIPES_DIR/<id>/recipe.toml` — opt-in. Point it at a clone of this
   repo to hot-edit a catalog recipe and load it live.
3. The published catalog (`registry.toml` → each entry's `toml_url`), SHA-256
   verified when `sha256` is set.
4. The copy bundled into the binary (offline fallback).

## Contributing a recipe

1. Get it working locally first (see GETTING_STARTED.md) — recipes in
   `~/.svrnmesh/recipes/` need no rebuild.
2. Move the directory here: `sovereign-recipes/<id>/recipe.toml`.
3. Add a `[[recipes]]` entry to `registry.toml` (copy a neighbor; set `id`, `name`,
   `description`, `license`, sizes, `catalog_status`).
4. To ship inside the app's offline bundle, add the id to the `RecipeId` enum in
   `corpus-engine/src/recipe_builtin.rs`. The
   `bundled_recipe_covers_every_snapshot_entry` test flags anything you missed.
5. `sovereign recipe test <path> --sample-size 50 --output TEST_REPORT.md`, commit
   the report, open a PR.

Editing recipe fields? Run `UPDATE_RECIPE_SCHEMA=1 cargo test -p corpus-engine
--test main recipe_schema` to regenerate `SCHEMA.md`; CI fails if it's stale.

## License

Recipe files are configuration, not data. Each recipe's `license` field describes
the license of the **source data**; it does not relicense the data. The TOML files
in this repository are Apache-2.0.
