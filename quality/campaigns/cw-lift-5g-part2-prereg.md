# cw-lift 5g part 2 — pre-registration

Written 2026-09-09, BEFORE any collector code exists and before any data.
Bars first or the verdict is not honest. Supersedes nothing; the ladder row
in `cw-lift.toml` stays OPEN until a measurement row here reads `met`.

## What part 2 was ordered to be, and why that order was wrong

The order (supervisor, 2026-09-09) priced part 2 as a deletion: ~2,465 lines
of parallel lease machinery against a fold plus one executor. Static survey
before touching it found the arithmetic wrong in three ways and the shape
wrong in one.

**The shape.** `ShardManager::coordinate_merge`
(`commonwealth-knowledge/src/shard_manager.rs:183`) is the only code path in
the workspace that pulls a peer's shard tarball and merges it. Its only two
production call sites are `commonwealth-api/src/routes_internal/corpus_queue.
rs:210` and `:550` — both inside the file the order says to delete. The CLI
does not cover it: `cmd_corpus_merge_partitions`
(`sovereign-cli-llm/src/corpus_cmd/partitions.rs:180`) merges every
`<corpus>-partition-*/` directory ON THIS NODE and documents itself as a
one-shot rescue. So the deletion as ordered would leave every donor's chunks
on the donor, with the fold reporting the handoff terminal and every unit
`Complete`. Exit 0, wrong corpus — this workspace's characteristic failure.

Part 1's proof did not catch it because it was single-node: two units, one
Halo, one partition directory. The gap is invisible at n=1, and 5e's
cross-node half is still could-not-judge, so nothing in phase 5 has yet
exercised a two-node merge.

**The arithmetic.** `corpus_queue.rs` is not wholly deletable —
`corpus_partition_evict` (700-727) and `corpus_collaborate_status` (728-843)
are separate surfaces, so its legacy half is ~500 of 843 lines. With
`ingest_grant.rs` (258) kept per part 1's decision, the real deletion is
~1,876 lines, not 2,465. Two live `WorkQueueManager` consumers the order's
table did not list: `shard_manager.rs` (`with_work_queue`, reading
`HandoffQueue.participating_peers`) and `corpus_collaborate.rs:260`.

## The seam, and why it needs no new decider

The fold already carries everything the merge needs. Three reads, each cited:

- **Trigger.** `WorkHandoff::phase_at` returns `HandoffPhase::Complete` when
  `queued == 0 && leased == 0` (`commonwealth-work/src/projection.rs:306`).
- **Leader.** `WorkHandoff.submitter: ActorKey`, "from ADMISSION, never from
  the payload" (`projection.rs:274`). Single-valued, identical on every node,
  already the only actor permitted to revoke. It is the merge leader and
  costs no second decider (ARCH §10.6).
- **Peer set.** `WorkUnitStatus::Complete` carries `lessee: ActorKey` and
  `provenance: ComputeAttribution` (`projection.rs:172-179`). The union of
  `lessee` over a handoff's terminal units is the participating-peer set,
  read off a durable replicated journal.

That last one is a strict improvement on what it replaces, and the
improvement should be claimed as one. `HandoffQueue.participating_peers` is
in-memory and dies with the coordinator, which is why `coordinate_merge`
carries a gossip-derived fallback for "queue-mode handoff that outlived a
coordinator restart" (`shard_manager.rs:235-260`). On the rail there is no
such case: the acts are the record. The fallback and its helper
`participating_peers_from_gossip` (`shard_manager.rs:661`) become deletable,
which the order did not count.

`Merging` was reserved for exactly this and says so:
`projection.rs:299-305` — "It is not pruned from the enum because pruning it
would fork the vocabulary ingest still uses, and cw-lift 5g brings ingest
onto this fold, at which point an `IngestExecutor` is what will produce it."
The test `the_handoff_phase_walks_open_to_draining_to_complete_and_never_
merges` (`projection/tests.rs:414`) stays true and must not be weakened: it
pins that the phase DERIVED FROM UNIT STATUSES never yields `Merging`. The
merge is a leader-side step after `Complete`, not a unit-derived state.

`commonwealth-work` learns nothing about ingest. It carries a
`[[forbid]] -> sovereign-*` row in `quality/ARCH_LAYERS.toml` and 5f builds
it in a sandbox; the collector lives on the sovereign side beside the
executor, same shape as `IngestExecutor` itself.

## Bars — fixed now, before the data

**B1 — the gap is real.** Two donors, one corpus, one handoff; both units
`Complete`; the canonical index is missing the non-leader donor's chunks.
Watched failing on the current tree before any collector exists. A negative
control at n=1 must PASS in the same harness, or B1 proves only that the
harness never merges.
KILL: if the merge already works cross-node today, part 2 is the ordered
deletion after all and this pre-registration is withdrawn in writing.

**B2 — the collector closes it.** Same scenario, collector wired: the
canonical index contains chunks from BOTH donors, and `svrn` answers a query
that only the remote donor's range can satisfy. Chunk count alone is not the
bar — a count can be right for the wrong reason; the query is what proves the
data is reachable.

**B3 — exactly one node merges.** Both donors run the collector; the
non-submitter must decline. Instrument: a tracing event at a captured target
naming the decision and the submitter it compared against. Two merges racing
one output directory is the failure this bar exists to catch, and a passing
B2 does not imply it.

