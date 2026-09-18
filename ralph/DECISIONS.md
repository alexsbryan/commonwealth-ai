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

## 2026-09-17 — rd-1-partition-gap: the page never received `peers`

### Fork 1 — the row's EDIT is inert on the real page. Choice: widen the row to `dev.rs:276`.

Reproduced: `DEV_SHIM`'s `live.send` (`sovereign-cli-llm/src/ring_cmd/dev.rs:271-277`) ends
`return null`, while the daemon's POST answers `{bytes, peers, delivered}`
(`routes_rail_live.rs:333-337`) and the driver's mirror reads `body.peers` straight off `fetch`
(`scripts/ring-doc-demo.sh:352-354`). Editing `app.js` and the driver "identically" would pass the
demo while the page served by `svrn ring dev` still said nothing — the masked pass this row exists
to remove. `rd-1-live-shim` never specified a `null` return; it is an implementation choice, and
`PeerDelivery`'s own doc (`routes_rail_live.rs:145-148`) says the page is meant to see it. Smallest
fix: `return r.json()`, asserted in the existing shim string test rather than a new one.
Order step 5 ("the gap panel on A and B names C") implies it.

### Fork 2 — `pollLive` clears what `sendPresence` found. Choice: two variables, one owner each.

Reproduced: both `A/app.js:169` and the driver (`:376`) assign `liveGaps = read.gaps` every 250 ms,
so a delivery gap would show for under one drain. `deliveryGaps` (owned by `sendPresence`) and
`liveGaps` (owned by `pollLive`) both feed the panel (ARCH 12: each side owns its own finding).
No new type, no roster read.

### Fork 3 — C absent from `peers` after mesh marks it offline. Choice: no gap line; the leg reads "at least one sample".

`peer_c.a` in `target/ring-doc-demo/session.json` (run at 0fb1e725f): 11 × `error: … error sending
request`, then 1 × `absent`; b: 12 × the error. The row already forbids a roster diff, so once C
leaves `peers` the page honestly knows nothing further. The positive control's during-split leg is
read as at least one sample naming C on each of a and b — the reading `panels_ok` already uses
(`scripts/ring-doc-demo.sh:690`, `v > 0`); the pre-split-empty leg is unchanged.

*Falsified if* the edited page served by `svrn ring dev` (not the driver) shows no C line during a
split while the driver's mirror does — the shim and the mirror diverged again; or if a split run
shows C `absent` from a's and b's `peers` on every sample, which makes Fork 3's leg unpassable
without a roster diff and goes back to the operator.

REVIEW-AFTER: Fork 3's "at least one sample" reading — a stricter "every sample" bar would need
the roster diff the row forbids.

Commit: the one that removes `ralph/NEEDS_HUMAN.md`.

## 2026-09-18 · rd-1-three-containers · the seam is one door; the forwarder is the instrument's

The worker's §6 (03:06Z) showed the row's premise false: `ring dev` binds loopback with no bind
flag (dev.rs:93), the rail never leaves loopback (rail_bind.rs:62), operator routes admit
loopback peers only (loopback_guard.rs:166). Decided by the seat: (1) no bind flag on
`svrn ring dev` — a LAN-reachable dev proxy hands the grant it holds to the LAN; the host
browser reaches a container's dev server through an in-container forwarder that is the
instrument's own component and stands in for 'the browser on that machine'. (2) Every
command that runs on or talks to a node goes through `sv`/`node_exec`/`node_curl`; the
row's earlier four-function seam was a list, not a door. (3) A's join address is per
backend. (4) `_cut`/`_heal` for phase 2; phase 4 keeps a real stop on both backends.

## 2026-09-18 · rd-1-three-containers · the join takes the product's no-VPN path on both backends

Worker §6 03:20Z: `relay=` is a POST to the founder's internal port (daemon.rs:1596-1607), which
is loopback-bound (ring-doc-demo.sh:191) — on podman B cannot reach it; on local it worked only
because three daemons share one loopback. Decided by the seat: option (i). The founder's
`/v1/mesh/status` already serves `join_link` with the live `dial=` (current_invite,
daemon.rs:1952; mesh_http.rs:504); B and C join with that link and the daemon key-dials the
founder over iroh. Same code on both backends (decision 2: yes; local re-run is the proof).
Not taken: `internal_bind = 0.0.0.0` — tests a path the Mac will never take and moves a
loopback pin. Recorded for the audit: `mesh rotate` prints the link without `dial=`.

## 2026-09-18 · REVIEW-audit-rd-1 · the row closes on the campaign's share; foreign reds go to the operator

Fork: TESTALL (exit 100, 4 fail) and PREPUSH (arch-gate, env-gate) stay red after the audit
fixed everything the campaign owns (e88a71212). Choice: mark the row `[x]` on e88a71212 and
9dd7a0401, with the reading the DEMO rows already use for exit 4: a red that names only commits
outside this campaign does not block it. Package items 1-6 are not fixed here. Each is outside
ring-doc's scope, and fixing them would add scope the campaign rule forbids.

