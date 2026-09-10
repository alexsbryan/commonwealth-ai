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

---

**B2 — first reading MET AT THE WRONG ALTITUDE (2026-09-09, `df2ffecb8`);
re-measured and MET (2026-09-09). Read the correction below the first reading
before citing it — the first reading proved the bytes landed, not that the
corpus works.** The reading as originally recorded is kept verbatim, because a
pre-registration whose measurements get quietly retrofitted is worth nothing.
`sovereign-mesh/tests/main/fold_ingest_cross_node_merge_e2e.rs`. Both halves
of the new path are measured, because the seam between them is thin glue and a
test that drives only the glue proves little.

*First half — `ingest_executor::fold_coverage_for`, a pure read.*
`the_fold_names_both_verified_donors_and_where_to_find_them`. Same fold as B1
(real Ed25519-signed ops through `commonwealth_rail::admit` → `WorkProjection::
fold`, two units, two lessees), asked for coverage:

    handoff_id : handoff-0000000000000000   (the submitted handoff)
    expected   : 2                          (distinct VERIFIED lessees)
    nodes      : [node-1100000000000000, node-2200000000000000]
    abandoned  : []                         (is_partial() == false)

Watched red by gating the collection loop on `&lessee == self_key` — the
local-donor-only defect moved down to the fold: `expected: left 1, right 2`.

*Second half — the merge, over a real socket.*
`two_donors_on_two_nodes_land_both_slices_in_the_canonical`, driving
`commonwealth_api::auto_recover::merge_from_fold_coverage` → `ShardManager::
merge_participants`. The peer donor's partition is served by the peer's OWN
`commonwealth_api::server::internal_router` on a loopback socket and reaches
the leader through `GET /internal/index/serve` → `tar xf`; the leader resolves
the peer's address through `peer_control_urls` → `PeerTransport::endpoints`
over a `MemberRecord`, the same as production. B1 could say nothing about any
of that, because B1 pulled nothing.

    merge outcome               : Recovered { chunks: 4, shards_covered: 2 }
    fold coverage               : expected=2 nodes=[node-11.., node-22..] abandoned=[]
    canonical exists            : true
    canonical chunk_count       : 4
    leader-only term reachable  : true
    peer-only term reachable    : true      ← THE BAR

`peer-only term reachable` is the bar and the chunk count is corroboration,
in that order, exactly as this pre-registration specified: a count can be
right for the wrong reason.

Watched red by truncating the participant list to the local node — the
original defect, where participants come from local disk rather than from the
fold. It reproduces B1's reading exactly:

    merge outcome               : Recovered { chunks: 2, shards_covered: 1 }
    fold coverage               : expected=2 nodes=[node-11.., node-22..]
    canonical chunk_count       : 2 (expected 4)
    peer-only term reachable    : false

The fold names both donors in the red as in the green, so the red is about the
participant set the merge was handed and not about the fold.

Both `#[ignore]`s from `50238f364` are GONE. B1's negative control
(`two_donors_on_one_node_do_land_both_slices_in_the_canonical`) is retained
unignored and still runs the DISK-derived path, because it controls for the
fixture rather than for the collector.

Known not checked by B2: two real machines (this is two index dirs, two
`AppState`s and two sockets in one process); the ingest pipeline itself (no
recipe, no acquirer, no embedder); `auto_ingest`'s tick loop, so the ORDER of
its arms — including the load-bearing `continue` after a coverage refusal — is
still unmeasured; and the actor-vs-host distinction `FoldCoverage::expected`
documents, which this fixture cannot see because one lessee reports one host,
so 2 actors and 2 hosts are the same number here.

**B2 — THE CORRECTION. The reading above was taken at the wrong altitude, and
what it actually proved was that the bytes are on disk. 2026-09-09.**

The bar says the merged canonical must answer "a QUERY that only the remote
donor's slice can satisfy", and says why in its own words: "a count can be
right for the wrong reason; the query is what proves the data is reachable."
The reading above satisfied it with `probe_canonical`, which opens the
canonical directory through `CorpusIndex::open`. That bypasses both gates
between a merged chunk set and a corpus anyone can reach, so it answers a
weaker question than the bar asked — and on `df2ffecb8` the two answers
disagreed.

