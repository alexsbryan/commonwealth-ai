<!-- ledger -->

**five-programs-33 · 2026-09-24 · fp-70 · director** — this commit
- Needed: fp-70's premise check failed. cw-rails reads the poll's inputs from its own data dir (~/.commonwealth-rails), while `offer` writes them under ~/.svrnmesh. The poll misses the house credential as well as the viewer id, so the row's fix as written would not have restored presence.
- Chose: rails' dir owns both inputs. One decider for rails' default dir goes in commonwealth-media, and rail_migration's mirror calls it. `offer` writes the house and viewer files there. A one-time boot migration beside `migrate_journals_to_rails` moves the old house file and the old config key. The row's "MediaRoute reads the file" arm is struck. Deleting the reader-less daemon state becomes its own row, fp-71.
- Because: the poll belongs to rails, so its inputs are rails' too (§4 rule 1, the precedent rail_migration.rs already set for journals). The alternative, rails learning svrnmesh_root, needs a new rails.toml key under `deny_unknown_fields` or a crate edge across the lift boundary. commonwealth-media is already a dependency of all three callers, so no edge is added. Measured at boundary-gate 54.

<!-- appendix -->

## five-programs-33 · 2026-09-24 — fp-70: the presence poll's inputs move to cw-rails' data dir; dead daemon state split to fp-71

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp70-20260924.md. Reproduced at ba7f63627:

- commonwealth-rails/src/presence.rs:57 reads `house_dir_under(&daemon.node.data_dir)`. config.rs:127-137 resolves that dir as `--data-dir`, else `$CW_RAILS_DIR`, else `~/.commonwealth-rails`.
- sovereign-cli-mesh/src/mesh_media/offer.rs:167-170 writes under `sovereign_contracts::rebrand::svrnmesh_root()`. offer.rs:36 writes `[iroh] media_viewer_user` into config.toml.
- `git grep CW_RAILS_DIR` / `--data-dir` outside ralph/ finds no launcher that points rails at the svrnmesh dir. The only mirror of rails' convention is sovereign-daemon/src/rail_migration.rs.
- `git grep '\.viewer_user\b|\.viewer_user(|\.house(|set_house'` hits only media_route.rs itself (:145, :152, :184, :191). The daemon holds that state and reads none of it.
- Cargo.toml: commonwealth-rails, sovereign-cli-mesh and sovereign-daemon each already depend on commonwealth-media. None of the three shares another crate that could host the dir decider (sovereign-daemon has commonwealth-rail-core, not commonwealth-rail).

The worker's four questions and the answers: (1) the owning dir is rails', decided from principle 12 and §4 rule 1. (2) The migration runs at daemon boot beside the journal move, never overwrites, and is logged, because a node offered before the fix will not re-run `offer`. (3) The deletion is fp-71, one dimension per move. (4) The tests are respelled in the row.

The behaviour change stays within the row's stated intent, which is that the documented offer flow publishes presence again. A rails key or a crate edge would have been a wider change, so this charter-covered fork is decided here and not escalated.

Falsifier: a production launcher does start cw-rails with `--data-dir`/`CW_RAILS_DIR` set to the svrnmesh root (a unit file, a script, or desktop spawning outside this repo). In that case the two dirs already agree on real installs, and the move is churn. Also: fp-71's premise check finds a daemon reader of `viewer_user()`/`house()`.

Gate at decision: boundary-gate 54 violation(s).

</details>
