# NEEDS_HUMAN — fp-70 (premise check failed before any edit)

## (a) The row

`fp-70 — MOVE the media viewer id to ONE store` (ralph/next/five-programs/STATE.md:173, left `[~]`, uncommitted). The row's own gate: "Premise check first: rails and the daemon resolve the same data_dir for `house_dir_under` — if not, §6 with both paths."

They do not.

## (b) What I ran and found

Rails' data dir (the `daemon.node.data_dir` the poll passes to `house_dir_under`, commonwealth-rails/src/presence.rs:57):

```
commonwealth/crates/commonwealth-rails/src/config.rs:21:  pub const DATA_DIR_ENV: &str = "CW_RAILS_DIR";
commonwealth/crates/commonwealth-rails/src/config.rs:127-137: resolve_data_dir = --data-dir, else $CW_RAILS_DIR, else ~/.commonwealth-rails
```

The daemon's and the offer verb's root:

```
sovereign/crates/sovereign-cli-daemon/src/lib.rs:184:   let data_dir = sovereign_contracts::rebrand::svrnmesh_root();
sovereign/crates/sovereign-cli-mesh/src/mesh_media/offer.rs:167-170:
    let root = sovereign_contracts::rebrand::svrnmesh_root();
    let house_dir = commonwealth_media::house_dir_under(&root);
sovereign-contracts/src/rebrand.rs:160: svrnmesh_root = $SVRNMESH_DATA_DIR, else ~/.svrnmesh (legacy fallback)
```

So on a default install the two paths are:

- rails poll reads `~/.commonwealth-rails/secrets/media-house/{authorization,viewer_user}`
- offer writes `~/.svrnmesh/secrets/media-house/authorization` (and the viewer id to `~/.svrnmesh/config.toml [iroh] media_viewer_user`)

Nothing aligns them: `git grep CW_RAILS_DIR` outside ralph/ hits only rails itself and sovereign-daemon/src/rail_migration.rs, which deliberately mirrors rails' convention to MOVE the ring journals into rails' dir (§4 rule 1, one data directory one owner).

The regression is therefore wider than the row states: since fp-46 the rails poll misses the HOUSE credential as well as the viewer id, so `read_presence` returns `None` at the `house.is_empty()` arm before the viewer is even consulted. Writing `viewer_user` beside the house file in `svrnmesh_root` (the row's fix as spelled) would change nothing a user sees.

Also measured: the daemon-side copies now have no reader. `git grep -n "viewer_user()\|\.house()"` finds only the definitions in sovereign-mesh/src/media_route.rs:168,190; `set_viewer_user` is written at sovereign-daemon/src/daemon.rs:943 and :4297, and `read_credentials_in`'s house half at daemon.rs:944 / iroh_access.rs:730, but no poll in the daemon reads either since fp-46 moved it to rails.

## (c) What the operator must decide

1. Which directory owns the poll's inputs (house credential + viewer id). My recommendation: rails' dir, by the §4 rule-1 argument rail_migration.rs already applies — the poll is rails' and so are its inputs. `offer` writes both files under the rails data dir resolved by the same mirrored convention rail_migration.rs:36-44 uses (`$CW_RAILS_DIR`, else `~/.commonwealth-rails`), and `withdraw` removes the viewer file. The alternative — rails learning `svrnmesh_root` — needs a rails.toml key, and `MediaSection` is `deny_unknown_fields` (declared.rs:32-40 explains why that is a rollout hazard), or a crate edge from commonwealth-rails into sovereign-contracts across the lift boundary.
2. Whether the row's migration also moves an existing `~/.svrnmesh/secrets/media-house/authorization` into the rails dir (needed for nodes offered before this fix; the viewer-key migration alone leaves them dark), and where that one-time move runs — the offer verb, or daemon boot beside rail_migration.rs.
3. Whether the now-reader-less daemon state (`MediaRoute::viewer_user`/`set_viewer_user`, the `house` field/`set_house`/`house()`, and the `[iroh] media_viewer_user` reload diff at admin_http.rs:366) is deleted in this row or a follow-up. The row as written says MediaRoute should READ the viewer file, but nothing in the daemon consumes it.
4. Re-spell the row's tests accordingly: "offer then the rails poll's `read_credentials` sees house AND viewer, with rails and offer each resolving their own default roots".

## (d) To resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.

Tree state: no source edits; only STATE.md's row flipped to `[~]` (uncommitted). Nothing built, boundary count unchanged.
