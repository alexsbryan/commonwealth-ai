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
