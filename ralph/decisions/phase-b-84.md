<!-- ledger -->

**phase-b-84 · 2026-10-01 · pb-mesh-exit-mesh · seat, amending phase-b-83 ruling (2) only** — this commit
- Needed: phase-b-83 (2) has the note-author roster read `members[].name` from svrn's `mesh.json` through `sovereign_contracts::node_identity`. Nothing has written that file since the flip. ~/.svrnmesh/meshes/27ba81666633874f720d14d8b88cbe11/mesh.json was last written 2026-09-30 20:42 PDT, the cutover's daemon stop. cw-rails writes its own ~/.commonwealth-rails/mesh.json (21:00 PDT and moving), and `git grep mesh.json` finds no svrn-side writer.
- Chose: names come through the daemon's `MembershipReader`. That is cw-rails' roster, injected as `FabricSeed.membership` (sovereign-mesh fabric.rs:68), whose `MemberDto` carries `name` (sovereign-contracts membership.rs:35). With no answer from it, names degrade to raw ids under the existing named warn (bootstrap.rs:51). `node_identity` gains no `members[].name`. Rulings (1) and (3)-(6) stand.
- Because:
  - Principle 6: a snapshot frozen at the flip would render every member who joins later as a raw id, with nothing saying why.
  - Principle 8: phase-b-80 makes cw-rails' roster the node's one roster, and the daemon already holds it through the port. A second read of a copy is the thing phase-b-80 refused.
  - Principle 11: the port exists and carries the field, so this is no new surface.
- Finding, recorded for phase-c rather than this row: sovereign-serve fetch_model.rs:220 `collect_peer_internal_urls` reads `sovereign_root()/mesh.json`. That path does not exist on this host; the file lives under meshes/<id>/ since before the flip. It also dials the members' `:9742` addresses, which the flip retired. So `svrn mesh fetch-model`'s peer discovery was already broken before the flip. It belongs on cw-rails' roster and serve's registered model-files origin.
- REVIEW-AFTER: pb-mesh-exit-mesh's landing. Falsified if the membership reader is not reachable at the point bootstrap builds the roster, so that names could not come from it at that moment. In that case the roster builds lazily from the reader, never from the file.
