---
schema: work-order/v1
id: handed-8-writer-door
status: draft
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — writers: an unguarded write does not compile
engine: ralph pool; REVIEW-mint first, then the rows it mints
budget: see the mint row's cap in ralph/next/handed/STATE.md
---

# Order: handed-8-writer-door — the in-crate door

## Objective

hd-3 puts a flock inside the write path and closes the raw handle to other crates
(E0624). Inside corpus-engine the write methods stay reachable on the index handle,
so "every write goes through the guard" is a module convention there. This order
makes it a compile error: the mutating methods MOVE off `CorpusIndex` onto a
`CorpusWriter` facade — obtained from `CorpusIndex::writer()`, which acquires the
flock — leaving no method of that name on the index at all. An in-crate write that
does not go through the facade is then **E0599** ("no method named `insert_batch`
found for struct `CorpusIndex`").

`pub(crate)` is NOT the mechanism and this order said so until round 3: a
`pub(crate)` method is still callable from everywhere inside corpus-engine, so the
plant would have stayed green (§6), and a private-but-existing method is E0624, not
E0599 — the code `handed.toml` and the audit name. If any one method must stay on
`CorpusIndex` (a read path needs it, or the facade cannot reach it), the row says so
and names E0624 for that site instead of pretending otherwise.

Cross-process exclusion stays a runtime refusal — another process is not in this
compilation unit — and TOPOLOGY.md:284-287 says so.

## Premises to verify before minting (from this session's reads, not re-checked)

- hd-3 has landed: `table()`/`connection()` are `pub(crate)`, `Error::WriterHeld`
  exists, and the per-corpus flock is taken at the index mutation sites.
- The write methods live on `CorpusIndex` (corpus-engine/src/index/), with the raw
  lancedb mutations in write.rs, create.rs, maintain.rs, raptor.rs, mod.rs.
- The atlas writers take an `atlas_dir: &Path` and are guarded by hd-3's second row.
- The mint must COUNT BOTH populations before minting rows — the round-3 review
  found this order telling it to census in-crate only, which priced the work at
  about 38% of what it is. Measured 2026-09-17 over the eleven distinctively-named
  mutating methods (`insert_batch`, `delete_chunks_by_source_doc`,
  `delete_chunks_by_ids`, `dedupe_by_content_hash`, `create_with_sharing`,
  `build_title_scalar_index`, `build_indexes`, `build_raptor_index`,
  `write_field_checkpoint`, `write_field_skeleton`, `mark_ingestion_complete`):
  - corpus-engine/src — **78 sites / 14 files**
  - corpus-engine tests + examples — **6 sites / 5 files** (a `tests/` target is a
    separate crate, so it sees only the facade's public surface)
  - outside corpus-engine — **56 sites / 29 files / 8 crates**: sovereign-mesh (17
    files), sovereign-tools (3), sovereign-core (3), sovereign-grants (2), and one
    each in sovereign-meshapp, sovereign-daemon, sovereign-cli-llm, sovereign-api
  `prune` and `optimize` are generic names (10 sites repo-wide, several on other
  types) — resolve each by receiver before counting it. The out-of-crate half is
  not optional: the facade is the public door too, so those 56 sites become
  `index.writer()?.<method>(..)`. The fat review's old "50+ production files"
  figure was for the rejected lease design and included readers; these numbers
  replace it. Re-run the census before minting — hd-3 lands first and moves code.

## Steps

1. MOVE the mutating methods onto `impl CorpusWriter`, obtained from
   `CorpusIndex::writer()`, which acquires the flock. No method of the same name is
   left on `CorpusIndex` — that absence is what makes step 3's plant E0599.
2. Repoint the in-crate callers. Mechanical; group by module, at most ~10 files a row.
3. PLANT: call a mutating method on a bare `CorpusIndex` inside corpus-engine; LINT
   must report **E0599** naming the method. Revert, LINT green. If the plant reports
   E0624 instead, the method was made private rather than moved — that is the wrong
   mechanism, not a passing plant.
4. Append every row you mint to the `depends` of `REVIEW-DEMO-hd-7-bench` and of
   `REVIEW-audit-hd-2` in `ralph/next/handed/STATE.md` (or `ralph/STATE.md` once
   promoted), in the same commit that mints them. Without it the pool's
   `first_ready_review` (scripts/ralph.py:256-261) can run the bench and the final
   audit before the rows they are meant to cover.

## Cap basis (re-priced at round 3; cap 9 in STATE.md, was 5)

1 row for the facade and `writer()`; 2 for corpus-engine/src's 14 files at ~10 a row;
3 for the 29 out-of-crate files; 1 for the 5 test/example files; 1 for the plant and
the `handed.toml` `enforced_by` update — **7-9 rows**. Past 9, PROMPT §4 applies:
write `ralph/NEEDS_HUMAN.md` with the measured count and stop.

## Kill

- A write path cannot obtain the facade without a nested acquire that self-refuses
  even with the in-process share (hd-3's `Weak` map): stop — the door is in the
  wrong place, and the flock alone is the honest guarantee.
- More rows than the cap: stop, per PROMPT §4.
