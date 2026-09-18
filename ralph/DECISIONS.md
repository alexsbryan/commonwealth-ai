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

## 2026-09-17 · REVIEW-build-rd-1-instrument · the pre-registration run read exit=1 on three bars

The package (`ralph/NEEDS_HUMAN.md`, removed in this commit) named three forks.
Each fact below was reproduced in this session, not taken from the package.

### The instrument row itself. Choice: `[x]` at 5ab927237.

The row's check says "the FIRST run is the pre-registration; its numbers are
recorded, not tuned to", and order step 7 says the measurement is
PRE-REGISTERED before the run. A pre-registration is done when it is recorded,
which 5ab927237 did. Five PASSED is what `REVIEW-DEMO-rd-1-run` expects, and
that row keeps its bar. Holding the instrument at `[~]` for exit=0 would have
the instrument owe the product's result.

*Falsified if* the instrument itself is wrong — a bar it misreads rather than a
product gap it reports. None of the three non-passes is that (below).

### Fork 3 — attribution disagrees across nodes. Choice: mint `rd-1-attribution-order`.

`createAttribution().absorb` skips seen ids and credits in arrival order
(`sovereign/apps/ring-doc/adapter.js:182-190`, called once per poll at
`app.js:222`), while the rail's total order is `(ts_unix, actor, seq, id)` with
second-resolution `ts` (`commonwealth-rail-core/src/admit.rs:34,443`). Two pages
that saw the same acts in different arrival orders can name different people,
which is what node b did. Order step 3 already says acts apply "in the rail's
order", and Demo step 3 has all three screens agree, so "latest" means latest in
rail order, and the adapter is what is wrong. Reading "latest" as arrival order
would change the bar's oracle. The cause is read from the code and was not
re-observed from the run's log (`up` empties the run dir). The new row's test
must fail on the current adapter first. That is where the cause gets confirmed.

*Falsified if* that test passes on the current adapter. Then the ordering is not
the cause, and the row goes back to instrumenting the run.

### Fork 2 — `commonwealth-rail*` diff. Choice: no bar change, no hakari exclusion. It clears on push.

The package called the lines uncommitted. They are committed now:
`git diff --stat origin/main -- 'commonwealth/crates/commonwealth-rail*'` is
exactly three `+workspace-hack = { … }` lines, `git log origin/main..HEAD` on
those paths is 44f9a1bdc alone, and the worktree equals 44f9a1bdc there. The bar
reads "zero diffs against origin/main". It reads 0.0 because a peer campaign's
commit is local and not yet public, not because the rail learned anything. Once
44f9a1bdc is pushed, the diff is empty and the bar is unchanged. The package's other
options are each worse. Narrowing to `src/` weakens a floor_basis, which is the
operator's. Excluding the rail crates from hakari reaches into the other
campaign's work. There is no hakari-free tree to run the demo in on `main`.
The push is the operator's, so it is named in the HUMAN row below.

*Falsified if* 44f9a1bdc is dropped or reshaped before the push. Then this fork
reopens as the package framed it.

### Fork 1 — the live lane is refused to every guest. Choice: the operator's. `HUMAN-rd-1-live-grant`.

Reproduced: `Scope::Rails(_) => &["/v1/rail/append", "/v1/rail/log"]`
(`sovereign-grants/src/guest_grant.rs:105`). The package did not raise one
thing, and it keeps this fork away from the director: `/v1/rail/live` has **no
namespace** ("No namespace: the buffer is one per daemon",
`sovereign-api/src/routes_rail_live.rs:255`), and the drain is destructive. So
the one-line fix the package proposed would let ANY rail-scoped guest link,
including one sent to a guest of another app, read and drain every app's
presence on that daemon. That changes what a link handed to a guest grants,
against the `Scope::Rails` doc's own one-namespace rule (`guest_grant.rs:84-87`).
The charter leaves that to the operator. The options and a recommendation
(namespace the lane, then grant it) are in the row. The loop runs
`rd-1-attribution-order` first, then stops at the HUMAN row with the package
the row names. `ra-doc-live-lane-non-durable` is COULD-NOT-JUDGE and not FAILED
because no cursor sample ever arrived, and that is consistent with the refusal
reproduced on all three proxies.

*Falsified if* guest grants are meant to be app-agnostic for the live lane,
e.g. the lane is decided to be a daemon-wide broadcast by design. Then option
(a) is right and this was a needless stop.

REVIEW-AFTER: whether the charter should name "a guest grant gains a path" as
the operator's explicitly. It was read here from "behaviour a peer can observe".

Commit: the one that removes `ralph/NEEDS_HUMAN.md`.

## 2026-09-18 · HUMAN-rd-1-live-grant · the operator's two answers

