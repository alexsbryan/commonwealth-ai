# NEEDS_HUMAN — fp-80 (the row's own file-count bar fires)

## (a) The unit

`- [~] fp-80 — depends [fp-79] — FLIP the daemon's store TYPES over the in-process backing (daemon src 1/3; census row 6; RESCOPED by five-programs-42 …): AppState's store-and-writer fields (state/store.rs inference_store, peer_preferences; state/node.rs activity_emitter) hold fp-78's port types, and StorePart gains the two ports … mesh_store/contribution_emitter … Files (10): state.rs, state/node.rs, state/serving.rs, state/store.rs, daemon.rs, bootstrap.rs, daemon_cmd/boot.rs, daemon_services.rs, venue_host.rs, newsworthy_host.rs. … A reader in fp-81/fp-82's files that stops compiling moves in only while the row stays at about ten files; past that, §6.`

No source file was edited. The only change in the tree is the row's `[~]` mark in STATE.md.

## (b) What I ran and found

Flipping the three EXISTING fields' types (`inference_store`, `peer_preferences`, `activity_emitter`) breaks every reader of them, and most readers are outside the ten files. The ports are async (`LedgerFut`, ledger_port.rs:45); the fields today are sync commonwealth-state writers, and the only sync surface fp-78 minted — `InferenceCache` (rails_client/ledger.rs:235) — answers `Result<_, NeverFilled>` for three readers plus `set_model_info`, so even the cache path changes each call site's shape.

```
git grep -n "\.\(inference_store\|peer_preferences\|activity_emitter\)\b" -- sovereign/crates/sovereign-daemon   minus the ten files
src (11 files, fp-81's and fp-82's):
  auto_ingest.rs:815
  routes_inference.rs:363,367,437,673,940,956,964,973,1189,1422
  routes_internal/corpus_collaborate.rs:136,1011
  routes_internal/corpus_ingest.rs:876,894
  routes_internal/corpus_queue.rs:114
  routes_internal/gossip.rs:182            (set_plan, sync)
  routes_internal/mesh_admin.rs:265,274,279 (set_model_info / list_models / remove_model_info, sync)
  routes_internal/peer_preference.rs:108,149,174
  routes_knowledge.rs:81
  routes_oicp.rs:195,261,265,276,381,505,521,536,561
  routes_status.rs:18,85,92
tests (4 files, fp-84's):
  src/routes_internal/mesh_admin/tests.rs:442,469,487
  src/tests/daemon.rs:417,432
  tests/main/models_http_e2e.rs:116,199
  tests/main/peer_preference_manifest.rs:133,184,213
```

So the type flip as written is a 10 + 11 + 4 = 25-file commit — it absorbs most of fp-81 and fp-82 and part of fp-84. The `From<Arc<MeshStore>>` bridge only covers construction; it does not keep method calls on the fields compiling.

The two NEW StorePart fields (`mesh_store` as `Arc<dyn ReplicatedKv>`, `contribution_emitter` as `ContributionLedgerPort`) are additive and do not break anything by themselves; moving the ten files' `fabric.*` readers onto them (daemon.rs:4049, state.rs:1622, venue_host.rs:65, newsworthy_host.rs:61,132) fits the ten files.

A second finding that any split has to answer: `get_llama_address`, `set_plan`, `remove_model_info`, `peer_preferences.get/list` and `activity_emitter.record` are called synchronously today, and the cache carries none of them. Each becomes an `.await` on the port (I did not verify that every one of the ~40 sites sits in an async context), and `list_models`/`get_local_embed_model` go through `InferenceCache`, which must be filled before the first request or `/v1/models` answers NeverFilled where it answers a list today — a behaviour change at a commit that promises none.

## (c) What the operator must decide

1. Split fp-80 (recommended): fp-80 becomes ADD the two StorePart port fields over `LocalLedger`/`MeshReplicatedKv` on Fabric's store, move the ten files' `fabric.mesh_store`/`fabric.contribution_emitter` readers onto them, mint the recording double and the bridge — no type change to the three existing fields. Each existing field then flips in one commit WITH its readers, one dimension per move (ARCH 2): `activity_emitter` (state/node.rs, state.rs, routes_inference.rs, corpus_ingest.rs — 4 files), `peer_preferences` (state/store.rs, state.rs, peer_preference.rs, routes_oicp.rs, peer_preference_manifest.rs — 5 files), `inference_store` (state/store.rs, state.rs, daemon.rs, auto_ingest.rs, routes_inference.rs, corpus_collaborate.rs, corpus_queue.rs, gossip.rs, mesh_admin.rs, mesh_admin/tests.rs, routes_knowledge.rs, routes_oicp.rs, routes_status.rs, tests/daemon.rs, models_http_e2e.rs — 15 files, over the cap, so it needs its own split, e.g. writers vs readers). fp-81/fp-82 shrink to their `fabric.*` readers.
2. Or accept fp-80 as a 25-file commit.
3. Either way: who fills `InferenceCache` before the first request under the in-process backing (boot.rs / daemon.rs start, one `refill().await`?), so `/v1/models` keeps answering a list at the type-flip commit. And whether the six sync call sites with no cache method (`get_llama_address` state.rs:1239, `set_plan` gossip.rs:182, `remove_model_info` mesh_admin.rs:279, `set_llama_address`, `peer_preferences.get` routes_oicp.rs:381, `activity_emitter.record` corpus_ingest.rs:876) go `.await` on the port, or the cache grows them.

(d) Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