Evidence, reproduced by the director: the campaign's one census red is green
(`sovereign-test.sh --package sovereign-core --filter f26_egress_boundary_census` → pass 1
fail 0). Every other red traces to a commit that is an ancestor of the queue start d8cd7bb9f
(`git merge-base --is-ancestor`), and `git log d8cd7bb9f..HEAD -- <file>` is empty for each
named file: quote_verification.rs and the conformance tags (30293904f, 09-13);
ingest_failure_modes.rs (last touched db21b2f8d, 09-03, the behaviour it checks changed in
30293904f); cli-contract.toml:3571 citing `docs/internal/RING_APPLICATIONS.md`, which is
gitignored (`.gitignore:67`) and absent on this host (a3bd715f5, 09-13); env-flags.toml:43 and
:1662 both declare `SOVEREIGN_SIDECAR_FEATURES` (e3474619c, 09-14); AGENTS.md +399 bytes
(9272ac2ca, 09-13).

Left for the operator, because each one is outside this campaign or is a ratchet/baseline call:
regenerate the conformance tags; decide whether the test or the code is right at
ingest_failure_modes.rs:518; fix the host-dependent doc citation; pick which env-flags
declaration stays; cut or accept AGENTS.md; the size-gate accepts, including
`sovereign-mesh::tests` for ring-doc's two test files. Finding 5 (each `/v1/rail/*` path is
spelled in three places) stays carried in `ralph/REVIEW_FINDINGS.md` and gets no row. It does
not block `HUMAN-rd-1-three-machines`, and the shared const needs a crate-placement decision
bigger than this campaign's rule allows.

*Falsified if* any of those reds passes at d8cd7bb9f and fails only once a campaign commit is
applied, or a campaign commit turns out to touch one of the named files.

REVIEW-AFTER: until the operator decides items 1-5, PREPUSH is red for every campaign on main.

Commit: the one that removes `ralph/NEEDS_HUMAN.md`.

## 2026-09-18 — REVIEW-build-rr-1-roster-from-mesh (ring-room): ESCALATED, not decided

Fork: making the derived roster the default for app rings needs a hunk in
`commonwealth-rail/src/lib.rs` (roster_origin/roster have no default source, :153-205), or
nine hooks outside the row, or a reversal of the operator's 2026-09-18 decision. The director
did not choose. This is the campaign's first stop condition (`campaign.md:71-72`), and the
charter reserves any rail diff. Evidence was reproduced: `lib.rs:153-165,176-205,251-260`,
`sovereign-api/src/routes_rail.rs:123-140`, `routes_internal/ring_sync.rs:141`. The
recommendation (option 1, with bar `campaign.md:23` amended to name the hunk) is in
`ralph/NEEDS_HUMAN.md` §(e). *Falsified if* some sovereign-side site that every namespace's
first touch passes through turns out to exist. It would have to be one that installs the
source before `rail.roster()` is read. None was found at the two doors above.

REVIEW-AFTER: the operator picks among options 1-4 in the package.

## 2026-09-18 — OPERATOR — REVIEW-build-rr-1-roster-from-mesh (ring-room): option 1, the rail's roster door gains a default

Operator's words: "There's plenty of great thinking on permissioning and resource groups
especially as they have inheritance rules. I just want default to be that an app applies to
anyone in the mesh, then expose some primitives to limit." Fork: the worker's package
(ralph/NEEDS_HUMAN.md of 2026-09-18 17:27Z, options 1-3 in its §(c); the director's summary
is the entry above). Choice: option 1. The zero-rail-diff clause of ring-room's predicate is
lifted for ONE hunk, lib.rs:130-205 (default RosterSource; precedence registered >
roster.json > default). `roster add` is the narrowing primitive, so `refuse_derived_roster`
refuses only registered sources. Evidence: lib.rs:153-165 (exact-match map), :176-205 (file
fallback), sovereign-api/src/routes_rail.rs:123-139 (first append refused before disk).
Falsified if a sovereign-side site every namespace's first touch passes through exists that
installs the source before `rail.roster()` — none found at the two doors. Row rewritten in
d5dc6037e; predicate, bar and rung note in the commit that follows this entry.

## 2026-09-18 — seat (charter: fixing a row whose premise the tree contradicts) — REVIEW-build-rr-1-roster-from-mesh lands; the ring-doc instrument's classifier narrowed; one follow-up row

Fork (ralph/NEEDS_HUMAN.md 18:01Z): e94b26826 built and green on every gate; ring-doc's
`verdict all` read four PASSED and `ra-doc-three-machines-converge` FAILED naming e94b26826.
Choice: option 1 — the instrument, not the bar. f181b179e (operator) says a rail diff made
only by commits OUTSIDE ring-doc is COULD-NOT-JUDGE; scripts/ring-doc-demo.sh:832 classified
"ours" by a bare "REVIEW-" prefix, which every campaign's review rows carry. Now: a subject
containing "rd-1-" or "ring-doc". Evidence: the row's own subject "REVIEW-build-rr-1-…"; the
f181b179e row text. Falsified if a ring-doc commit exists whose subject carries neither token.
Row marked [x]. Worker's §(c)3 accepted as real: the six daemon namespaces lost their
registration, and a stray `rings/<ns>/roster.json` would now narrow a daemon ring (this
host has one stray file, ~/.svrnmesh/rings/work/roster.json, not a daemon namespace) —
minted `rr-1-daemon-rings-registered` (register the closed set with derive_roster; test:
file ignored for daemon rings, narrows app rings). Also for the operator: the worker tore
down a leftover podman ring-doc session from ~16:30 (ports 19849/59/69) that had collided
with its DEMO run — if that was your rehearsal, it is gone.