Weighed by the seat, decided by the operator in session ("sounds good").

### The live-lane grant. Choice: (b) namespace the lane, then grant it. `rd-1-live-namespace`.

A boundary question, held to the boundary the code already draws: the namespace lives on the
grant and never in the request (`guest_grant.rs:81-88`), and append/log resolve it from the
grant (`routes_rail.rs:85-99`). (a) would put an unscoped, destructively-drained route behind a
scoped grant — a privacy hole and, with two apps on one daemon, a correctness bug (one app's
poll eats the other's cursors). (c) moves trust into the dev proxy and leaves the route
unscoped. (b) makes the lane the same shape as its siblings. Two refinements written into the
row: an envelope for a namespace the daemon holds no grant for is refused with a reason, which
is what bounds memory; and the mounted-paths test must see the new path.

### The converge bar. Choice: no push tonight, no bar change; the instrument names the foreign commits. `rd-1-instrument-rail-diff`.

A ruler question. The bar means "this campaign did not change the rail"; the leg measures the
diff against origin/main, which conflates "changed by us" with "not yet pushed by anyone".
Pushing 57 commits is a release of the shared branch and is decided on its own merits, not to
clear a bar. The leg keeps its diff exactly as demanding and gains the four-verdict discipline:
a non-empty diff made only of commits outside this campaign reads COULD-NOT-JUDGE naming them,
never FAILED, and never PASSED.

## 2026-09-18 · rd-1-three-containers · three machines rehearsed as three containers first

Operator direction in session ("Mint it"). The one-host instrument already runs three real
daemons on real iroh; what it lacks of "three machines" is three network identities, a real
network cut, and three browser tabs on three addresses. Containers on one podman network give
exactly that; VMs would add a kernel each and nothing the demo exercises. Reuse: MESH_QA.md
designed a podman backend for the mesh soak and never built it — this is that seam, once.
Premises checked on this host 2026-09-18: rootless `podman network create` works; a container
on the toolbox image resolves host.containers.internal but the loopback-bound house daemon on
:9741 answers 000, so the row makes "boots with entry unreachable" a bring-up check.
The rehearsal (HUMAN-rd-1-three-tabs) does not retire HUMAN-rd-1-three-machines: the Mac's own
build and the WAN relay path are that row's claim.

## 2026-09-18 · rd-1-instrument-rail-diff · director, resolution 1

### Fork 1 — the row's DEMO cannot read five PASSED. Choice: mark it done at f181b179e.

The operator's no-push decision makes converge COULD-NOT-JUDGE while 44f9a1bdc (build-latency,
the only commit behind `git log origin/main..HEAD -- 'commonwealth/crates/commonwealth-rail*'`,
reproduced) is unpushed, and the row's own check asks for exactly that reading. f181b179e touches
only `scripts/ring-doc-demo.sh` (census + report), so it cannot move any other row. The same
premise was false in `REVIEW-DEMO-rd-1-run` ("five PASSED"); its expectation now accepts converge
COULD-NOT-JUDGE naming only foreign commits. *Falsified if* the census names an `rd-1-`/`REVIEW-`/
`ralph`/`ring-doc` commit and the row still reads COULD-NOT-JUDGE.

### Fork 2 — partition-drill "regressed". Choice: not a regression; a masked failure. New row `rd-1-partition-gap`, before `rd-1-tune`.

The pre-registration PASS (12/12 on a, b, c) was the live lane's refusal: 5ab927237's body records
every `/v1/rail/live` call answering out_of_scope, and that error sits in `liveGaps`, which the
panel includes (`scripts/ring-doc-demo.sh:387`, `A/app.js:240-243`) — so every page's panel was
non-empty for the whole run regardless of C. With the lane working (09ba44764), the current
session.json reads a 0/12, b 0/12, c 12/12, and c's only text is its own drain error. Nothing on
a or b names C: the rail reports a hole only after a later act arrives, and `sendPresence` drops
the live POST's per-peer `PeerDelivery` report (`routes_rail_live.rs:142-152`), whose doc says it
exists so the page can show a half-up lane. Order step 5 already says A's and B's panels name C;
the row makes the page say it, instrumenting first, with a §6 exit if C is absent from `peers`
rather than `delivered: false`. Bar, floor and `panels_ok` untouched (ARCH 5: a gate never
watched fail for the right reason). *Falsified if* the instrument shows a or b's panel did name
C in a run with the live lane refused — i.e. the pass had a second source.

REVIEW-AFTER: whether naming an undelivered peer in the gap panel is "behaviour a user can
observe beyond the row". Read here as the order's own step 5, not new behaviour.

Commit: the one that removes `ralph/NEEDS_HUMAN.md`.
