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

**Shared leaves** — crates outside `studio/` a package crate may still take.
What qualifies one is the CLOSURE, not the directory: most sit at the repo
root beside `oicp-types`, and `sovereign-time` lives under `sovereign/crates/`
with an empty dependency list, which is the same argument made by location
rather than by convention.

| Crate | Allowed internal deps |
|---|---|
| `oicp-types` | *(none)* — the wire vocabulary, a pure leaf. |
| `kernel-types` | *(none)* — identity + provenance. Empty by contract: a kernel that may name a product crate is not a kernel. |
| `corpus-engine-sections` | *(none)* — the section detectors. Third-party budget is `regex` + `tracing`, which is why it may cross at all. |
| `sovereign-contracts` | `oicp-types`, `kernel-types`. |
| `oicp-client` | `sovereign-contracts`, `oicp-types`. |
| `sovereign-time` | *(none)* — three wall-clock functions and an EMPTY `[dependencies]`. Admitted so `clock-gate`'s "one decider per island" rule and this boundary point the same way.  |

## The rules

1. **A package crate may depend only on other package crates + the shared leaves.**
   No `sovereign-core`, `sovereign-tools`, `sovereign-inference`, `corpus-engine`,
   `sovereign-mesh`, … Note `corpus-engine-sections` is a shared leaf and
   `corpus-engine` is not: the rule is about the CLOSURE, and the leaf's is
   `regex` + `tracing` while the engine's drags LanceDB, Tantivy and rusqlite. The corpus/atlas-backed tools that a workflow sometimes
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

   **And no RUNTIME reach-out past the crate root** (rule 3c, 2026-09-04). A
   path derived from `CARGO_MANIFEST_DIR` that then climbs out with `.parent()`
   or a `..` segment, or a `git` subprocess that never says which directory it
   means. It is the same escape with no compile-time literal to grep, so the
   two rules above were structurally blind to it: the commonwealth lift of
   2026-09-04 priced the blindness at 490 passed / 2 failed in a leaf's own
   tests, under a gate that stayed green throughout. Git run at a path the
   CALLER supplies is not flagged — the defect is deriving the path from where
   the crate happens to sit, not touching git.

The rules count **dev- and build-dependencies too**: a crate a third party lifts
carries its tests and its build scripts, so those must respect the same budget.

## When the gate fails

It names the offending edge (`crate → dep`) or file. Either the dependency
doesn't belong in the package — move the code that needs it monolith-side and
inject through a seam (a trait in `sovereign-contracts`, or the runner's
`extra_tools`) — or, if a leaf genuinely must grow, widen its `[[package_leaf]]`
budget in `quality/ARCH_LAYERS.toml` deliberately, with this table updated to
match.

Both tables above are DECLARED in `quality/ARCH_LAYERS.toml` (schema v3,
2026-09-03) rather than in the gate's Rust. They moved there when the gate
learned about N packages instead of just this one: it is policy, and it now
shares the parser (`quality/arch-layers`) with layer-gate and `arch_report` so
the three cannot drift on what a boundary means. The tables here are the prose
copy — the TOML is the source.
