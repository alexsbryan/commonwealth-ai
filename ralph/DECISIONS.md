# ralph director decisions

One entry per resolution, one commit per entry, so `git revert <sha>` undoes
exactly it. Format: date · unit · the fork · the choice · the evidence · what
would falsify it.

## 2026-09-17 · REVIEW-build-rd-1-live · the live lane's crate, its PLANT, and its census row

Three forks came up in `ralph/NEEDS_HUMAN.md`. All three are the charter's, so
all three are decided here; the row at `ralph/next/ring-doc/STATE.md:39` is
rewritten to match and returned to `[ ]`.

### Fork 1 — where `push_ephemeral` lives. Choice: all of it in `sovereign-api`.

The row as written was unbuildable, and the package is right about why.
`sovereign/crates/sovereign-mesh/Cargo.toml:54` depends on `sovereign-api`;
`sovereign/crates/sovereign-api/Cargo.toml` has no `sovereign-mesh` line
(reproduced: `grep -n sovereign-mesh …/sovereign-api/Cargo.toml` prints only
`sovereign-meshapp-registry` at :22 and a comment at :132). So a client route
in `sovereign-api` cannot call a helper in `sovereign-mesh`, and the 256-entry
buffer cannot be typed in `sovereign-mesh` while living on `sovereign-api`'s
`AppState` (`state.rs:747`).

Of the package's three ways out I take **1a**, over 1b's `Arc<dyn …>` seam on
`AppState`: principle 11 (prove what exists cannot serve before you build new)
and principle 8 (the existing decider over a new one). The fan-out already
exists in `sovereign-api` in the shape the row asks for —
`routes_internal/pipeline_pause.rs:302` `forward_to_peers` reads
`state.inner.mesh`, filters `node_id != self && status == Online`, and fans out
over `state.peer_transport()`, the same three moves as `gossip.rs`
`announce_presence_change:925-970` one crate up. 1b adds a trait object and an
install site that neither the row nor the order names; 1a adds nothing and
drops two files from the row (`sovereign-mesh/src/ring_live.rs` and the
`gossip.rs` `online_peer_contacts` edit), leaving `gossip.rs` untouched.

The order permits it. O1 Scope (`order.md:133-136`) already places "the live
client routes" in `sovereign-api/src/routes_rail.rs` and makes the mesh-side
module conditional ("**or** a new sibling module", "`state.rs` **if** the ring
buffer lives on AppState"); the buffer does live on AppState, and AppState is
sovereign-api's.

Branch-merge cost is unchanged, not reduced: O1 Seams (`order.md:191-201`)
assigned the `inner.mesh` → `inner.fabric.mesh` one-liner to the gossip helper;
it moves to `routes_rail_live.rs`, the same single line `pipeline_pause.rs:303`
needs on `origin/ralph/domains-campaign`. One file, one line, either way.

*Falsified if* `sovereign-api` turns out to need something only `sovereign-mesh`
exports to do the push — in which case 1b is the fallback and the seam is
argued on its own.

### Fork 2 — the PLANT that could not go red. Choice: the row gains the test it was missing (2a).

Reproduced: `replication_sender_census.rs:110-112` scans only for the routes
named in `REPLICATION_SENDERS` (:36-45, one row, `/internal/ring/sync`), and
its own header (:131-139) says the only sabotage it can see is a SECOND
URL-join site on the surviving route. A `store.set(...)` inside a handler
changes no such site, so the row's PLANT was green by construction — PROMPT §5
names that §6, "the enforcement does not enforce".

O1 step 4 (`order.md:98-100`) already says which test is watched failing: "the
replication-sender census **stays green**, and **a restart empties the
buffer**". The census is the positive control; the missing half is a
non-durability test, and the row named no file for it. The row now adds
`sovereign-mesh/tests/main/ring_live_non_durable.rs` in the in-process harness
shape of `ring_append_nudges_sync.rs` (`AppState` + the real
`client_router`/`internal_router` on real sockets), with two tests: a live
payload leaves every file under the rail dir byte-identical while
`GET /v1/rail/live` still returns it (the second clause is the vacuity guard),
and a fresh `AppState` over the same dir drains empty. The PLANT becomes
"append the payload as an act to `state.ring_rail()`'s journal", which the
on-disk snapshot sees.

*Falsified if* the snapshot proves flaky — something else writes under the rail
dir during the test. The row pins no ring-sync loop for exactly that reason; if
it still moves, narrow the assertion to the `NS` journal's op count, which
`ring_append_nudges_sync.rs` already has a helper for.

### Fork 3 — a `REPLICATION_SENDERS` row for `/internal/ring/live`. Choice: no row. `REVIEW-AFTER:`

The order decides this one and the package did not read that far. O1 Seams
(`order.md:182-183`): "The live lane lands in NO store. The replication census
is the proof and it is run, not remembered" — the census proves it by staying
green, which it only does if the live route is absent from the table. The
table's subject is stated in its own doc (`:22-23`): "every production site
that puts **replicated state** on the wire". A payload that lands in no store
and no journal is delivery, not record, which is this campaign's whole
predicate. Declaring it would make the instrument answer a different question
than the one it names.

Tagged `REVIEW-AFTER:` because the table's other sentence (:34, "a new row here
is a review moment, never a silent pass") supports the opposite reading, and
because the consequence of not declaring is real: with no row, the census
counts zero sites on `/internal/ring/live`, so nothing stops a second live-push
site appearing later. If the operator wants that ratchet, the row is one line —
but the table then needs its subject widened from "replicated state" to "state
on the wire", and that is a rename of the instrument, not an addition to it.

*Falsified if* a live payload is ever found in a store or a journal. Then the
lane is replicated state, the row is owed, and
`ring_live_non_durable.rs` is the test that should have caught it first.

Commit: recorded in the same commit as the row rewrite and the removal of
`ralph/NEEDS_HUMAN.md`.