## 2026-09-18 — seat (charter: which of the options an order names; the smaller reversible step) — rr-1-citation-names-the-machine: wire half lands (1426c47f8), citation half is option B as a row

Fork: the worker's package (18:09Z), kept whole below. Choice: surface = EpistemicFooter's
released citations (the gate's ledger IS the citation; the prose fallback and the per-corpus
'via' line stay); join = option B — `ReleasedCitation.member` stamped by corpus in
`citations_of` from the existing `peer_attribution` map. Why not A: a per-chunk parallel vec
through ~10 files buys nothing while the fan-out asks ONE peer per corpus per turn
(routes_knowledge.rs:130-159), so per-corpus is already per-passage. Why not C: the
instrument reads the daemon's HTTP headless; a desktop-only join leaves it nothing to read,
and the bar's goodhart says the name must be on the citation. Falsified if a turn can
receive one corpus's passages from two members (then B mis-names the second's and A is
owed). Test lives in sovereign-core (citations_of) + a chat-ui node test; PLANT = drop the
stamp. Row `rr-1-citation-member-on-released` minted; the instrument row depends on it.

<details><summary>the worker's package</summary>

# NEEDS_HUMAN — rr-1-citation-names-the-machine (citation half)

## (a) Unit

`rr-1-citation-names-the-machine` in `ralph/next/ring-room/STATE.md` (left `[~]`).
The wire half is committed (see `git log -1 --grep rr-1-citation`): `peer_name`/
`peer_node_id` fields on `KnowledgeResult`, one write site in `fanout_one_peer`,
metadata keys retired, `knowledge_client` reads the field, `corpora_unhosted` +
`UnavailabilityReason::NotHosted`. CLEAN, LINT, PLANT, TEST(sovereign-api/mesh/core)
all green in that commit.

The row's remaining clause: "add the same name to the citation struct the passage
renders under and to the desktop's citation line as '<corpus> on <member>' (find
the one Svelte component that renders a citation)". The tree disagrees with its
premises in two places, so the choice is a design one and is not mine to make.

## (b) What I found

- There is no ONE citation component. Four render citation-ish lines on the
  desktop: `packages/chat-ui/src/components/EpistemicFooter.svelte:404-433`
  (the gate's released citations, `ledger.citations`), `SourceAttribution.svelte`
  (parses the prose `Sources:` block, used when no ledger), `AnswerProvenance.svelte`
  (flag-gated native-grounding segments), and `RoutingMeta.svelte` (the per-corpus
  summary, already "via <peer>").
- The only per-citation struct is `ReleasedCitation`
  (`sovereign/crates/sovereign-contracts/src/types/epistemic.rs:93`), projected from
  `kernel_types::Citation` by `EpistemicState::citations_of`
  (`epistemic.rs:66-78`), called once at
  `sovereign-core/src/runtime/grounding/inner.rs:498`. The gate there sees only
  `EvidenceContext` parallel vectors (`chunks`, `chunk_targets`, `chunk_custodies`,
  … `inner.rs:58-105`); no member name reaches it. Threading one means a new
  parallel vec through `grounding/mod.rs:225,317,380,410-429`, `gate.rs`,
  `handlers/knowledge_query.rs:1703-1743`, `handlers/simple.rs:189`,
  `streaming.rs:1519-1570`, `synthesis_common.rs:131`, and the filter in
  `inner.rs:58-105` — about ten files the row does not name.
- The member name already reaches the desktop per CHUNK: the pipeline stamps
  `metadata["peer"]` on every mesh hit (`retrieval_pipeline.rs:2342-2346`), and
  the desktop holds those as `retrievedChunks`; `EpistemicFooter.svelte:224-236`
  already joins a holding to its retrieved chunk by `(corpus_id, chunk_id)`.

## (c) Decide

1. Which surface is "the citation": `EpistemicFooter`'s released citations only,
   or also `SourceAttribution`'s prose `Sources:` lines?
2. Where the name joins the citation:
   - (A) Rust: `ReleasedCitation` gains `member: Option<String>`, filled via a new
     `chunk_members` parallel vec through `EvidenceContext` (~10 files listed above,
     sovereign-core test asserts it). Structural, larger.
   - (B) Rust, smaller: `citations_of` takes the turn's `peer_attribution`
     (corpus → member, `retrieval_pipeline.rs:2335-2340`) and stamps by corpus.
     Per-corpus, not per-chunk: wrong only if two members serve the same corpus in
     one turn (`or_insert_with` keeps the first).
   - (C) Desktop only: `EpistemicFooter` joins `citation.target` to
     `retrievedChunks` by `(corpus_id, chunk_id)` — the join it already does at
     :224-236 — and renders `metadata.peer` as "<corpus> on <member>". No Rust
     change; the row's "citation struct" clause is dropped.
3. The row's test clause asks `knowledge_fanout.rs` (sovereign-api) to assert "the
   citation's member name"; that test only sees the wire, so a citation assert has
   to live in sovereign-core (A/B) or a node/Svelte test (C). Name which, and the
   PLANT for it.

## (d) Then

Edit or mark the row in `ralph/next/ring-room/STATE.md` (e.g. mark it `[x]` with
the wire commit and mint a follow-up row for the chosen citation option), then
`rm ralph/STOP ralph/NEEDS_HUMAN.md`.

</details>

## 2026-09-18 — seat #2 (charter: fixing a row whose premise the tree contradicts) — rr-1-citation-member-on-released: option A at its real size, the member rides the gate beside custody

Fork: the worker's package (18:24Z, inline below): `peer_attribution` is destructured away in
`prepare_knowledge_context` (`runtime/retrieval/mod.rs:178-261`) and `gate_answer_inner` sees
only `EvidenceContext`; threading the map is ~7 files, the same as a parallel vec. Choice:
option A — `chunk_members: Vec<Option<String>>` parallel to `chunks`, filled in
`gate_evidence_with_sources` from `metadata["peer"]` (one writer, `retrieval_pipeline.rs:2344`)
beside `custody_of`; `ReleasedCitation.member` stamped in `citations_of`. Why A over B at equal
cost: per-chunk and exact, and `Custody::Peer` already names 'arrived from another node' — the
member is that stamp's companion, not a second concept (ARCH 12). Seam named in the row: a
typed home on `ScoredChunk.provenance` beside `stamped_custody()` is rr-2, not a free-form
key forever. Falsified if a released citation's chunk index and the members vec can drift —
the keep-filter test pins alignment. Row rewritten in place; instrument dependency unchanged.

<details><summary>the worker's package</summary>

# NEEDS_HUMAN — rr-1-citation-member-on-released (premise false: the map never reaches the gate)

## (a) Unit

`rr-1-citation-member-on-released` in `ralph/next/ring-room/STATE.md` (left `[~]`, no code edited).
The row: `citations_of` "take the turn's `peer_attribution` (corpus → member name,
`retrieval_pipeline.rs:2335-2346` — the ONE map, not a second one) … EDIT its one caller
`grounding/inner.rs:498` to pass it (thread the existing map, add no parallel vec)."