**B4 — the merge is idempotent under at-least-once.** Run the collector twice
against the same terminal handoff. `merge_shards` dedupes on `content_hash`
and `(unit_id, source_doc_id)` (`corpus-engine/src/sharding.rs:780-784`), so
the claim is already load-bearing in `IngestExecutor`'s
`Idempotency::Idempotent` declaration. It has never been exercised at the
MERGE level, only asserted. Second run must not change the chunk count.

**B5 — a donor that never reports is named, not silently dropped.** One unit
terminal `Failed` (attempts spent, `outcome: None`), the rest `Complete`. The
merge must proceed with what exists AND the corpus must record that it is
partial. Absence is reported, never defaulted (ARCH §18.3). A merge that
quietly ships 2-of-3 partitions as if complete is the same defect as the gap
this whole rung is fixing.

**B7 — a partial canonical is never advertised as complete.** Added
2026-09-09 AFTER B1 ran and BEFORE any fix, because B1's measurement found a
live hazard wider than this rung.

`auto_recover::try_recover_stranded_partitions` is reachable on the fold path
(`sovereign-mesh/src/auto_ingest.rs:297`) and merges only the partitions under
one node's index dir. Its guard against publishing a partial canonical —
`RecoveryOutcome::IncompleteCoverage` — arms only when a partition meta stamps
`total_shards`, and `corpus-engine/src/engine/ingest.rs:718` stamps it for
`ExtractorConfig::WikipediaJsonl` and nothing else, scoped with the comment
"the only multi-shard extractor today; trivial to extend when more arrive."

**That comment is now false and `ingest:v1` is what falsified it.** A fold unit
slices ANY recipe across donors, so every recipe is multi-shard the moment the
fold is used. `auto_ingest.rs:263-276` states what the dark guard costs, from a
failure already had in the wild: "what produced the 17/38 partial canonical bug
linux-peer hit … Producing a partial canonical ourselves and then re-
advertising it on gossip pollutes the mesh's canonical-sync convergence — every
peer ends up with a different 'complete' canonical and they fight forever."

The bar: a non-Wikipedia recipe sliced into two units across two donors, and
the node holding one partition must REFUSE to publish a canonical and say which
coverage it lacks. Arming `total_shards` for every sliced recipe is the likely
shape; the collector knows the unit count from the handoff, which is the number
the guard wants and the legacy path never had.

This bar gates the collector rather than following it. The collector makes the
fold the normal path, so shipping it with the guard dark would take a hazard
that today needs a stranded partition to reach and put it on the main road.

**B6 — no gate regressions.** `layer-gate` exit 0 (the forbid row is what
keeps `commonwealth-work` liftable), `boundary-gate` exit 0 — with the caveat
that boundary-gate is under repair in a parallel lane and its exit 0 is worth
less than it looks until that lands. `sovereign-lint.sh --human --full` exit
0. Workspace tests exit 0.

## What this plan does NOT check, stated now rather than discovered later

- **The network half.** An in-process two-donor simulation stubs the HTTP
  tarball pull. `coordinate_merge` fetches over the wire and an in-process
  test proves none of that. B1-B5 are mechanism bars; a real two-machine run
  is a separate, later reading and is currently gated behind `forget-member`
  on the duplicate Mac row (endpoint key 86627fd5).
- **Byte-level shard integrity across the wire.** The post-merge re-embed
  spot-check (`VerifyReport`) exists and is reused, not re-derived.
- **Whether the deletion's line count meets `cw-net-deletion`.** That is that
  bar's measurement row, not this one's, and a measured miss leaves it open.

## Order of work

The deletion does not start until B2 and B3 are green. Deleting a working
path before its replacement is proven is what part 1 refused to do and the
reason it split; part 2 inherits the rule.

## Measurements

**B1 — MET (red for the right reason), 2026-09-09, commit `50238f364`.**
`sovereign-mesh/tests/main/fold_ingest_cross_node_merge_e2e.rs::
two_donors_on_two_nodes_leave_the_canonical_missing_the_peers_chunks`. Two
`ingest:v1` units, real Ed25519-signed ops through `commonwealth_rail::admit`
into `WorkProjection::fold`, completed by two DIFFERENT lessees, handoff
asserted at `HandoffPhase::Complete` before the corpus is asked anything.

    merge outcome on the leader : Recovered { chunks: 2, shards_covered: 0 }
    canonical exists            : true
    canonical chunk_count       : 2   (expected 4 — 2 local + 2 peer)
    leader-only term reachable  : true
    peer-only term reachable    : false

Negative control `two_donors_on_one_node_do_land_both_slices_in_the_canonical`
PASSES at 4 chunks with both terms reachable, so the red is the cross-node
case and not a harness that never merges.

**The prediction was right in substance and wrong in mechanism, which made it
worse.** The pre-registration above predicted nothing would merge. What
actually happens is that the merge SUCCEEDS and REPORTS success on half the
corpus, and the canonical is then re-advertised in `hosted_corpora` gossip. A
node advertising a complete corpus that holds half the data is a stronger
version of the same defect, and it is what minted B7.

B1's kill condition did NOT fire: the merge does not work cross-node today.
The pre-registration stands and part 2 proceeds as the collector, not as the
ordered deletion.

Both tests are `#[ignore]`d with reasons naming this rung. That is a temporary
marker and not a verdict — the suite is not green because this passed, it is
green because this is hidden. The ignore comes off with the collector.

Known not checked by B1: the HTTP tarball pull, the ingest pipeline itself (no
recipe, no acquirer, no embedder), and the `total_shards` coverage gate — which
is now B7's subject.