*What the corpus actually was.* `merge_from_fold_coverage` →
`ShardManager::merge_participants` → `CorpusEngine::merge_partitions`
(`corpus-engine/src/engine/mod.rs:2771`) writes the merged chunks and stops. It
never called `build_indexes`, `mark_indexes_built`, `mark_ingestion_complete`
or `compute_and_stamp_fingerprint` — all four of which the disk-derived sibling
does (`sharding.rs`, and the fingerprint stamp after them). So the canonical
carried `ingestion_in_progress: true, indexes_built: false`;
`CorpusEngine::installed_indexes` skips it on the first
(`is_ingestion_complete`), `usable_indexes` on the second, and
`hosted_corpora` gossip is built from `installed_indexes()`
(`sovereign-mesh/src/capabilities.rs:103` → `:122`).

This was already recorded in this file, under B5, as "A SEPARATE DEFECT FOUND
WHILE MEASURING B5, NOT FIXED", together with the sentence "that means B2's
green overstates the end-user reading". It was recorded and not acted on, and
the row above went on saying MET. Recording a defect is not the same as
correcting the measurement it invalidates.

*The re-measurement.* `two_donors_on_two_nodes_land_both_slices_in_the_canonical`
now reads the bar through `CorpusEngine::usable_indexes()` — opening the corpus
by id through `CorpusEngine::open_index_for_corpus`, so the search runs on a
handle the product's own accessor produced — and asserts the corpus is present
in `installed_indexes()`, which is the gossip term. The disk-level probe is
kept beside it as corroboration, so "the peer's chunks never arrived" and "the
chunks arrived and nothing routes to them" print as different failures.

Watched RED on the tree before the fix, with both probes side by side:

    merge outcome               : Recovered { chunks: 4, shards_covered: 2 }
    fold coverage               : expected=2 nodes=[node-11.., node-22..]
    installed_indexes()         : []
    usable_indexes()            : []
    canonical dirs on disk      : ["cw-lift-5g-two-nodes"]
    leader-only term (installed): None
    peer-only term (installed)  : None      ← THE BAR, and it was never asked
    --- through CorpusIndex::open, which bypasses both gates ---
    canonical exists            : true
    canonical chunk_count       : 4
    leader-only term (on disk)  : true
    peer-only term (on disk)    : true

`None` rather than `false` is deliberate (§18.3): the search never RAN, because
`usable_indexes()` had no row to run it against, and "asked and missed" is a
different reading from "never asked".

The red also settles which half was broken. The merge was fine — 4 chunks, both
donors' terms retrievable from a handle that already existed. Only the finalize
was missing, so the correction is about the bar's altitude and not about the
collector.

*The fix.* `merge_from_fold_coverage` calls `corpus_engine::finalize_canonical`
after `merge_participants` returns `Ok(Some(_))`. That function is the disk
path's own Phase 3, lifted out of `merge_partitions_into_canonical` and given
one name (ARCH §10.6) rather than re-spelled: `build_indexes` →
`mark_indexes_built` → `mark_ingestion_complete` → fingerprint LAST, with the
ordering rationale — a peer pulling against a fingerprint trusts the chunk set
is stable, and the ingestion-complete bit is the proxy for stable — living in
that one place. `merge_partitions_into_canonical`'s behaviour is byte-for-byte
what it was; the finalize is on the FOLD path only, and NOT inside
`merge_participants`, which `coordinate_merge` shares (§10.2).

GREEN after, same test, exit 0:

    installed_indexes()         : ["cw-lift-5g-two-nodes"]
    usable_indexes()            : ["cw-lift-5g-two-nodes"]
    leader-only term (installed): Some(true)
    peer-only term (installed)  : Some(true)    ← THE BAR
    canonical chunk_count       : 4

*Merge succeeds, finalize fails.* Reported as its own fact, not folded into
either neighbour (§18.3): `RecoveryOutcome::MergedButNotInstalled { chunks,
canonical_path, error }`, with an `error!` at the `auto_ingest` arm. `Recovered`
would claim a built canonical, which is the defect above verbatim; `Failed`
would claim nothing was produced, and `merge_participants` has by then deleted
every source partition, so the chunks in that directory are the only copy. The
canonical is deliberately left in place for that reason. The consequence,
stated rather than discovered: the next tick short-circuits on
`AlreadyHasCanonical` (it tests for the meta the merge already wrote), so
nothing retries the finalize. Making the finalize a repair loop would have to
tell "finalize failed here" apart from "another writer is mid-ingest", and that
is a separate decision this rung does not make.