## (b) What I found

The premise under option B is that the caller at `inner.rs:498` can pass the map. It cannot:
nothing in the gate holds it.

- The map is born in `PipelineState.peer_attribution`
  (`sovereign-core/src/runtime/retrieval_pipeline.rs:403`, filled at :2334-2340 — the row's
  path `sovereign-core/src/retrieval_pipeline.rs` is `src/runtime/retrieval_pipeline.rs`).
- It dies in `prepare_knowledge_context` (`runtime/retrieval/mod.rs:178-186` destructures it,
  :191 counts mesh hits, :259-261 folds it into `SourceSummary.from_peer` via
  `build_provenance_components`). `KnowledgeContext` (`runtime/types.rs:27-...`) has no field
  for it; only `sources: Vec<SourceSummary>` carries the projection.
- `gate_answer_inner` (`grounding/inner.rs:10-18`) sees only `&EvidenceContext`, whose fields
  (`grounding/mod.rs:174-269`) are chunks/labels/locators/targets/grains/custodies/urls/
  admission — no corpus→member map, and `Custody` does not name the peer.
- `EvidenceContext` has no `Default`; it is built literally at 5 production sites in 4 files
  the row does not name — `handlers/knowledge_query.rs:1738`, `streaming.rs:1562`,
  `streaming.rs:3182`, `handlers/synthesis_common.rs:122`, `handlers/simple.rs:181` — plus 7
  literals in `grounding/tests.rs`. Those sites build from `ScoredChunk`s via
  `gate_evidence_with_sources` (`grounding/mod.rs:345`), and each mesh chunk already carries
  `metadata["peer"]` (`retrieval_pipeline.rs:2344`).

Commands: `grep -rn peer_attribution sovereign/crates --include=*.rs` (hits only in
retrieval_pipeline.rs, retrieval/mod.rs, formatters.rs, one desktop test);
`grep -rln "EvidenceContext {" sovereign/crates/sovereign-core/src` (6 files, 14 literals).

So "thread the existing map" is the same shape of cost the seat rejected for option A — about
7 files, not 1 — just with a map field instead of a parallel vec.

## (c) Decide

1. Accept B at its real size: add `peer_attribution: HashMap<String,String>` to
   `KnowledgeContext` (types.rs, set at retrieval/mod.rs:598) and to `EvidenceContext`
   (grounding/mod.rs:174), fill it at the 5 sites above (empty map at simple.rs and
   synthesis_common.rs where no fan-out ran, if that holds), 7 test literals, then
   `citations_of` at inner.rs:498. ~7 files, one map, no parallel vec. Re-mint the row with
   those files in its read list.
2. Or derive per chunk inside the gate from what already reaches it: `gate_evidence_with_sources`
   also returns the chunk's `metadata["peer"]`, i.e. option A (a parallel vec through the same
   filter in inner.rs:34-105) — per-passage and exact, same file count.
3. Or fall back to option C (desktop join by `(corpus_id, chunk_id)` against `retrievedChunks`,
   `EpistemicFooter.svelte:224-236`) and accept that the headless instrument reads the name
   from `retrieved_chunks[].metadata.peer` instead of the citation.

(d) Edit or mark the row in ralph/next/ring-room/STATE.md, then
`rm ralph/STOP ralph/NEEDS_HUMAN.md`.

</details>

