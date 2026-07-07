# The studio package boundary

`studio/crates/` holds the **workflow + recipe authoring package** — the stack a
third party could lift out of this monorepo and run against any host that speaks
the OICP contract (the OICP manifest + an OpenAI-compatible HTTP surface). For
that to stay true, the package must never reach back into the monolith. This
document is the contract; `cargo run -p xtask -- boundary-gate` enforces it in CI
(blocking).

## The two tiers

**Package crates** (`studio/crates/`):

| Crate | Role |
|---|---|
| `sovereign-tools-base` | The pure, leaf-dependency workflow tools (shell, web, chunk, section, file/json/csv/zip/vector, MCP client). |
| `sovereign-workflow` | The Step · Artifact · Runner engine. |
| `sovereign-workflow-host` | Registry assembly + in-process runner + shipped catalog + the `recipe:`-installer and workflow-author tools. |
| `sovereign-recipe-author` | The recipe-authoring tool bundle + `RecipeProject` model + its rusqlite project store. |
| `sovereign-studio` | *(arrives in B:P9e)* the headless authoring/run CLI. |

**Shared leaves** (repo root, siblings of `oicp-types`):

| Crate | Allowed internal deps |
|---|---|
| `oicp-types` | *(none)* — the wire vocabulary, a pure leaf. |
| `sovereign-contracts` | `oicp-types` only. |
| `oicp-client` | `sovereign-contracts`, `oicp-types`. |

## The rules

1. **A package crate may depend only on other package crates + the shared leaves.**
   No `sovereign-core`, `sovereign-tools`, `sovereign-inference`, `corpus-engine`,
   `sovereign-mesh`, … The corpus/atlas-backed tools that a workflow sometimes
   needs (`extract`, `corpus_store`, `corpus_search`, `atlas_gaps`,
   `atlas_tensions`) stay monolith-side and are injected at call sites via the
   runner's `extra_tools` slot — they are not package dependencies.
2. **The shared leaves keep the budget in the table above.** They are the tip of
   the DAG; widening one of them widens the whole package's contract surface, so
   the gate pins them by hand.
3. **No `build.rs` in any package or leaf crate**, and **no `include_str!` /
   `include_bytes!` that escapes the crate root** — the one exception is the
   checked-in `sovereign-recipes/` tree, which `sovereign-contracts` vendors as a
   typed `const`. A build script or an escaping embed is a source-tree reach-in no
   package boundary survives (it was killing the recipe-schema `syn` walk that
   B:P0 removed).

The rules count **dev- and build-dependencies too**: a crate a third party lifts
carries its tests and its build scripts, so those must respect the same budget.

## When the gate fails

It names the offending edge (`crate → dep`) or file. Either the dependency
doesn't belong in the package — move the code that needs it monolith-side and
inject through a seam (a trait in `sovereign-contracts`, or the runner's
`extra_tools`) — or, if a leaf genuinely must grow, widen `allowed_leaf_deps` in
`corpus-engine/xtask/src/main.rs` deliberately, with this table updated to match.
