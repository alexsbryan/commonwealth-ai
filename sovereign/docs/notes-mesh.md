# NoteStore mesh propagation

Status: shipped 2026-05-25 (v9 schema; T1 + propagation surface).
Owner crate: `corpus-engine-notes`.
Spec: [`specs/NOTES_TIERED.md`](specs/NOTES_TIERED.md).

## What propagates

| Scope | Private | Tombstone | Goes on the wire? |
|---|---|---|---|
| `global` | `false` | `false` | yes — `app_id="notes"` |
| `global` | `false` | `true` | yes — tombstone event, peers apply |
| `global` | `true` | any | no — `app_id="notes-private"` is structurally gossip-excluded |
| `session` | any | any | no — node-local |
| `feature` | any | any | no — node-local |

Privacy is enforced at two layers: the NoteStore write path picks
the right `app_id` per the per-note flag, and
`GOSSIP_EXCLUDED_APP_IDS` in `commonwealth-state` keeps any record
written under `notes-private` from ever entering an outbound
gossip frame. Either layer alone would be sufficient; both
together pin the invariant with two independent test guards.

## What the wire carries — the note, not the vector

Changed 2026-08-13 (order `mesh-scale-t1-notes`). A gossiped note used
to carry its T1 embedding: 1024 dims → a 4,096-byte blob → a JSON array
of 4,096 decimal integers, **14.5 KB against a 1.5 KB note body**. At a
measured mean of 16.1 KB per event the 8 MiB request-body limit
(`server.rs`) landed at **~520 notes** — a full-store push to a fresh
peer stopped working somewhere in the fifth hundred. It was also wrong:
a peer running a different embed model shipped vectors from a foreign
space, and the receiver's cosine read scored them against local query
vectors as if they were comparable.

Now:

- `NotePropagationEvent.embedding` serializes as `null`
  **unconditionally** — the serializer ignores what the field holds, so
  no future constructor can put a vector back on the wire. Measured
  after the change on the same real store: **1,548-1,591 bytes/note**
  (n=500 / n=100), cliff **~5,300-5,400 notes**, a 10.4× improvement.
- `ingest_remote_notes` DISCARDS any vector a sender did ship and
  re-embeds the note's content through this node's own `embed_fn`, in
  the local model space, outside the connection mutex. It does this
  even when the sender labels the vector with our own model id: that
  label is a field the sender supplies.
- If the local embed hook is down, the note is stored **without** an
  embedding row — readable by keyword, excluded from the cosine pool —
  until `backfill_tier_artifacts` picks it up at the next daemon start.
  Never blended unembedded, never dropped. The ingest poller warns when
  its deferred count is non-zero.
- The cosine read admits only rows whose `model_id` matches this node's
  embed model, which is what covers rows a store already held in the
  old shape.

**Mixed mesh, both directions, indefinitely.** A peer on the pre-strip
build still sends a populated `embedding`; we decode it and throw the
vector away. And we still write the `embedding` key (as `null`) rather
than omitting it, because that peer's struct has no serde default for
it and a missing key is a decode error there. No schema break either
way, and no version gate to remove later.

## Identity — `content_hash`, not `note_id`

Every propagated note carries a `content_hash`: SHA-256 over
`kind || US || content || US || scope || US || COALESCE(feature_id, '') || US || session_id`
(US = `0x1F`, the ASCII unit separator). The hash is the
propagation primary key — peers deduplicate on it, not on the
locally-generated `note_id`.

This matters for **toolbx container peers**. If `~/.svrnmesh`
isn't bind-mounted from the host, `node_id` rotates on every
container rebuild — the same operator looks like a brand-new
peer every time. Without a stable identity:

- Two rebuilds of the same operator would each ship the same
  note as a "fresh write" under a different `origin_node_id` →
  duplicates pile up on every other peer.
- The work-atlas would surface every rebuild as an unknown
  member, drowning out the actual peer count.

With `content_hash` as the primary key, the dedup is
toolbx-rebuild-safe by construction:

- A note written twice (same content, different rebuilds)
  collapses to one row on every peer via `INSERT OR IGNORE`.
- The first write's `origin_node_id` wins; later rebuilds
  contribute nothing.
- `node_id` rotation costs nothing at the data layer; the
  cosmetic mesh-topology view is the only thing that flickers.

## Toolbx bind-mount — recommended

For long-running toolbx workstations, bind-mount `~/.svrnmesh`
from the host into the container so `node_id` survives rebuilds:

```sh
toolbox enter \
  -- --volume "$HOME/.svrnmesh:$HOME/.svrnmesh" \
     -- /bin/bash
```

