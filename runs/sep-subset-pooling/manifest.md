# sep-subset-pooling — is the LAST-pooled stack actually better, or just different?

Staged 2026-09-07 by ei-6b, per the seat's addendum (steer 5b1a9529, operator:
"Queue the sep-subset re-embed lane to decide"). **The pre-registered decision
below was written before the run.**

## Why

ei-6b measured that `sep` and `wikipedia` are MEAN-pooled corpora under a
LAST-pooled stack (note 500f1229): sep's chunks score 0.9997 re-embedded with
`--pooling mean` and 0.7069 with `last`, while `wessex-hoard` (2026-09-02) is
0.9968 last / 0.6615 mean and `sep-al-farabi`'s atlas seeds (2026-09-05) are
0.9610 last / 0.5131 mean. Repairing it means re-embedding 182k + 1.9M
paragraphs and re-publishing both snapshots — operator-only and irreversible.

Nobody has measured whether it is worth it. A mismatched space is obviously
wrong on a cosine; it does not follow that RETRIEVAL is worse, because the
reranker and the FTS leg may be carrying the result. This lane answers that on
a fixture, cheaply, before anyone spends the re-embed.

## Arms — one variable

- **M** — `raptor-subset-off` exactly as it stands: 33,884 chunks, vectors
  copied from `sep`, therefore mean-pooled. Not modified, not re-indexed.
- **L** — a NEW corpus id, `raptor-subset-pooled-last`: the same rows with the
  same ids, titles and content, and **only** the `chunks.lance` vector column
  re-embedded through the daemon's document path (`/v1/embeddings`, the current
  last-pooled stack). Atlas and seed tables carried over unchanged — they are
  already last-pooled.

Nothing is overwritten. Installed `sep`, every `sep-<slug>`, `wikipedia` and M
itself are never opened for write.

## Two traps this run is built around, both banked

1. **`lance.write_dataset` copies ROWS, NOT INDICES**, and `CorpusIndex::search`
   gates BOTH legs on an index existing above `FLAT_SCAN_THRESHOLD` = 10,000
   rows (`corpus-engine/src/index/search.rs:296-298`). At 33,884 rows a copied
   fixture retrieves *nothing* and still exits 0 with real search times — which
   is exactly what happened to ei-7a (note 29f1f14a: `sources 0/66`,
   `facts 0/158`, nothing warned). `build_last_pooled.py` rebuilds all three
   indices, with parameters MIRRORED from `corpus-engine/src/index/create.rs`
   rather than guessed (IVF_PQ, partitions = sqrt(rows) clamped [8,4096],
   distance Cosine, sub-vectors = dims/16; Inverted on `content` and `title`),
   then ASSERTS both legs live in the exact terms `gate_info` reads and refuses
   to leave a fixture that fails it.
2. **`svrn corpus optimize` cannot repair it** — it reports "already
   maintained" and skips the index pass on a table with zero indices.

## Instrument validated before the result (ARCH §18.4)

`selfcheck.py` runs the same self-consistency probe on both arms and the run
STOPS if either fails:

- **L must read >= 0.99** at the daemon's document path. Below that the
  re-embed did not land and no bench number is interpretable.
- **M must still read ~0.70** (ceiling 0.80). If it moved, the control was
  disturbed and the comparison is void.

Both rows are printed in the run log whatever happens.

## Measurement

The SEP bank (`sovereign/bench/sep/questions.toml`, 21 questions, 66 expected
sources / 158 expected facts) with `corpus =` rewritten per arm — ei-7a's
pattern, the bank otherwise unmodified. `svrn eval run --prod-pipeline
--isolate --limit 30`, the `retrieval-prod` HARD lane: facts + sources recall.
n=2 per arm, both arms the same day, mode printed in every row. Synthesis arm
only if time allows; it is not part of the decision.

Reference points from ei-7a on this same fixture, for orientation only —
not baselines this run is judged against: `sep` control 42/66 (63.6%),
`raptor-subset-off` 48/66 (72.7%).

## PRE-REGISTERED DECISION

The retrieval band here is EXACT — the scorer is deterministic given a fixed
index — so **any delta of >= 1 fact or source is real**. Both numbers are
reported either way.

| outcome | what it means | what I recommend |
|---|---|---|
| **L > M** on either metric, neither down | last pooling retrieves better, as the model's spec says it should | recommend re-embed + re-publish of `sep` and `wikipedia` to the operator |
| **L < M** | the last-pooled stack REGRESSES on the bench against the model's own spec | that is the finding, and it outranks the re-publish question |
| **L = M** | the reranker and the FTS leg mask the space | re-publish is for PORTABILITY only, not for measured retrieval gain |
| one up, one down | **MIXED** — not the clean result the decision was registered on | report both; recommend nothing on this alone |
| a trial produced no JSON | **COULD-NOT-JUDGE** — a verdict about the RUNNER, not about pooling | fix the runner, re-run |

## Cost

Re-embed 33,884 chunks at a measured 17.3 chunk/s (batch 128 through the
daemon; batch 32 gave 13.4 and batch 256 gave 10.3, so 128 is the pick) ≈ **33
min**, plus ~10 s of index build, plus 4 bank runs at ~1m45s ≈ 7 min. Forecast
**~45 min**, which is why this is a unit and not a Bash call. Disk: ~341 MB for
arm L. Needs the embed slot only — no primary model — but the bank runs want
the campaign's engine resident, so it waits for 35B-BACK.

## Legs and markers

`preflight` (CLI, python-lance, both arms' existence, the bank, and one live
daemon embed — all of it BEFORE the 33-minute step) → `box-before` →
`bank-copies` → `build-arm-l` → `selfcheck` → `bank-<arm>-t<n>` ×4 → `score` →
`VERDICT-*` → `box-after` → `DONE`. A terminal `DONE` marker is written on
SIGTERM/INT/HUP too, so a kill cannot look like "still running".

---

## Second measurement: `--limit 10` (added 2026-09-08, seat-authorized)

**Why a second limit exists.** The `--limit 30` run came back
`VERDICT-M-BETTER` at sources 64/66 (97%) and facts 145/159 (91%) for M against
63/66 and 144/159 for L — a real delta by the pre-registered exact band (trial
spread was zero on both arms and both metrics), but on a **saturated** scale
with two slots of headroom. ei-7a measured this same fixture at **48/66 at the
default `--limit 10`**, so limit 10 leaves ~18 points of room and can actually
separate the arms. That is the ARCH §18.4 concern — an exact band on a scale
with no headroom still cannot discriminate — and it is the reason for a second
measurement, not a reason to re-read the first.

**The limit-30 result stays on the record exactly as it fired**, with the
ceiling named beside it. This run adds a row; it does not replace one.

**Same arms, same script, same five pre-registered outcomes.** `REUSE_ARM_L=1`
skips the re-embed (arm L is already built and installed, 33,884 rows, both
legs indexed, self-probe 0.9999) and runs the bank only — one script with two
invocations, because the second limit is a second measurement of the same two
arms and forking the script would make them two things that merely look alike.
The self-check still runs first and still stops the run if either arm has
moved. n=2 per arm, both arms the same day, `--prod-pipeline --isolate`.

**Invocation:** `REUSE_ARM_L=1 BANK_LIMIT=10 runs/sep-subset-pooling/run.sh`

**Scope caveat carried into both rows:** this is the sep SUBSET fixture — 21
questions, 66 expected sources, 159 expected facts, 33,884 chunks from 288
articles. It is not the full SEP bank, and **no `wikipedia` arm was run at
all**, though wikipedia is mispooled the same way and is 1,896,488 chunks.