*Not fixed, named:* `coordinate_merge` shares `merge_participants` and has the
identical gap — a queue-mode merge produces the same unfinalized canonical.
Left alone on purpose (§10.2): fixing it inside the shared function would move
the legacy path's behaviour as a side effect of this rung.

**B5 and B7 under the correction, checked.**

*B5's merge clause — affected, same correction, no change to the verdict.*
`the_merge_proceeds_with_the_slices_that_exist` asserts reachability through
`probe_canonical`, the same `CorpusIndex::open` probe B2's first reading used,
so its "both donors' terms reachable" was true on disk and overstated at the
user's altitude for the same reason. The clause's own question — "does the
merge proceed with the slices that exist" — is answered either way, and it
still passes. Measured rather than reasoned: the corpora this fixture builds
through `merge_from_fold_coverage` now carry `ingestion_in_progress: false,
indexes_built: true` (printed by the corpus-clause reading below, on the same
fixture). Its assertions are unchanged; its docs now name the altitude and
point at B2 for the installed-altitude reading of the same merge path.

*B5's corpus clause — STILL NOT MET, re-measured after the fix, and one field
sharper.* Re-run with `--run-ignored`: both `_corpus_meta.json` files are still
equal field for field, so the bar is unmoved and the `#[ignore]` stays. What
changed is that the indistinguishability is now REACHABLE — previously it was
blocked by this very defect, since neither corpus was advertised at all. Both
now carry `indexes_built: true`, both reach `installed_indexes()`, and
`canonical_fingerprint` — `None` on both before — is now the same non-null hash
on both:

    canonical_fingerprint : 48cecea964a5708290d88f724e33f9b96613d25587f157e25944944e2ad8b9d6
    (identical on the 2-of-3 corpus and the 2-of-2 corpus)

A peer choosing which canonical to pull by fingerprint now reads a corpus
missing a slice and a whole one as the same corpus. That is the same bar, one
notch sharper, and it argues for closing B5's corpus clause sooner rather than
later.

*B7 — untouched, checked.* Its refusal case writes no canonical (the merge
returns `Err(IncompleteCoverage)` before `merge_partitions` is reached), so no
finalize runs and the assertion "canonical: ABSENT" is unaffected. Its paired
positive and reading 2's control DO merge, and now finalize as well; both
assert on the `RecoveryOutcome` variant and the disk-level probe, neither of
which moved. Reading 3 is about `total_shards` on the disk path and does not
touch this. All ten tests across the three `fold_ingest_*` files pass, exit 0.

**B3 — MET, 2026-09-09.** `only_the_submitter_reads_a_merge_out_of_the_fold`.
The peer folds the SAME journal and asks the same question; `fold_coverage_for`
returns `None`. The paired positive control in the same test — the submitter
DOES get an answer from that same projection — is what stops the refusal being
"a function that always declines".

The pre-registered instrument is in place and asserted, not merely present.
`fold_coverage_for` now emits a `debug` event on the decline naming the
decision and BOTH keys it compared:

    fold_coverage_for: declining — this node is not the submitter of this
    ingest handoff, so another node leads its merge
      handoff=… submitter=<leader actor> self_key=<peer actor>

Watched red by disabling the `&handoff.submitter != self_key` arm: the peer
comes back with `Some(FoldCoverage { nodes: [node-11.., node-22..], expected:
2, abandoned: [] })` — the leader's coverage, verbatim. Two nodes, one output
directory.

Known not checked by B3: that two nodes running the real `auto_ingest` loop
concurrently do not race. This is the leader DECISION, which is where the race
is prevented; it is not a concurrency test.

**B4 — the bar is MET and the mechanism this pre-registration named is NOT the
one that holds it. 2026-09-09.**
`commonwealth-knowledge/tests/main/merge_participants_idempotence.rs`, driving
`ShardManager::merge_participants` directly — going through
`merge_from_fold_coverage` would have measured its `AlreadyHasCanonical`
short-circuit, which says the second run declined, not that a second run would
have been safe.

*The bar.* `a_second_merge_of_the_same_handoff_leaves_the_canonical_untouched`.
Two merges of the same participant set. The second run is not a no-op: the
local partition dir was cleaned up by run one, so `merge_participants` resolves
this node's shard through its `original_path` fallback — the canonical itself —
and re-pulls the peer's shard down the socket. Chunk count 2 → 2, both rows
still reachable. **Met.**

*How it is met.* Not by dedupe. The second run returns

    Err(Database("Table 'chunks' already exists"))

