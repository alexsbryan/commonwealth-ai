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
makes it a compile error: the mutating methods become `pub(crate)` on the index and
are exposed only on a `CorpusWriter` facade that acquires the flock itself. An
in-crate write that does not go through the facade is E0599.

Cross-process exclusion stays a runtime refusal — another process is not in this
compilation unit — and TOPOLOGY.md:284-287 says so.

## Premises to verify before minting (from this session's reads, not re-checked)

- hd-3 has landed: `table()`/`connection()` are `pub(crate)`, `Error::WriterHeld`
  exists, and the per-corpus flock is taken at the index mutation sites.
- The write methods live on `CorpusIndex` (corpus-engine/src/index/), with the raw
  lancedb mutations in write.rs, create.rs, maintain.rs, raptor.rs, mod.rs.
- The atlas writers take an `atlas_dir: &Path` and are guarded by hd-3's second row.
- The mint must COUNT the in-crate callers of the write methods before minting rows:
  the fat review's figure of "50+ production files" was for the rejected lease
  design and includes readers.

## Steps

1. Move the mutating methods to `pub(crate)` and expose them on a `CorpusWriter`
   facade obtained from `CorpusIndex::writer()`, which acquires the flock.
2. Repoint the in-crate callers. Mechanical; group by module, at most ~10 files a row.
3. PLANT: call a mutating method on a bare `CorpusIndex` inside corpus-engine; LINT
   must report E0599 naming the method. Revert, LINT green.

## Kill

- A write path cannot obtain the facade without a nested acquire that self-refuses
  even with the in-process share (hd-3's `Weak` map): stop — the door is in the
  wrong place, and the flock alone is the honest guarantee.
- More rows than the cap: stop, per PROMPT §4.