## 2026-09-18 — seat #3 (charter: fixing a row whose premise the tree contradicts) — rr-1-media-offer-verb: the admit list goes on the wire as its own row; `commonwealth-rails` is not the ring rail

Fork: the worker's package (18:36Z, inline below). The `offered to` line needs a gossiped
field; the only literal inside `commonwealth-rail*` is `commonwealth-rails/src/gossip.rs:78`
(`minimal_capabilities`). `commonwealth-rails` is "the minimal rails daemon — the process that
IS your address on the mesh, with media registered on it" (its Cargo.toml); the predicate
(ring-doc's words: "the rail learning NOTHING to carry the CRDT") protects the ring rail,
`commonwealth-rail` + `commonwealth-rail-core`. The glob was imprecise, not the intent.
Choice: the worker's option 1 as the split it proposed — `rr-1-media-allow-on-the-wire`
(NodeCapabilities.media_allow, serde default/skip-empty; IrohDialInfo; stamp at gossip.rs:553;
MediaOffer.offered_to; every full literal fixed in one mechanical commit, no Default impl)
ahead of the verb, which now depends on it. Clause reworded in ring-room.toml, PROMPT §7,
order Seams, campaign.md. Falsified if the operator meant the rails daemon too — then revert
the wording and the wire row, and `offered to` waits for a design that never gossips it.

<details><summary>the worker's package</summary>

# NEEDS_HUMAN — rr-1-media-offer-verb (stopped at the premise check, no code edited)

## (a) The unit

`ralph/next/ring-room/STATE.md` row `rr-1-media-offer-verb` (held `[~]`). The half that
stops it: "EDIT `list_offers` (:184): print `offered to: everyone here` or `offered to:
<names>` per offer, from the advertised offer (if the admit list is not on the wire today,
put it beside the origin kind in `MeshMember.origins`' advertisement —
`daemon_wire/mesh.rs:93-95,345-347` — as data the holder publishes, never a second list)."

## (b) What I found

The admit list is NOT on the wire today. `MediaOffer` (commonwealth-media/src/reach.rs:153)
carries peer/node_id/status/path; `MediaCandidate` (:110) is derived from
`MemberRecord.capabilities.origins`. `MeshMember.origins` (daemon_wire/mesh.rs:96, :349) is a
read-side mirror (state.rs:50, mesh_http.rs:496) of the gossiped
`NodeCapabilities.origins` (commonwealth-core/src/capabilities.rs:73), stamped each round at
sovereign-mesh/src/gossip.rs:553 from `IrohDialInfo.origins` (commonwealth-core/src/mesh/mod.rs:350).
So "data the holder publishes" means a new field on a GOSSIPED struct — `NodeCapabilities`
or `MemberRecord`.

Both are constructed as full struct literals (no `..Default`, `NodeCapabilities` has no
`Default` impl) inside the forbidden tree:

```
$ git grep -n "MemberRecord {\|NodeCapabilities {" -- 'commonwealth/crates/commonwealth-rail*'
commonwealth/crates/commonwealth-rails/src/gossip.rs:54   NodeCapabilities {   (minimal_capabilities, production)
commonwealth/crates/commonwealth-rails/src/acceptor.rs:164 MemberRecord {       (test)
commonwealth/crates/commonwealth-rails/src/gossip.rs:429  MemberRecord {       (test)
commonwealth/crates/commonwealth-rails/tests/two_daemons.rs:73 MemberRecord {  (test)
```

A new `NodeCapabilities` field therefore needs a one-line diff at
commonwealth-rails/src/gossip.rs:78 (`media_allow: Vec::new(),`), which PROMPT §7 forbids
("zero diffs there is the campaign predicate - a row that seems to need one is §6"). It also
breaks ~22 other literal sites in ~18 files outside the rails (sovereign-api/tests/main ×6,
sovereign-mesh/tests/main ×6, sovereign-mesh-test-harness ×2, commonwealth-discovery
membership.rs ×2, commonwealth-core ×2, sovereign-mesh capabilities.rs/persist.rs/ring_roster,
sovereign-serving knowledge_assignment.rs) — past the row's ~10-file atom.

Everything else in the row checks out and is buildable without the rails: `[iroh] media_origin`
and `media_allow` both exist (setup_config_iroh.rs:107, :125 — `--admit` writes the EXISTING
`media_allow`, no new noun); the live reload route explicitly marks `iroh.media_origin`
restart-required (sovereign-mesh/src/admin_http.rs:350-351), so the verb restarts the daemon
itself; `publish_cmd.rs` already has the comment-preserving `load_doc`/`write_doc`/
`resolve_target` to reuse; cw-media-demo.sh:131-142 is the config-edit block to replace.

## (c) What the operator must decide

1. Permit one more rails hunk — `media_allow: Vec::new(),` in
   `commonwealth-rails/src/gossip.rs:78` `minimal_capabilities` — and accept the ~22 literal
   sites as one mechanical commit (probably its own row ahead of this one, since it is ~18
   files). The field would be `NodeCapabilities.media_allow` (serde default, skip-if-empty),
   stamped at gossip.rs:553 beside `origins` from a new `IrohDialInfo.media_allow`, carried to
   `MediaCandidate`/`MediaOffer.offered_to`.
2. OR split the row: ship the verb now (`offer <origin> [--admit ...]`, writes
   media_origin + media_allow, restarts the daemon itself, cw-media-demo.sh calls it, the
   narrowing test in commonwealth-media), and move the `offered to:` line to a new row that
   depends on decision 1. Note rr-1-library-rail needs `offered to` too.
3. OR give `NodeCapabilities` a `Default` impl + a rails diff anyway — same predicate problem
   as 1, fewer future breakages; still a rails hunk.

## (d) Then

Edit or mark the row in ralph/next/ring-room/STATE.md, then
`rm ralph/STOP ralph/NEEDS_HUMAN.md`.

</details>

## 2026-09-18 — seat #4 (charter: which of the options an order names; the smaller reversible step) — REVIEW-build-rr-1-instrument: real Jellyfin as a netns sibling, real weights on a and b, a phase switch in D

Fork: the worker's package (19:04Z, inline below). (1) Jellyfin: 1a — sibling container in b's
network namespace from the host; cw-media-demo.sh split into holder-up / holder-setup; one
restart (the verb's). 1b would substitute a file server for 'it plays' and the bar's demo
sentence is the thing a person watches — not this week. (2) Weights: 2a — the 0.6B embedder on
b, embedder + smallest grounding chat GGUF on a; the bar reads a RELEASED citation, so a
synthesis is the measurement, and 2b would measure the wire the previous rows already test.
Census: install (bring-up, before the walk) is printed and classified; member
addresses/ports/hosts/credentials there still count, model paths do not; the bar's count is
the walk. (3) RING_DOC_PHASES env switch in D — the second and last edit to D. Falsified if
the smallest GGUF that grounds does not fit beside the embedder in a node's RSS on this host
(then leg 1 runs a alone with weights and b as terminal + embedder only, and the run names it).

<details><summary>the worker's package</summary>

# NEEDS_HUMAN — REVIEW-build-rr-1-instrument (ring-room)

## (a) The unit

`ralph/next/ring-room/STATE.md:56`, `- [~] REVIEW-build-rr-1-instrument — depends [rr-1-daemon-rings-registered,
rr-1-citation-member-on-released, rr-1-library-rail] — CREATE scripts/ring-room-demo.sh REUSING D's node door
and podman backend …` All three dependencies are `[x]`. Nothing was built; the only change to the tree is
the `[~]` on that row (uncommitted).

Two of the row's premises are false on the nodes D's `up` actually brings up. Leg (1) and leg (3) cannot
run as written, so the row's DEMO check ("four PASSED or FAILED with their values") cannot be met: those two
bars could only ever read COULD-NOT-JUDGE, which the check calls §6. Building the script first would
pre-register two legs whose shape depends on your answer below.

## (b) What I ran, and what came back

Bring-up, D's own door, podman backend, throwaway data dir (35 s):

    RING_DOC_BACKEND=podman RING_DOC_DIR=$PWD/target/ralph/rr-probe scripts/ring-doc-demo.sh up
    up (podman): a(alex) b(bo) c(cy) — proxies on 19849 19859 19869

Premise 1 — "b runs `scripts/cw-media-demo.sh` inside its node". The node image has no podman, and
cw-media-demo.sh's `holder_up` begins by requiring it (cw-media-demo.sh:82, whose own message already says
"the sovereign-vulkan toolbox: it is not"):

    podman exec ring-doc-b sh -c 'command -v podman || echo NO-PODMAN; command -v flatpak-spawn || echo NO-FLATPAK-SPAWN'
    NO-PODMAN
    NO-FLATPAK-SPAWN

(The jellyfin image IS on the host: `docker.io/jellyfin/jellyfin:latest 1.7 GB`.)

Premise 2 — "b ingests a small folder (`svrn corpus ingest`) … then a answers a 5-question bank". D's nodes
are terminal by design (ring-doc-demo.sh:196 "Terminal nodes: no weights"; `[node] entry` points at the
container's own loopback :9741, where nothing listens). Ingest refuses at the embed step:

    podman exec -e SOVEREIGN_DATA_DIR=$D/b ring-doc-b sovereign-cli corpus ingest $D/folder
    Daemon not reachable at http://127.0.0.1:19851 (the daemon advertises no models). A `model:`/`embed:`
    step needs it — start it with `sovereign daemon`.

And A's answer carries a *released* citation (933d5a14e: `ReleasedCitation.member`), which only a grounded
synthesis produces — so A needs a chat model too. `sovereign/models/` is inside the bind-mounted repo
(Qwen3-Embedding-0.6B-Q8_0.gguf and several chat GGUFs are there), so weights are reachable; no node is
configured to load them.

Torn down afterwards (`… ring-doc-demo.sh down`; no `ring-doc-*` container left).

Drift, not a stop: the row says D's dispatcher `case "$1"` is at :911; it is at :917.

## (c) What the operator must decide

1. **The Jellyfin stand-in on b (leg 3).** Pick one:
   a. The instrument starts Jellyfin from the host side as a sibling container in b's network namespace
      (`podman run --network container:ring-doc-b … jellyfin`), so it listens on b's own loopback :8096,
      and runs INSIDE b only cw-media-demo.sh's non-podman half (wizard, `holder_key`'s
      `svrn mesh media declare`, the offer verb). That needs cw-media-demo.sh split so the half is callable
      — e.g. a `holder-setup` verb, `holder-up` = container + `holder-setup` — an edit to a file this row
      does not name (and its `-p 127.0.0.1:8096:8096` cannot combine with `--network container:`).
      Note `holder_key` (cw-media-demo.sh:74-77) runs `svrn daemon stop/start`; inside a node that restarts
      the throwaway daemon outside D's pidfile — harmless on podman (the container is removed), but the
      offer verb already restarts the daemon itself, so it would be a second restart.
   b. Leg 3 runs the offer and the viewer half against an origin with no real Jellyfin (a python
      http.server on b's loopback serving one file) — measures the tunnel and the rail, not Jellyfin; the
      bar's demo sentence says "it plays", so this is a substitution the verdict would have to name.
   c. Something else (podman in the node image is not ours to do).

2. **Weights on the nodes (leg 1).** Pick one:
   a. The instrument gives b an embedder and a a chat model from `sovereign/models/` (b: the 0.6B
      embedder; a: embedder + the smallest chat GGUF that grounds). Costs RSS and a few minutes of load per
      run, and writes model lines into two node configs — which the nothing-typed census must then
      classify: I would count config written by bring-up (before the walk) as install, printed but not
      counted, and count only what the driver types during the walk. Say if that line is wrong.
   b. Measure the citation one layer down: A's knowledge search fan-out returns passages whose `peer_name`
      field (1426c47f8) names B, with no synthesis — still needs the embedder on b (and on a, if a embeds
      the query). The bar's `one_line` says "A answers each with a citation", so this also substitutes and
      must be named in the row.
   c. A reaches an inference provider outside the node — the deployed daemon is off-limits and the node's
      netns cannot reach the host loopback, so this is only open if you name one.

3. **Run length (not a blocker, a heads-up).** Leg 2 reuses D's driver, which is one heredoc running all
   four ring-doc phases (the 60 s partition and a daemon restart included); ring-room needs phases 1 and 3.
   Reusing it whole keeps the "never copy" rule and adds roughly two minutes per run; skipping phases 2 and
   4 needs an env switch in D's driver — a second edit to D beyond the dispatcher guard. Say which.

The row's own judgment (guard D's dispatcher with `[[ "${BASH_SOURCE[0]}" == "$0" ]]` vs lifting the door
into `scripts/ring-node-door.sh`) I will take as the 2-line guard once the above is settled: it is the
smaller and reversible step, and D's `SCRIPT` re-entry (`_cut`/`_heal`/`_stop`/`_start`) keeps working.

## (d) Then

Edit or mark the row in ralph/next/ring-room/STATE.md (it is `[~]`, uncommitted), then
`rm ralph/STOP ralph/NEEDS_HUMAN.md`.

</details>

## 2026-09-18 — seat #5 — the pre-registered first run's three failures become rows ahead of rr-1-tune

Evidence: 99ca7e4cb (five verdict rows + census) and bc679026d (join leg). Rows minted:
REVIEW-build-rr-1-answer-fans-out (instrument the ask on a first — routing short-circuit on an
empty local set, `mesh_knowledge` None, or a hosted set missing b — then fix at the one
'which corpora exist' decider, reusing hosted_corpora; e2e test watched failing first);
rr-1-media-origin-live (media_origin/media_allow read per dial behind the reload handle; the
two restart_required pushes deleted; the >120 s stale dial after a restart NOTED as a mesh
bug, not fixed); rr-1-nothing-typed-to-zero (`corpus share` verb; offer verb probes 8096;
`admit` without origin; census rule: verbatim tool-printed URLs opened = `opened`, not
typed — provenance line required; assembled strings count). rr-1-tune now depends on the
last. Falsified if the instrumented ask shows the mesh step DID run and returned b's passage
— then the loss is between retrieval and the gate's release, and the row's fix site moves.
Why the seat minted instead of letting REVIEW-DEMO fail: three known reds would cost a
REVIEW-DEMO session and a resolution session to arrive at the same rows.

## 2026-09-18 — seat #6 — REVIEW-build-rr-1-answer-fans-out lands (b0b491740 instrument, 768895508 fix); the per-claim release carries the member as a row; ask timeout 600 s

Fork: the worker's package (inline below). The fan-out now reaches the peer on every turn
(mesh_hits=5, provenance from_peer Bo); the bar reads 0.4 because two of five answers were
released through the per-claim path, which emits no ReleasedCitation, and one overran the
300 s ask timeout on the CPU 2B. Choice on (1): the worker's (i), shaped honestly — a quote
citation cannot be minted for a claim verdict (epistemic.rs:100-118 binds a quote to ONE
chunk), so the claim's support pointer (`Provenance` corpus variant) gains `member`, stamped
from the aligned chunk_members vec; the instrument counts either evidence form and a release
with claims_checked 0 stays 0. NOT (ii): a bar change is the operator's and the goodhart
already forbids counting the summary-level name. NOT (iii): swapping the model to move the
number is the whack the campaign forbids; the room's machine is not a CPU node and the row's
comment says so. (2): ASK_TIMEOUT_S 600, a measurement budget, not a product change. (3):
row marked [x]. Flag to the operator: "citation" in the bar now means the released evidence
pointer in either gate mode — revert if you read it as the quote citation only. Falsified if
the per-claim path's supporting chunk index is not the gate's leaf index (then the member
stamp mis-aligns and the alignment test in 933d5a14e's shape is owed here too).

<details><summary>the worker's package</summary>

# NEEDS_HUMAN — REVIEW-build-rr-1-answer-fans-out

## (a) The unit

`REVIEW-build-rr-1-answer-fans-out` (row 58 of `ralph/next/ring-room/STATE.md`, left `[~]`).
The row's DEMO check says: "paste the five rows; `ra-room-answer-names-the-machine`
must read 1.0". It reads **0.4**, up from 0.0.

The row's build is done and committed:

- `b0b491740`: the instrument. It named the branch that lost the answer: the chat
  turn reached `step_main_retrieval_mesh`, and the daemon's `Runtime` had
  `mesh_knowledge = None`. Decisive lines are in that commit's body.
- `768895508`: the fix. `daemon_knowledge_source` (sovereign-mesh/src/knowledge_client.rs)
  gives the daemon a loopback seam to its own `/v1/knowledge/search`, which is the one
  decider for local ∪ `hosted_corpora`. `KnowledgeQueryPlan.peer_attribution` now
  carries the pipeline's map, so KnowledgeQuery provenance names the peer (both
  builders used to pass an empty map). The e2e test was watched red, and two PLANTs
  went red. CLEAN, LINT, TEST(sovereign-core/-mesh/-api) all exit=0.

## (b) What was run, and what it printed

`RALPH_DEMO_SCRIPT=scripts/ring-room-demo.sh scripts/ralph-check.sh demo` took about
22 minutes. The answer leg alone ran 14:33 to 14:51, because every answer now does a
real grounded synthesis on the 2B model. exit=1.

```
ra-room-answer-names-the-machine   0.4   FAILED
ra-room-doc-name-from-membership   1.0   PASSED
ra-room-film-from-the-library-rail 0.0   FAILED  (c_first_byte false; pick_to_first_byte_s 58.95, http 206)
ra-room-plug-in-live               0.0   FAILED  (c_answer_names false; answered_s null in the 60 s window)
ra-room-nothing-typed              10    FAILED  (walk count; rr-1-nothing-typed-to-zero's row)
```

The answer bar, question by question:

```
q0  error: empty JSON: `chat ask` killed by ASK_TIMEOUT_S=300 (turn 21:34:22 → gate 21:39:18)
q1  released 3, members [Bo, Bo, Bo]   grounded     (gate_action=citation_grounded)
q2  released 0                         grounded     (gate_action=released, mode per_claim)
q3  released 0                         unverified   (gate_action=released, mode per_claim, claims_checked 0)
q4  released 1, members [Bo]           grounded     (gate_action=citation_grounded)
```

In a's daemon.err, all five turns read
`knowledge fan-out summary local_hits=0 mesh_hits=5 mesh_peer_tagged=5 mesh_corpora={"room-yOwnPh"}`,
and every answer's provenance reads `sources: [{origin: room-yOwnPh, count: 5, from_peer: Bo}]`.
The answer reaches the peer and names it. What still fails the bar is the gate's
release path on this model:

- 2 of 5 go through the per-claim release, which carries no quote citations.
- 1 of 5 overran the 300 s ask timeout.

Neither is on the fan-out path this row owns.

## (c) What the operator must decide

1. **Is the per-claim release a citation?** q2 and q3 were released grounded on Bo's
   passages, but through `mode: per_claim` (grounding gate, sovereign-core
   `runtime/grounding/`). That path emits no `ReleasedCitation` rows, so the bar,
   which reads `epistemic_state.citations[].member`
   (scripts/ring-room-demo.sh report), scores them 0. Choose one:
   - (i) a new row makes per-claim releases project `ReleasedCitation` rows, with
     `member` from the supporting chunk. That is gate work, outside this campaign's
     "strictly necessary" so far.
   - (ii) the bar also counts the provenance `from_peer` on a grounded answer. That
     is a bar change in quality/campaigns/ring-room.toml, and it is the operator's
     to make.
   - (iii) a larger chat model on a (RING_ROOM_CHAT, scripts/ring-room-demo.sh:51)
     that writes quotable answers.
2. **The ask timeout.** `ASK_TIMEOUT_S=300` (scripts/ring-room-demo.sh:61) was sized
   when every answer was a fast general-knowledge refusal. A grounded answer on the
   2B CPU node took 296 s for q0, and 92–229 s for the others (turn routed → gate lifecycle, a's daemon.err). Raise it, or accept
   q0 as the model's cost.
3. **Mark this row.** The row's own build (instrument, seam, attribution, e2e test)
   is committed and green on every check except the 1.0 floor, which depends on 1
   and 2 above. You can mark it `[x] 768895508` and mint a row for whichever of 1(i),
   1(ii) or 1(iii) you choose, or keep it `[~]`.

Two other reds belong to rows that already exist: the film leg's first byte took
58.95 s (rr-1-media-origin-live), and walk count = 10 (rr-1-nothing-typed-to-zero).
plug-in-live's `c_answer_names` is leg 1's failure re-run on the fourth node.

## (d) To resume

Edit or mark the row in ralph/next/ring-room/STATE.md, then
`rm ralph/STOP ralph/NEEDS_HUMAN.md`.

</details>
