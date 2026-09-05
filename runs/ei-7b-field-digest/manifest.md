# ei-7b — the Field guide digest, rendered from the atlas

`runs/ei-7b-field-digest/run.sh`, per order `ei-7b-field-model`.

## What it does, and how long each leg takes

| Leg | What | Marker | Forecast |
|---|---|---|---|
| 0 | two reflink copies of the `sep` index into `ei7b-legacy` and `ei7b-fieldguide` | `RC_FIXTURE=` | < 30 s, ~0 bytes (btrfs reflink) |
| 1 | `enrich field-atoms sep --into ei7b-fieldguide --show-digest` — publishes 549 `Question` + 759 `Position` atoms and prints BOTH digests | `RC_DIGEST=` | < 60 s, no model |
| 2 | `eval run --bank <derived> --prod-pipeline --isolate`, 4 runs: atoms / legacy / legacy / atoms | `RC_atoms_1=` … `RC_legacy_2=` | ~4 × 10-15 min |
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
`field_digests` / `from_atlas` / `from_legacy_file` counts off the
`retrieval_audit` target, which are the only direct evidence of which source
served the digest.

The arms are the two SOURCES `field_atoms::load_field_model` can serve from, on
byte-identical corpora:

| Arm | Corpus | v1 file | Atlas | Expected |
|---|---|---|---|---|
| `atoms` | `ei7b-fieldguide` | removed | 549 `Question` + 759 `Position` | `from_atlas=1`, `from_legacy_file=0` |
| `legacy` | `ei7b-legacy` | kept | empty | `from_atlas=0`, `from_legacy_file=1` |

The `legacy` arm IS the merge precondition in a run: it is the shape `sep` is
in today, and it must still splice a digest. A third "no field model at all"
arm was dropped — it measures nothing this change decides and would cost two
more model runs. If BOTH arms read `field_digests=0` the instrument is dark and
the source scores mean nothing for this comparison; `INSTRUMENT_NOTE_` says so
explicitly rather than letting a plausible zero pass.

## Controls

Nothing writes to `sep`, to any `sep-<slug>` atlas, to `wessex-hoard`, to
`brothers-karamazov-book-1`, or to `wikipedia`. The two fixture ids
deliberately do NOT start with `sep-`: `EvidenceSite::derive`'s table is
`[("sep-", "sep")]`, so `sep-fieldguide` would declare the control corpus as
its parent and fetch evidence out of it.

## Cleanup

`rm -rf ~/.svrnmesh/indexes/ei7b-legacy ~/.svrnmesh/indexes/ei7b-fieldguide`
after the rows are read. They are reflink copies, so they hold ~0 exclusive
bytes until the atlas write diverges them (~a few MB).