and leaves the canonical exactly as it was. `merge_shards` builds its output
with `CorpusIndex::create` → `create_empty_table`, which refuses a directory
already holding a `chunks` table, so the merge never reaches the dedupe at all.
This falsifies `corpus-engine/src/sharding.rs`'s own comment on the
single-shard fast path — "callers that actually do want to fold a partition
into an existing canonical fall through to the full merge below (which dedupes
via `content_hash`)". The fall-through errors first.
`CorpusIndex::create_or_resume` documents this exact LanceDB failure and works
around it; `merge_shards` does not. NOT FIXED HERE: whether folding into a live
canonical is a capability or a refusal is corpus-engine's decision, and the
refusal is the safe half.

Watched red by making `CorpusIndex::create_with_sharing` clear an existing
directory first — the obvious "fix" for that error. Run two then deletes the
canonical it had just named as its own local shard, and reports success over
the wreckage:

    the second delivery changed the canonical's chunk count: 2 → 0
    Second run returned: Ok(Some(IndexInfo { chunk_count: 1, … }))

That is why the bar is asserted on the CORPUS and not on the return value.

*The dedupe, measured where it is reachable.*
`the_merge_dedupes_a_row_two_donors_both_contributed`. One merge, three shards,
four input rows, two of them the same `content_hash` from two different donors.
Canonical holds three, all reachable. This is the mechanism
`IngestExecutor`'s `Idempotency::Idempotent` is declared on and it is real —
it just cannot be reached by running the merge twice. Watched red by deleting
the `seen_hashes.contains(h)` early return: `left 4, right 3`.

Known not checked by B4: the fold (no journal — the handoff is an id); the
SECONDARY `(unit_id, source_doc_id)` dedupe key, since these rows carry a
populated `content_hash`; concurrent merges; and whether the refused second run
leaves its re-pulled shard directory on disk (it does — `merge_participants`
cleans up only after a successful merge — which is why the caller's
`AlreadyHasCanonical` short-circuit is load-bearing rather than an
optimisation).

**A hazard confirmed NOT live in production, 2026-09-09.** `NodeId`'s `Display`
is `node-<hex of the first EIGHT bytes>` and every partition directory name is
built from it, so two nodes sharing a 64-bit prefix would collide on one
directory and the merge would silently see fewer shards. Every production
`NodeId` comes from `NodeId::generate()` — 16 CSPRNG bytes via `getrandom`
(`kernel-types/src/ids.rs`) — reached through
`persist::load_or_generate_self_node_id` and `membership::init_mesh*`; every
`NodeId::from_u128` call site in the workspace is inside a `mod tests`. So the
truncation leaves 64 random bits and a collision needs a birthday collision at
that width: about `n²/2^65`, which is ~3e-14 at a thousand nodes. The hazard is
real in FIXTURES, where ids are minted from small integers, and both test files
say so at the point where the ids are chosen. Not fixed, and no fix is owed.

---

**B5 — the merge clause is MET; THE CORPUS CLAUSE IS MEASURED AND NOT MET.
2026-09-09.** `sovereign-mesh/tests/main/fold_ingest_abandoned_unit_e2e.rs`.

The bar has two clauses and only one of them holds. "The merge must proceed
with what exists" — met. "AND the corpus must record that it is partial" — not
met, and not by a small margin: the canonical's `_corpus_meta.json` is
identical, field for field, to one built from a handoff that delivered
everything.

*The fixture.* One handoff, THREE `ingest:v1` units, real Ed25519 ops through
`commonwealth_rail::admit` → `WorkProjection::fold`. Two units completed by two
different lessees; the third leased three times by real `Lease` acts, each
after the previous `LEASE_MS` window closed, and never reported at all. The
`Failed { outcome: None }` is therefore DERIVED by `ProjectedUnit::status_at`
(`projection.rs:237`) from a journal a mesh would actually accumulate, not
hand-written. Asserted as a precondition, not assumed: `attempts ==
MAX_UNIT_ATTEMPTS`, `outcome.is_none()`, handoff `Complete`.

*Clause one — the fold names it.*
`a_unit_whose_attempts_are_spent_is_named_in_the_coverage`.

    handoff_id : the submitted handoff, asserted equal to it
    expected   : 2                       (the two VERIFIED lessees; the
                                           abandoned unit has no lessee and
                                           does not inflate the denominator)
    nodes      : [node-1100000000000000, node-2200000000000000]
    abandoned  : ["01f4c6c1…e72dd"]      (is_partial() == true)