(Or add a persistent `[mounts]` entry in your toolbx config.)
This isn't required for correctness — `content_hash` carries us
either way — but it keeps the work-atlas readable: peers stay
stable across rebuilds.

## Conflict resolution

Three named cases, applied by `NoteStore::ingest_remote_notes`:

1. **Identical `content_hash` collision** — idempotent. First
   insert wins; subsequent `INSERT OR IGNORE`. Two peers writing
   semantically identical notes collapse to one row.
2. **Concurrent supersedes** — when two peers each write a
   successor to the same base note while disconnected, both
   successors land on every peer with the second arrival flagged
   via the `fork_of` column pointing at its sibling. No silent
   LWW collapse. The reader surface (audit display / CLI) shows
   the fork; an operator can collapse it explicitly via a new
   supersedes pointing at both.
3. **Tombstone vs edit** — tombstone wins regardless of
   `updated_at`. An edit with a later timestamp does not
   resurrect a tombstoned note. Re-tombstoning is
   operator-explicit via `set_note_tombstone(id, false)`.

## Two transport paths

The mesh layer has two complementary propagation paths:

**Fast path (delta watermark).** Every gossip round (~10s
default), each peer ships notes created since the last
acknowledged `(created_at, note_id)` for that peer. Bounded at
200/round; large catch-ups span multiple rounds. The watermark
table is per-peer and durable.

**Slow path (bucketed content-hash digest).** Every Nth round
(default 10 ≈ every 100s), peers exchange a 256-bucket
content-hash digest (`BTreeMap<u8, u64>` keyed on the first hex
byte of `content_hash`, FNV-1a-64 over each bucket's sorted
hashes). Buckets that disagree trigger a hash-list exchange,
then a targeted pull. Cheap on the wire (~2KB digest), covers
divergence the delta path can't fix:

- Long offline → watermark drifted past local retention horizon.
- Snapshot rollback.
- Bootstrap of a fresh peer — reconciliation is the bootstrap.

## Verifying propagation locally

Two-node smoke (channel transport, no daemons):

```sh
cargo test -p corpus-engine-notes --test propagation
```

Wire size + mixed-mesh shapes + local re-embed at ingest:

```sh
cargo test -p corpus-engine-notes --test note_wire_shapes
cargo test -p corpus-engine-notes --test red_baseline_cross_model_notes
cargo test -p corpus-engine-notes --test note_cosine_own_space
```

`red_baseline_cross_model_notes` has two arms and they are not
interchangeable: one asserts that a vector a peer ships is discarded and
the note re-embedded here (read back out of `note_embeddings`, so it is
storage that is checked and not ranking); the other asserts that a row
already on disk in the pre-2026-08-13 shape stays out of the cosine
pool. Ingest fixes the first case and cannot touch the second.
`note_cosine_own_space` is the same read filter reached from a solo
install, where it is the operator changing embed models — not a peer —
that strands the old rows.

`note_wire_shapes` prints the measured `T1_WIRE green=… red=…` line
(both serializations of the same event, so a regression is visible as a
ratio, not a bare number). To measure against a REAL store instead of a
fixture, snapshot `~/.svrnmesh/notes.db` — never open the live one,
`NoteStore::open` migrates — and run the harness the red baseline used:

```sh
RED_BASELINE_NOTES_DB=/path/to/snapshot.db \
  cargo test -p corpus-engine-notes --test red_baseline_note_wire_size \
  -- --ignored --nocapture
```

Nine cases: convergence, privacy, toolbx volatility, supersedes
chain, offline divergence, bootstrap join, concurrent supersedes
fork, tombstone-vs-edit race, reconciliation bucket diff.

Two-daemon smoke (HTTP transport, real gossip): see
`sovereign-mesh/tests/gossip_integration.rs` — the wiring of the
NoteStore propagation sink into `MeshStore::put` is the
integration boundary; the corpus-engine-notes tests pin the
semantics on the channel stub, and gossip_integration pins the
HTTP layer.

## Toolbx volatility — manual repro recipe

If you suspect duplicate notes after a container rebuild:

1. Pre-rebuild: enter your toolbx container and write a probe
   note via `sovereign note --kind invariant --content 'probe
   <date>'`. Confirm it lands locally
   (`sovereign tools call notes --query=probe`).
2. Trigger a rebuild that loses `~/.svrnmesh` (without the
   bind-mount).
3. Re-enter the rebuilt container and write a probe note with
   **the same content** + same `--session` (so the
   `content_hash` matches).
4. On any peer node, after one gossip round:
   `sovereign tools call notes --query=probe` should return
   exactly one row, not two.

If you see two rows, the `content_hash` collision check is
broken — file an issue at the spec doc reference above with the
contents of both rows + their content_hashes.
