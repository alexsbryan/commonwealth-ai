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
reported as such. Its yield is the `YIELD_<arm>_<run>` line.

The arms are the two SOURCES `field_atoms::load_field_model` can serve from, on
byte-identical corpora:

| Arm | Corpus | v1 file | Atlas |
|---|---|---|---|
| `atoms` | `ei7b-fieldguide` | removed | 549 `Question` + 759 `Position` |
| `legacy` | `ei7b-legacy` | kept | empty |

The `legacy` arm IS the merge precondition in a run: it is the shape `sep` is in
today, and it must still serve a digest.

### What the 11:17 run got wrong, and what now catches it

Both arms retrieved NOTHING and the lane still exited 0. Two independent
defects, both in this script:

1. **`cp -r` copies `_corpus_meta.json`, which carries `corpus_id`.** Both
   fixtures advertised themselves as `sep`; `installed_indexes()` dedups on the
   advertised id, kept the real `sep` and DROPPED both fixtures. Neither ever
   appeared in the corpora list. Leg 0 now rewrites `corpus_id` per fixture
   (ei-7a's `build_subset.py:210` always did) and ASSERTS it, exiting 94 rather
   than proceeding. Leg 2 also fails loudly on `corpus_id collision` in the log
   and on `corpora_searched_max=0 || final_chunks_total=0`.

   It was NOT the ei-7a index trap. The reflink carried all five Lance index
   files across verbatim — same UUIDs on `sep`, `ei7b-legacy` and
   `ei7b-fieldguide` — so a filesystem copy is not a `write_dataset` copy and
   does not lose indices.

2. **`--prod-pipeline` cannot observe the digest at all.** It drives
   `Runtime::retrieve_evidence` (context build → `kq_pipeline()` → merge →
   truncate) and does NO synthesis, so it never assembles a system message.
   `splice_ambient_field_digests` has exactly two callers — `turn.rs:593` and
   `streaming.rs:4440` — and this mode enters neither. A `field_digests` counter
   here was always going to read 0 for a reason that has nothing to do with the
   port. The `DIGEST_` line is kept as a STATEMENT of that fact so the next
   person does not design the same instrument, and the digest's evidence is leg
   1 plus the unit tests.

Both new checks were validated against the 11:17 logs before being trusted:
they report `COULD-NOT-JUDGE` on that data, on each cause independently.

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