Watched red by deleting the `WorkUnitStatus::Failed { .. } => abandoned.push(…)`
arm — the `_ => {}` below it swallows the unit: `left: [], right: ["01f4c6c1…"]`.

*Clause one — the merge proceeds.*
`the_merge_proceeds_with_the_slices_that_exist`, over the same real socket B2
uses. `Recovered { chunks: 4 }`, both donors' terms reachable. Watched red by
counting the abandoned unit into `expected` (`actors.len() + abandoned.len()`):
`PartitionsUnreachable { covered: 2, expected: 3 }`, no canonical, every tick,
forever — an abandoned unit is work that never happened, not a missing
partition, and conflating the two strands the corpus over a slice no retry can
produce.

*Clause two — NOT MET.*
`a_corpus_missing_an_abandoned_slice_records_nothing_that_says_so`, `#[ignore]`d
with B5's number on it. Two corpora on one node, merged the same way from the
same fold: one from the three-unit handoff above, one from B2's two-unit
handoff. Both `_corpus_meta.json` files, with `corpus_id` / `corpus_name` /
`created_at` / `last_updated` blanked, are EQUAL — and so is every `IndexInfo`
field `build_hosted_corpora` (`capabilities.rs:285-310`) copies onto the wire:

    partial : query_sharing=true is_shard=false chunk_range=None
              chunk_count=4 total_shards=None processed_shards=[]
    whole   : query_sharing=true is_shard=false chunk_range=None
              chunk_count=4 total_shards=None processed_shards=[]

**The chunk counts are equal and that is the sharpest form of it**: a
three-unit handoff that delivered two slices is byte-identical to a two-unit
handoff that delivered both of its own. A reading that compared 1-of-2 against
2-of-2 would find different counts and could be talked into calling that a
signal; it is not one, because no peer knows what the count should have been.

The only record of the abandonment is the `tracing::warn!` in `auto_ingest`'s
arm — process-local, gone on the next restart, invisible to every peer. The
shape a signal would take already exists (`CorpusShardInfo.total_shards` /
`processed_shards`, and `coverage_ratio()` over them) and is `None`/`[]` for
every fold-sliced recipe, because the only production writer of `total_shards`
is `ingest.rs:718` under `ExtractorConfig::WikipediaJsonl`.

The test asserts the BAR and is ignored, rather than asserting today's
behaviour: pinning the indistinguishability would pass forever and go red the
day somebody fixes it. The ignore marks an OPEN bar, the way `50238f364`
marked B1's.

**A SEPARATE DEFECT FOUND WHILE MEASURING B5, NOT FIXED — since fixed, see
"B2 — THE CORRECTION" above; the reading below is kept as it was written,
because it is also the record of B2's first green being taken at the wrong
altitude and of that being noticed here and not acted on.** A canonical built by
the fold path is not advertised at all. `merge_participants` →
`CorpusEngine::merge_partitions` → `sharding::merge_shards` creates the output
with `CorpusIndex::create` and never clears `ingestion_in_progress`; the DISK
path's `merge_partitions_into_canonical` does (`sharding.rs:1424`). So the
fold-built canonical carries `ingestion_in_progress: true, indexes_built:
false`, and `CorpusEngine::installed_indexes` skips it — measured on the disk
this test builds:

    installed_indexes rows = 0 -> []
    hosted_corpora         = []
    canonical dirs on disk = ["cw-lift-5g-record-whole", "cw-lift-5g-record-partial"]

