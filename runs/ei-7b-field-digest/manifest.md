# ei-7b — the Field guide digest, rendered from the atlas

`runs/ei-7b-field-digest/run.sh`, per order `ei-7b-field-model`.

## What it does, and how long each leg takes

| Leg | What | Marker | Forecast |
|---|---|---|---|
| 0 | two reflink copies of the `sep` index into `ei7b-plain` and `ei7b-fieldguide` | `RC_FIXTURE=` | < 30 s, ~0 bytes (btrfs reflink) |
| 1 | `enrich field-atoms sep --into ei7b-fieldguide --show-digest` — publishes 549 `Question` + 759 `Position` atoms and prints BOTH digests | `RC_DIGEST=` | < 60 s, no model |
| 2 | `eval run --bank <derived> --prod-pipeline --isolate`, 4 runs: atoms / none / none / atoms | `RC_atoms_1=` … `RC_none_2=` | ~4 × 10-15 min |
| — | terminal | `DONE` | |

Box state is printed as `BOX_before:` / `BOX_after:` (GTT bytes, MemAvailable, `df /home`).

## Refusal

Refuses with exit 3 when `pgrep -af 'cargo|rustc'` finds a build or
`MemAvailable < 40 GB`. Override: `EI7B_FORCE=1`.

## What each leg is evidence FOR

**Leg 1 is the evidence.** `context.knowledge_view_digests` has exactly one
reader — `sovereign-core/src/runtime/system_message.rs:233`, prompt assembly —
so this change moves the PROMPT and structurally cannot move retrieval's source
recall. The two digest texts, rendered by the same `render_landscape` from the
two sources, are what the port has to be judged on.

**Leg 2 is a does-not-regress check on a path the change cannot touch**, and is
reported as such. Its useful yield is the `DIGEST_<arm>_<run>` line: the
`field_digests` count from the `retrieval_audit` target, which is the only
direct evidence the digest fired at all. The `atoms` arm must show
`field_digests > 0` and the `none` arm `field_digests=0` with `skipped>0` — if
both arms read zero, the instrument is dark and the source scores mean nothing
for this comparison (`INSTRUMENT_NOTE_` says so explicitly).

## Controls

Nothing writes to `sep`, to any `sep-<slug>` atlas, to `wessex-hoard`, to
`brothers-karamazov-book-1`, or to `wikipedia`. The two fixture ids
deliberately do NOT start with `sep-`: `EvidenceSite::derive`'s table is
`[("sep-", "sep")]`, so `sep-fieldguide` would declare the control corpus as
its parent and fetch evidence out of it.

## Cleanup

`rm -rf ~/.svrnmesh/indexes/ei7b-plain ~/.svrnmesh/indexes/ei7b-fieldguide`
after the rows are read. They are reflink copies, so they hold ~0 exclusive
bytes until the atlas write diverges them (~a few MB).
