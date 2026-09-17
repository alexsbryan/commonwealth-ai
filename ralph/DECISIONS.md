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

## 2026-09-17 · rd-1-awareness · the page cannot reach `/v1/rail/live` under `svrn ring dev`

The unit is done and committed (`d572d8f3d`); nothing is broken. What stopped
the loop is that the transport the row names is not reachable from the page the
demo opens, and the fix lives in files no row named. One fork, decided here,
plus the sub-fork the package correctly called the substance. `rd-1-live-shim`
is added to `ralph/next/ring-doc/STATE.md` and carries both.

### Fork 1 — proxy the live lane, or move the demo off `svrn ring dev`. Choice: proxy it.

Reproduced: `svrn ring dev` routes exactly three things
(`ring_cmd/dev.rs:87-91`) — `POST /__ring/{op}`, the shim, and a static
fallback — and the op table answers two ops with a 404 for anything else
(`:140-166`). So a page `fetch("/v1/rail/live")` (`A/app.js:136`, `:172`)
lands on `static_handler` and 404s, which the committed page renders honestly
as `presence not read: /v1/rail/live answered 404`. The rail itself has three
routes since `6ac1fd39f` (`sovereign-api/src/server.rs:262-272`).

The order settles it without a new judgement: O1's Demo step 1
(`order.md:51`) is "Three browser tabs, one per machine, `svrn ring dev
ring-doc` on each", and step 5 (`STATE.md:40`) puts awareness on `POST/GET
/v1/rail/live`. Both cannot be true unless the dev server carries the lane.
The package's option 3 — serve the app same-origin with the rail listener —
would rewrite that Demo step, which is the operator's, and would also hand the
browser a page on the `UNTRUSTED_LOOPBACK` bind the proxy exists to keep the
grant token off (`dev.rs:72-77`).

The shim's own doc comment (`dev.rs:115-124`) pre-authorised this: "a third arm
here would mean the rail had grown a third route, and that is where the
decision belongs." The condition is met, so the comment is rewritten rather
than worked around.

*Falsified if* the day-6 demo is decided to run from somewhere other than
`svrn ring dev` — then this row is dead code and O1's Demo step 1 is what
changed.

### Sub-fork — the drain is a GET and `op_handler` is POST-only. Choice: two POST ops, no router change.

Three ways were open: make the route `any(...)`; spend one POST op on both
directions with a direction field in the body; or name two ops.

Two ops. The direction field is impossible, not merely worse: the push body
reaches the daemon verbatim and is read as opaque text
(`routes_rail_live.rs:253-258`), so there is nowhere in it to put a field
without the daemon having to parse a payload it promises not to look inside.
And `any(...)` is unnecessary, because the existing table already proves the
shape — `"log"` is a browser POST that carries an upstream GET (`:141-147`).
A drain op is that same shape a second time, whereas widening the route would
additionally admit `GET /__ring/append`, a verb the proxy has no meaning for.

The four arms become one pure `upstream(op) -> Option<(Method, path, ctype)>`.
That is not cleanup for its own sake: it is the only way this row's PLANT can
be watched fail without standing up a proxy and a daemon (principle 5). It
also keeps one spelling of each path — `RAIL_LIVE_PATH` joins its two siblings
in `sovereign-cli-shared/src/rail.rs:45-46`, which exist for exactly this
reason. A four-arm `match` on string ids brushes principle 9; it stays a match
because the set is closed and compiled in, and it is now one named decider
rather than four inline ones.

The second half is a JS trap worth naming: the shim's `call` helper
`JSON.stringify`s its body (`dev.rs:215-218`), and `presenceEnvelope` already
returns a JSON STRING (`A/adapter.js:220-222`). Routing `live.send` through
`call` would double-encode, `decodePresence` would `JSON.parse` to a bare
string, `env.kind` would be `undefined`, and every payload would be skipped
SILENTLY (`adapter.js:232-240`) — a lane that answers 200 and shows no
cursors. So `live.send` is a raw `text/plain` fetch, and a test watches for
the regression.

*Falsified if* something later needs to GET through the proxy from a plain
`<a>` or an `<img>`, which a POST-only op cannot serve. Then the route becomes
`any(...)` and `upstream`'s method column is what it was already for.

Commit: recorded in the same commit as the new row and the removal of
`ralph/NEEDS_HUMAN.md`.