That means B2's green overstates the end-user reading: the merged corpus is
searchable through `CorpusIndex::open` (which is what the probe does) and NOT
through `usable_indexes`, which requires `indexes_built`. It also means the
"advertised indistinguishably" hazard is currently blocked by an unrelated bug
rather than absent — the moment the canonical becomes advertisable, the two
records above are what a peer receives. Same class as B4's `Table 'chunks'
already exists`: the merge's post-conditions are corpus-engine's decision, and
this lane records it rather than guessing at it. `coordinate_merge` shares
`merge_participants` verbatim, so this is not new with the fold.

Known not checked by B5: two real machines; the ingest pipeline; whether a peer
would behave differently given a completeness signal (that is the fix's bar);
and the actor-vs-host rule, which this fixture still cannot witness for B2's
reason.

---

**B7 — MET, in all three of the parts that were open. 2026-09-09.**
`sovereign-mesh/tests/main/fold_ingest_coverage_refusal_e2e.rs`.

*1. The guard is armed on the FOLD path.*
`a_two_donor_fold_missing_its_peer_refuses_and_writes_no_canonical`. B2's
scenario with the peer's socket never spawned — the peer is a mesh member at a
port bound and released, so it is genuinely dead rather than guessed.

    merge outcome  : PartitionsUnreachable { covered: 1, expected: 2 }
    canonical      : ABSENT                      ← THE BAR
    local partition: still on disk

The last line is not cosmetic: `merge_participants` deletes every resolved
shard dir after a successful merge, so a refusal that ran the cleanup would
destroy the half of the corpus that does exist while reporting only that
coverage was short.

Watched red by disabling the `shard_dirs.len() < expected` arm in
`merge_participants`: `Recovered { chunks: 2, shards_covered: 2 }` and a
canonical holding half the corpus. The test also carries its own paired
positive — the same disk and the same dead peer merged with `expected = 1`
produces exactly that partial canonical — because a refusal that never merges
anything is indistinguishable from a merge path that is simply broken.

*2. THE `continue`, which was the point.*
`the_folds_refusal_is_final_and_the_disk_path_never_runs`, driven through the
REAL `sovereign_mesh::auto_ingest::spawn_auto_collaborate_loop` — the claim is
about the ORDER of that loop's arms and nothing smaller can see an order. The
bar is asserted only after the arm's own REFUSED warning is observed, so
"nothing happened" cannot pass as "the refusal held".

Its control, `without_a_fold_the_same_tick_publishes_the_partial_canonical`, is
the same disk and the same loop with no rail installed: `fold_now` returns
`None`, the tick falls through, and a 1-of-2 canonical lands with the peer's
term unreachable. Without it, "no canonical appeared" would also be what a loop
that never reached the stranded scan looks like.

Watched red by deleting the `continue` at the end of the fold arm. The trace is
the bug verbatim, in order, in one tick:

    WARN  merge_participants: refusing to merge — coverage is incomplete
          covered=1 expected=2 missing=[node-2200000000000000]
    WARN  auto_ingest: REFUSED — merging now would publish a partial canonical
    INFO  auto_recover: attempting stranded-partition merge into canonical
          partition_count=1
    INFO  auto_recover: chunk-merge phase complete chunks_merged=2
    INFO  auto_recover: canonical meta stamped
    → canonical chunk_count 2, peer-only term unreachable

*3. The `total_shards` premise.*
`the_older_disk_guard_is_dark_without_a_total_shards_stamp`. Confirmed at HEAD:
the only production caller of `CorpusIndex::set_total_shards` outside
`sharding.rs`'s merge-replay is `corpus-engine/src/engine/ingest.rs:718`, inside
`if let ExtractorConfig::WikipediaJsonl { .. }`. Measured rather than cited: a
partition written the way a fold unit writes one carries no `total_shards`, and
`try_recover_stranded_partitions` merges a 1-of-2 canonical without returning
`IncompleteCoverage`. Stamp the field by hand on the same disk and the same
call refuses (`IncompleteCoverage { total: 2 }`) with no canonical left behind.
Watched red by disabling `shard_union.len() < n`: the armed half comes back
`Recovered { chunks: 2 }`.

**The useful finding either way:** for a FOLD-driven merge the `total_shards`
gap is irrelevant, because `merge_from_fold_coverage` arms
`expected_partitions` from the handoff's own verified-donor count and never
consults `total_shards` at all. The gap stays open for every merge that still
comes off local disk — which is where reading 3's first half lands, and which
the `continue` in reading 2 is what keeps the fold path away from.

**A mis-report found and not fixed.** `RecoveryOutcome::Recovered.shards_covered`
is `participants.len()` — the number the merge was ASKED for, not the number it
resolved. Visible in the watched red above: `Recovered { chunks: 2,
shards_covered: 2 }` after one of the two participants was unreachable. It is
only a log field today, and every caller that acts on coverage acts on
`PartitionsUnreachable` instead, but it is a count derived from the request
rather than from what happened (ARCH §18.1).

Known not checked by B7: two real machines; the ingest pipeline; two nodes
ticking concurrently (that is B3's decision, not a concurrency test); the
peer-canonical pull arm between the two, which needs a gossip advertisement
this fixture has none of; and any tick after the first.
