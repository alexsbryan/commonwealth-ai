# ralph director decisions — domains

Each entry: date · unit · the fork · the choice · evidence · what would falsify
it · the commit it landed in. A REVIEW-AFTER tag marks a call the charter did
not clearly cover.

## 2026-09-16 · dm-appstate-answering · the mint's "no consumer changes" premise is false

**Fork.** `dm-appstate-answering` (STATE.md:148) claimed "no consumer changes",
citing an empty grep as evidence. The package says the grep is defective and
three consumer sites exist. Decide the consumer-repoint shape, and whether to
correct the five downstream extraction rows before they start.

**Choice.**

1. The premise is false; the row is corrected. `\b` is the defect: this host's
   git grep (Apple Git 2.50.1) returns empty for any pattern using it —
   `git grep -cE 'session\b' -- …/routes_inference.rs` is empty while `'session'`
   returns 41. Without `\b`, `routes_inference.rs:1513` appears
   (`state.inner.session_store`), and the other two are read across line breaks
   at `:1536-1539` (`middleware_registry`) and `:1557-1561` (`repo_root`), all
   inside `run_atos_pipeline` (`#[cfg(feature = "atos")]`, `:1506`).
2. The repoint reads the part directly — `state.inner.answering.<field>`, with
   `AnsweringPart`'s fields `pub` — not new delegating accessors. The charter
   says the existing surface over a new one and the smaller reversible step; the
   existing surface for these fields is a `pub` field read, and the repo reserves
   accessors for fields needing a load (`self_node_pubkey`, `ring_rail`,
   `ring_write_nudge` all read through `RwLock`/`ArcSwap`). DC §4.2 already
   expects consumers to read parts: "Handlers take a part, never the node", and
   "62 are route shells reading the node's parts, which is the design"
   (DAEMON_CORE.md:354,408).
3. The five downstream extraction rows are re-scoped in this commit. They carry
   the same false premise indirectly — "delegate the accessors" presumes
   accessors that do not exist — and their fields have direct external reads
   (workbench 1, ingest 29, node 45, serving 62, fabric 187; lower bounds by the
   boundary-requiring pattern, a few doc-comment mentions included). Letting each
   worker rediscover this would cost five worker+supervisor cycles for one defect
   (principle 2). Each now names the measurement command and warns `\b` is broken.

**Evidence.** ralph/NEEDS_HUMAN.md; the commands above (reproduced); the same
`\b` grep is premise 2 of `4412049ca`; a lower bound of direct `.inner.<field>`
reads in sovereign-api is 183; no method in `impl AppStateInner`/`impl AppState`
reads the three (`git grep -nE '\.(middleware_registry|session_store|repo_root)'`
in state.rs is empty). DAEMON_CORE.md:332-418 (§4.2); ARCH_LAYERS.toml:702-704;
state.rs:531,537,543,1507-1510.

**Falsified by.** A grep on a host where `\b` works showing the three fields are
read only through accessors; DC §4.2 naming accessor methods as the part surface;
or the operator's design review requiring accessors (HUMAN-design-review approved
DC §4, which says handlers take parts).

**REVIEW-AFTER:** the systemic re-scope changes five not-yet-started rows in one
commit; the operator may prefer per-row re-scoping.

**Landed in.** this commit — the supervisor records its range in
`ralph/.director-commits` (`git revert <sha>` reverts the single commit).

## 2026-09-16 · REVIEW-build-appstate-identity · the identity reader's edge, and the re-export that avoids a baseline raise

**Fork.** The worker's `IdentityReader` (sovereign-contracts) made `sovereign-api`
depend on the leaf directly, and `layer-gate` refused the fan-in growth 30 → 31.
Accept the growth (a hand-edited `fan_in.tsv` line + a §10.1 ledger entry, the
`66b6578d4` / `b3edcc335` method) or reach the reader through an existing
dependency's re-export?

**Choice.** The re-export. `sovereign-core` already carries the precedent — its
`daemon_wire` block (`sovereign-core/src/lib.rs:74-81`) exists for exactly this
refusal ("so that a contracts module is reachable at its `sovereign_core::`
path"), and `sovereign-api` already depends on `sovereign-core`, so the reader
costs no new edge. `sovereign-api/Cargo.toml` drops the direct
`sovereign-contracts` dep; `sovereign_core::identity` re-exports
`sovereign_contracts::identity`; the three import sites follow, and the rustdoc
link in `state/serving.rs:130` repoints to the Principal precedent it names.
Principle 11: the inventory (the existing re-export pattern) outranks the plan
(a new edge), and the ratchet keeps its meaning.

**Evidence.** ralph/NEEDS_HUMAN.md; `quality/baselines/fan_in.tsv:11` stays `30`;
`cargo xtask layer-gate` exit=0 (fan-in within caps); LINT exit=0;
TEST(sovereign-api) 564 pass.

**Falsified by.** A sovereign-api use of `sovereign_contracts::` outside the
identity module that the dep removal breaks (LINT would fail), or a layer-gate
violation from the `sovereign-core` path.

**Landed in.** this commit. The director session died mid-edit (an opencode
server error, `err_3a891645`); the operator finished and verified its delta, so
the decision is the director's and the verification is the operator's.

## 2026-09-16 · REVIEW-build-appstate-self-claims · the SelfClaims port has no home that carries all five inputs; redraw hosted corpora out

**Fork.** `REVIEW-build-appstate-self-claims` (STATE.md:155) stopped because the
`SelfClaims` port DC §4.2 decides has no crate that is simultaneously nameable by
both `sovereign-mesh` and `sovereign-api`, able to carry `CorpusShardInfo`, able
to declare an `async fn`, and ratchet-neutral. The worker named four placements
and recommended option 1 (contracts port, hosted corpora redrawn out); it did not
decide, because clearing the row is an operator act.

**Choice.** Option 1, and the row is re-scoped to it. Declare `SelfClaims` +
`LocalClaims` in `sovereign-contracts`, re-exported at `sovereign_core::self_claims`
on the `identity` precedent (`sovereign-core/src/lib.rs:89`; DECISIONS.md
2026-09-16 identity entry). It answers availability, in-flight, storage remaining
and embed model, plus the storage-used write-back. **Hosted corpora is redrawn
out**: it is read from the `engine` parameter, not `AppState`, and `Fabric`
already legitimately names `corpus-engine`, so `build_hosted_corpora` stays in
`sovereign-mesh` and the port does not carry `CorpusShardInfo`.

This is the design's own terms, not a deviation from it. DC §6's kill bar says
"If `SelfClaims` needs more than about five inputs from other contexts'
internals, Fabric is computing Serving's claims for it: redraw the port"
(`quality/DAEMON_CORE.md:570-572`), and DC §7 names the five inputs unverified
("§4.2's five come from reading gossip and the capabilities builder, not from a
port drafted against them", `:591-592`). Options 2–4 each buy the literal row at
a named cost the charter puts off-limits or principle 11 avoids: option 2 weakens
`commonwealth-core`'s stated liftability contract ("declares no `async fn`",
`commonwealth-core/src/lib.rs:21`); option 3 puts a Fabric/node port in the
Serving package (owner mismatch, DC §4.2's owner table); option 4 raises
`commonwealth-core`'s fan-in 16 → 17 (`quality/baselines/fan_in.tsv:6`) against a
baseline whose header says "never adds, never raises". The inventory (a contracts
port already exists for `MemberReach` and `IdentityReader`) outranks a new edge.

**Evidence (all reproduced this session).**
- `sovereign code converge noun SelfClaims --corpus-id commonwealth-ai` → 0
  definitions; `LocalClaims` → 0 definitions.
- `build_local_capabilities` (`sovereign-mesh/src/capabilities.rs:64-70`) reads
  only storage (`:132` `set_storage_used_bytes`, `:133` `storage_remaining_bytes`)
  and in-flight (`:238` `current_local_in_flight`) off `AppState`; the caller
  `gossip::run_one_round` reads the inference store and recomputes availability
  (`gossip.rs:468`, `:474-478`); hosted corpora comes from the `engine` parameter
  (`:102-124`, `build_hosted_corpora` `:263`). `loaded_models` is a non-read
  (`:189`).
- `sovereign-contracts` is a layer-0 `[[package_leaf]]` with allow-list
  `["oicp-types", "kernel-types", "sovereign-time"]`
  (`quality/ARCH_LAYERS.toml:860-873`); `commonwealth-core` is in `mesh-foundation`
  (`:161`); `CorpusShardInfo` is `commonwealth-core/src/knowledge.rs:64`. A
  contracts trait cannot name it.
- `sovereign-contracts` already declares `#[async_trait]` traits
  (`traits.rs:32`, `local_inference.rs:33`), so an async port is native there.
- `EmbedModelInfo` is `oicp_types::manifest::EmbedModelInfo`
  (`oicp-types/src/manifest.rs:263`), re-exported as
  `commonwealth_core::oicp::EmbedModelInfo` — so the contracts port CAN name it.
- `sovereign-mesh` already depends on `sovereign-contracts`
  (`sovereign-mesh/Cargo.toml:13`); `sovereign-api` does not, and reaches it via
  `sovereign_core` (`sovereign-core/src/lib.rs:89`), so no fan-in moves.

**Falsified by.** A later measurement showing `CorpusShardInfo` has no
`commonwealth-core`-only field (it is a `Serialize`/`Deserialize` wire type whose
fields are `String`/`Option`/`Vec`/`u64` — `knowledge.rs:64-95`), in which case
the follow-up move lands and hosted corpora folds into the port without the
redraw; or DC §6/§7 being revised to require all five inputs in the port, which
would reopen the placement fork.

**REVIEW-AFTER:** the redraw drops one of the five answers DC §4.2's narrative
lists (hosted corpora), so the morning review should decide whether DC §4.2's
text is amended to record the redraw or a follow-up row is minted to move
`CorpusShardInfo` to `oicp-types`. The row text carries the redraw; the DC was
left unedited because `HUMAN-design-review` approved it.

**Landed in.** this commit — the re-scoped `REVIEW-build-appstate-self-claims`
row in `ralph/STATE.md` and this entry. `git revert <sha>` reverts it alone.

## 2026-09-16 · REVIEW-build-mesh-host-decouple · four mesh files cannot be decoupled; they are the daemon's, and move with the type

**Fork.** `REVIEW-build-mesh-host-decouple` (STATE.md:173) is the `[~]` row the
pool resumed; it stopped after committing the 9 mechanical sites (`93f0c04a3`)
with 8 left. Four of the eight sit in `venue_host.rs`, `roster_repair.rs`,
`media_reach.rs` and `origin_fanout.rs`, each of which carries an inherent
`impl EmbeddedDaemon` (or `impl VenueSource`/`VenueHost for EmbeddedDaemon`)
plus a route shell. The row's resolution method — "each item moves to a leaf
both crates can name, or the caller stops needing it" — has no instance here:
`EmbeddedDaemon` is the daemon's composition root and no leaf can host it.
Decide whether those four files are `fabric` (and their daemon glue is
extracted) or `host` (and the files move with the type), and where the eighth
site's helper belongs.

**Choice.**

1. **The four files are not decoupled; their disposition is
   `REVIEW-build-daemon-embedded-split`.** Rust pins them to the crate that
   defines `EmbeddedDaemon`: E0116 forbids an inherent impl leaving its type's
   crate, and the orphan rule does the same for `impl VenueSource for
   EmbeddedDaemon`. `EmbeddedDaemon` lives in the host-tagged `daemon.rs`, and
   DC §4.1 says it "splits by owner, not size" — the route shells and the
   `VenueSource`/`VenueHost` impls to sovereign-daemon, while the membership
   methods DC §4.1 itself names (`forget_member`; `origin_offers`/`origin_reach`
   = "report reach") re-home to Fabric. So the four files are *mixed* today and
   are split — not decoupled — by row 175. Tagging them `host` would have
   mis-tagged the membership methods; tagging them `fabric` and extracting the
   impls here would have duplicated row 175. Row 173's check is therefore
   narrowed to name the four deferred files explicitly (principle 6: the
   absence is reported, not defaulted), and row 175 gains them.
2. **The eighth site is resolvable and is resolved.** `reindexer.rs:972` named
   `crate::auto_resume::env_truthy`, a pure truthiness helper. It moves to
   `sovereign-contracts::env::truthy` — the shared leaf the row's own method
   cites (`sovereign-contracts::worker_pod` precedent) — and both callers
   (`auto_resume.rs`, `reindexer.rs`) repoint. One spelling survives, so ARCH 8
   holds when `auto_resume` moves to `sovereign-daemon` and `reindexer` to a
   `corpus-engine` crate and they may no longer name each other.

**Evidence (reproduced this session).**
- `git grep -nE 'crate::(local_only|loopback_guard|http_response|types|daemon|
  supervised_task|work_donor|auto_resume)' -- sovereign/crates/sovereign-mesh/src`
  excluding host-tagged modules: exactly 8 sites before, 7 after (the four
  files, lines listed in `93f0c04a3`'s body); the reindexer site is gone.
- `git grep -n 'impl EmbeddedDaemon\|impl .* for EmbeddedDaemon'` →
  `daemon.rs:512`, `media_reach.rs:56`, `origin_fanout.rs:55`,
  `roster_repair.rs:50`, `venue_host.rs:51,58`; `pub struct EmbeddedDaemon`
  is `daemon.rs:211` (DT context `host`).
- DC §4.1 (DAEMON_CORE.md:287-334): the host is "assembly / surface / edge /
  adapter", `EmbeddedDaemon` "splits by owner, not size", membership operations
  are Fabric's methods; `quality/ARCH_LAYERS.toml:739-742` is the forbid.
- `sovereign-mesh/Cargo.toml:13` already names `sovereign-contracts`, so the
  leaf move adds no edge.

**Falsified by.** A later measurement showing the four files' route shells and
impls are cleanly separable without touching `EmbeddedDaemon` (then 173 could
have resolved them directly); or row 175's split keeping the files in
`sovereign-mesh`, which would make `fabric` the right tag and this deferral a
detour.

**REVIEW-AFTER:** the charter covers re-scoping and deferring a row, but this
also narrows a row's *check* (from "empty" to "only the four named files"), and
mints the four files into row 175 — the morning review should confirm the
four-file boundary is the one DC §4.1 draws.

**Landed in.** this commit — `ralph/STATE.md` rows 173 (marked `[x]`, corrected
scope/check) and 175 (four files added), `sovereign-contracts/src/env.rs` +
`lib.rs`, `sovereign-mesh/src/auto_resume.rs`, `sovereign-mesh/src/reindexer.rs`.

## 2026-09-16 · REVIEW-build-appstate-self-claims · REVIEW-AFTER resolved — the DC records the redraw, no follow-up row

**Fork.** The redraw left DC §4.2 claiming five answers (availability, in-flight,
storage remaining, loaded models, hosted corpora) while the shipped port answers
four (`self_claims.rs:31-44`). Amend the DC to record the redraw, or mint a
follow-up row moving `CorpusShardInfo` to `oicp-types` so hosted corpora folds
into the port?

**Choice.** Amend the DC; no follow-up row. The port's job — kill the
`fabric -> host` backflow — is done by the four answers. Hosted corpora is
engine-sourced (DC §4.2's own table lists the engine as a shared Fabric
dependency, `:365`) and never touched `AppState`, so folding it back buys no
boundary and costs a 14-site move across four crates (commonwealth-core,
sovereign-mesh, sovereign-tools, sovereign-api tests). `loaded_models` is an
honest empty (`capabilities.rs:188`), so §4.2 needed a precision edit regardless
of where `CorpusShardInfo` lives — a move alone cannot make the text true. The
move's trigger stays in the redraw entry's falsifier: a second consumer through
the port, or a kernel rung that wants wire types in `oicp-types`.

**Evidence.** `self_claims.rs:31-44` (four fields, no corpora);
`capabilities.rs:184-194` (`hosted_corpora` from the engine, `loaded_models:
Vec::new()`); DC §4.2 table `:365`; the type is pure serde wire
(`knowledge.rs:20-25`, `:64-116`); 14 call sites (`callers`); `sovereign-mesh`
already names `corpus-engine` (`Cargo.toml:104`).

**Falsified by.** A second consumer that needs hosted corpora through the port;
or the engine handle ceasing to be a legitimate Fabric dependency; or DC §6/§7
being revised to require all five inputs (the redraw entry's own falsifier).

**Landed in.** this commit — DC §4.2 (`:383-385`) and §7 (`:591-592`), no code
change, the edit line-count-neutral so the `:570-572` / `:591-592` citations
hold. Resolved in the morning review the redraw entry asked for.

## 2026-09-16 · REVIEW-build-mesh-api-decouple · the three loops are Fabric's; the wire types get a leaf

**Fork.** The worker resolved 13 of 27 non-`host`→`host` sites (`468be9869`)
and stopped: the remaining 14 are in `gossip.rs`, `ring_sync.rs` and
`rail_kv_pump.rs`, which read `AppState` (17 things in `run_one_round`) and the
`routes_internal`/`server` wire types. Are the loops fabric (Fabric's state must
reach them) or host (the daemon's background tasks)? And where do the wire types
live?

**Choice.**

1. The loops are **fabric**. DC §4.2:347 assigns the roster, identity, clock,
   transport, dial info and the three liveness maps to Fabric, and DC §4.1's
   host "decides nothing a context owns" — a module that is none of
   assembly/surface/edge/adapter holds a decision, and `run_one_round` holds
   Fabric's. Their `AppState`/`FabricSeed` reads defer to
   `REVIEW-build-daemon-parts`, which already relocates `state/fabric.rs` (315)
   → sovereign-mesh and repoints its consumers (DC §4.2's six-owner table).
2. The wire types get a **new leaf**, `sovereign-peer-wire` (layer `mesh-api`):
   they carry `commonwealth_rail::{Digest, Op, SignedOp}` / `Mesh` /
   `MemberRecord`, so no existing leaf can host them, and the loops (fabric) may
   not name the daemon that `dm-daemon-api-http-b2` moves the routes to.
   `REVIEW-build-peer-wire` creates it and repoints both sides.
3. The ~1,480-line `ring_sync` test module moves to `tests/main/` (with
   `exchange` made `pub`) rather than a host shim in mesh's `lib.rs`; the same
   rows carry it.

**Evidence.** ralph/NEEDS_HUMAN.md (the worker's package);
`git grep -n 'sovereign_api::' sovereign/crates/sovereign-mesh/src` after
`468be9869`; DAEMON_CORE.md:262-290 (§4.1, "its background tasks and their
shutdown" / "decides nothing a context owns") and :347, :378-385 (§4.2);
DT module tags (gossip.rs 1,478 + ring_sync.rs 2,072 + rail_kv_pump.rs 1,011 =
fabric); ARCH_LAYERS.toml `mesh-api` (sovereign-api, sovereign-daemon,
sovereign-mesh — a leaf there is nameable by both).

**Falsified by.** A measurement showing the loops' liveness decisions are the
daemon's (DC §4.1 amended to put them in the host); or a later row moving
Fabric's state to the daemon instead of mesh.

**Landed in.** this commit — `ralph/STATE.md` (the row re-scoped and marked
`[x]`; `REVIEW-build-peer-wire` minted; `REVIEW-build-daemon-parts` re-scoped)
and this entry. `git revert <sha>` reverts it alone.

## 2026-09-16 · REVIEW-build-daemon-embedded-split · the shells leave first; the type moves when nothing in mesh names it

**Fork.** The row is an ordering defect, not a content defect: moving
`EmbeddedDaemon` out of `sovereign-mesh` needs every mesh module naming it to
move in the same commit — 25 route shells hold `Arc<EmbeddedDaemon>` — but
those shells are the payload of the rows that depend on this one, and
`[[forbid]] from = "sovereign-mesh" to = "sovereign-daemon"`
(`quality/ARCH_LAYERS.toml:739-742`) makes the intermediate state (type in the
daemon, shells in mesh holding it) uncompilable. Re-sequence, widen, or split
in place?

**Choice.** Re-sequence (the package's option i). `dm-daemon-mesh-edge` now
depends on `REVIEW-build-mesh-host-decouple` (done), not on this row, so the
chain `edge -> http-a -> http-b -> jobs` moves the 21 shells to
sovereign-daemon first — the daemon may name mesh, so they compile holding
`sovereign_mesh::daemon::EmbeddedDaemon` — and this row then waits on
`dm-daemon-mesh-jobs` and on `REVIEW-build-daemon-parts` (DC §4.1's "once
Fabric owns its state they are Fabric's methods" is a forward reference until
`state/fabric.rs` lands in sovereign-mesh — the package's supporting fact 1).
The four files carrying inherent `impl EmbeddedDaemon` move with the type, not
with the shells (E0116). The split is minted as three sub-rows rather than one
commit (7,500+ lines over five files, 62 external sites; the ten-file grammar).

Option (ii), widening the row to absorb the shell moves, is ~15,000 lines in
one commit; option (iii), splitting in place, leaves the type in mesh and does
not deliver DC §4.1's by-owner outcome.

**Evidence.** ralph/NEEDS_HUMAN.md (the worker's package, measurements
reproduced); `git grep -l 'Arc<EmbeddedDaemon>'` = 25 files;
`quality/ARCH_LAYERS.toml:739-742`; DC §4.1 ("EmbeddedDaemon splits by owner,
not size"); STATE.md:177-180 (the dependent chain); `wc -l` on the seven files
(daemon.rs 5,638 + daemon_services.rs 1,084 + lib.rs 168 + the four impl files
673). The director's own session produced nothing in 33 minutes and timed out;
resolved by the supervisor session instead.

**Falsified by.** A shell that turns out to need the daemon-side type before
the type moves (LINT would fail on the chain); or DC §4.1 revised so the
membership methods stay on the host type, which would make the split
unnecessary.

**Landed in.** this commit — `ralph/STATE.md` (three edits: the two `depends`
lines, the row's RE-SEQUENCED note, and the row back to `[ ]`) and this entry.
`git revert <sha>` reverts it alone.

## 2026-09-17 · dm-daemon-api-edge · the api host cluster is atomic; five rows fold into one

**Fork.** `dm-daemon-api-edge` cannot execute as written. The package
(`ralph/NEEDS_HUMAN.md`, 22:25) names two independent blockers: every file the
row moves reaches `crate::state` (or `crate::routes_inference`) and `state.rs` /
`server.rs` / the route shells reach the edge back — a Cargo package cycle if
the edge moves alone — and `[[forbid]] sovereign-api -> sovereign-*`
(`quality/ARCH_LAYERS.toml:711-741`) does not except `sovereign-daemon`, so §3a's
own shim direction and the consumers' repoint both fail `LAYER`. The design
already says the cluster is atomic: `quality/DOMAINS.toml` api-9, "host
(18,930), frontdoor.rs included, whole -> sovereign-daemon. LAST. INTERLEAVE:
with dm-mesh-host; the two host clusters land in ONE crate or the AppState edge
just changes address."

**Choice.** Fold the api host cluster's rows into this one and move it whole in
ONE commit: the edge, `state.rs` with its six parts, `server.rs` and the routes.
`dm-daemon-api-state`, `dm-daemon-api-http-a/b1/b2` and `REVIEW-build-peer-wire`
are absorbed (marked `[x]` with an ABSORBED note; their `depends` stay
satisfied). `REVIEW-build-daemon-parts` now depends on this row, not on
`dm-daemon-api-state`. The same commit carries what the cluster needs to
compile: (a) `sovereign-peer-wire` (the daemon↔daemon wire types, so the moved
routes and the three mesh loops share one leaf); (b) `state/fabric.rs` →
sovereign-mesh with the three loops repointed to Fabric's own state (the
`REVIEW-build-mesh-api-decouple` deferral); (c) the external consumers
(cli-daemon 35, cli-llm 24, cli-dev 3, the harness) repointed to
`sovereign_daemon::…`.

A sequence of small commits does not exist: no subset of the cycle compiles, and
the one-way forbid blocks the shim that would make a partial move legal.

**Evidence.** `ralph/NEEDS_HUMAN.md` 2026-09-17 (b) (the package's measurements);
`quality/DOMAINS.toml` api-9; `quality/ARCH_LAYERS.toml:711-741`;
`git grep -n 'impl EmbeddedDaemon'` and the `crate::state` reach measured in the
package; the lane logs (`target/ralph/lane-dm-daemon-api-edge.out`) showing the
row's own worker reaching the same wall and stopping.

**Falsified by.** A working split of the cycle (a port or reader that lets the
edge move before the state); or the operator widening api's `except` to include
the daemon or a wire leaf, which would make a partial move legal.

**REVIEW-AFTER:** the fold marks five rows `[x]` without their own commits —
the operator may prefer the rows kept `[ ]` with a re-scope instead of an
absorb; and the one-commit move is ~19k lines, far past the ten-file grammar,
which the operator may want split by file family if a compile-only-once path
can be shown.

**Landed in.** this commit — `ralph/STATE.md` (the row's new scope, five
ABSORBED marks, `REVIEW-build-daemon-parts`' dep) and this entry.
`git revert <sha>` reverts it alone.

## 2026-09-17 · dm-daemon-api-edge · the lane's package: the row's premises were wrong in six places

**Fork.** The api-edge lane — refreshed onto the base, so the `.ralph` allow
reached it — did the deep analysis and stopped with a package in its worktree
(`.ralph/wt/dm-daemon-api-edge/ralph/NEEDS_HUMAN.md`, invisible to the pool
until the wave check was fixed in the same commit): the folded row's premises
are wrong in six places. Fix the row, or let it fail again?

**Choice.** All six applied as row surgery; nothing moves destination.
1. **P0** — `sovereign-mesh-test-harness` names `sovereign-api::server`/`state`
   against a no-except `[[forbid]]` (ARCH_LAYERS.toml:754-757): minted
   `REVIEW-build-harness-oicp-seam` (the forbid's own stated fix — simulate
   against the OICP/contracts seam) and added it to the deps; widening the
   except instead is an operator row.
2. **Split the loops** — minted `REVIEW-build-mesh-loops-decouple`
   (`state/fabric.rs` → sovereign-mesh, `MeshMutationHook` with it, the 16
   accessors, the three loops taking Fabric's part plus the node's engine and
   the `SelfClaims` answer, the callers in `daemon.rs`) and added it to the
   deps.
3. **The wire leaf holds four items, not six** — un-absorbed
   `REVIEW-build-peer-wire` with the correction: `JoinRequest`/`GossipRequest`
   already live in `commonwealth_core::mesh::wire` and are re-exports.
4. **`dm-auto-recover-move` runs first** — added to the deps.
5. **`sovereign-api/tests/` (13 files) joins the move set.**
6. **Re-priced**: 32,284 host lines, ~45 source + 42 test files.

**Evidence.** The lane's package (its commands and outputs, now surfaced to
`ralph/NEEDS_HUMAN.md` by the new wave check); the `git grep` sites named in
each item; `git grep -n 'sovereign_api::'` over the harness and mesh.

**Falsified by.** A `REVIEW-build-mesh-loops-decouple` attempt that cannot move
`state/fabric.rs` without breaking the api side (then the port is the answer,
as that row says); or a seam fix that still leaves the harness naming
`sovereign-api`.

**Landed in.** this commit — `ralph/STATE.md` (four edits: the deps line, the
correction note, two minted rows, the un-absorb) and this entry.
`git revert <sha>` reverts it alone.

## 2026-09-16 · REVIEW-build-api-host-decouple · the daemon's edge is host; the AppState reads defer to the state dissolution

**Fork.** The row lists 29 non-`host` → `host` references across seven host
modules and asks to resolve each ("moves to a leaf both sides can name or
becomes a port"), noting the `middleware` seam lifts to sovereign-contracts
only with the answering move. Sixteen resolve against the tree; thirteen do not
— the `AppState`/`AppStateInner` reads and the `server::mock_router` /
`state::test_app_state*` test infrastructure. Are `admission.rs` and
`principal.rs` serving (as DT tagged them) or host, and where do the AppState
reads go?

**Choice.**

1. `admission.rs` and `principal.rs` are **host**, retagged in DT.
   `principal.rs` is `impl AppState { fn resolve }` (an inherent impl cannot
   leave the crate defining the type); `admission.rs` holds `Arc<AppStateInner>`
   and `impl Admission for AppState`. DAEMON_CORE.md §3.3 and SERVING_BOUNDARY
   (c) both call the resolver the daemon's edge and keep the axum middlewares
   host-side, and the serving *decision* already moved at
   `REVIEW-build-serving-move-admission` (`abd718469`). They move with
   `dm-daemon-api-edge` — which does not list them today; that row should absorb
   them. This breaks the census's `host ↔ serving` cycle: serving's tree in
   `sovereign-api` goes 1623/2 → 0/0 and host's 31016/50 → 32639/52.
2. The non-`middleware` items repoint to the leaf that already exists:
   `FimCompletionRequest` / `EditSlotStatus` / `LocalInferenceError` →
   `oicp_types` (already `state.rs`'s own source), `LocalInferenceService` →
   `sovereign_core::traits`; `next_edit_journal.rs`'s unused `State<AppState>`
   extractor is deleted; `turn_fidelity.rs`'s two `crate::frontdoor` doc links
   become plain text.
3. The `AppState` reads **defer**, as the mesh sibling's did
   (`REVIEW-build-mesh-api-decouple`): `auto_recover.rs` to
   `REVIEW-build-daemon-parts` (the ingest ports — engine handle, mesh store,
   emitter, identity reader), `routes_edit_predictions.rs` (the foreground
   signal, Serving's local-inference handle, the test node) to
   `dm-daemon-api-state` / `REVIEW-build-daemon-parts`, `server::mock_router` to
   the test-node assembly DC §4.2 names. The `middleware` seam (5) defers to the
   answering move, which the row itself states.

**Evidence.** `git grep` over the DT module tags: 29 sites before, 13 after,
all named above. `python3 scripts/domains-census.py plan --crate sovereign-api`
before (serving tree 1623/2, `host -> serving` import) and after (serving tree
0/0, host imports no serving). DAEMON_CORE.md §3.3, §4.1, §4.2;
SERVING_BOUNDARY.md (c); `git show abd718469`.

**Falsified by.** A showing that the resolver can leave `sovereign-api` —
E0116 would need `AppState::resolve` to become a free function over a port the
daemon implements, after which the resolver could live in
`sovereign-serving-host`; or a port introduced for the AppState reads that
removes the need for the state dissolution to carry them.

**Landed in.** `0cb82a426` (code + DT; `fba39af69` is its rustfmt) and the
`ralph: REVIEW-build-api-host-decouple done` commit (`ralph/STATE.md` + this
entry). `git revert 0cb82a426` reverts the code half alone.

## 2026-09-16 · REVIEW-build-middleware-seam · the seam stays in sovereign-contracts; the pipeline config moves to oicp-types

**Fork.** The row lifts the middleware seam into `sovereign-contracts` and says
`PipelineContext.context_config: serving_policy::pipeline_aliases::PipelineContextConfig`
is "legal from sovereign-contracts" because serving-policy is contract layer.
The lift as implemented is layer-gate-red: `sovereign-contracts → serving-policy`
propagates into BOTH thin surfaces. Where does `PipelineContextConfig` live so
the seam can name it? Three options were packaged: (1) move it to `oicp-types`,
relaxing serving-policy's documented zero-dep; (2) grandfather
desktop/mobile → serving-policy with an `[[exception]]`; (3) move the seam to
`sovereign-core`.

**Choice.** Option 1. Keep the seam in `sovereign-contracts`; move
`PipelineContextConfig` DOWN to `oicp-types`, re-exported by `serving-policy`;
land the `SERVING_BOUNDARY.md` / `serving-policy/Cargo.toml` doc change in the
same commit (principle 3).

Why not 2: adding an `[[exception]]` is operator-only (charter, "Leave these"),
and the thin-surface rule's own header says the denied set is the crates that
can assemble or HOST a backend (ARCH_LAYERS.toml:1126) — serving-policy cannot,
so the fix is to remove the edge, not grandfather it.

Why not 3: the seam must be nameable by every `Middleware` implementor, and DT
tags `decision_extractor` `workspace` (DOMAINS.toml:1095-1098), whose home is
`corpus-engine-notes` — a knowledge-layer crate that may name only the leaves
(`[[forbid]] corpus-engine* → sovereign-* except sovereign-contracts`,
ARCH_LAYERS.toml:350-354). `sovereign-core` is not a leaf, so a seam there
closes the workspace adapter's path and needs `dm-decision-extractor-move`
re-scoped too — a larger change than this fork requires.

Why option 1 is smallest and keeps the property: `oicp-types` is on `may_reach`
(ARCH_LAYERS.toml:1179-1187), so the closure stays clean; `sovereign-contracts`
already names `oicp-types` (Cargo.toml:22), so the seam gains no new edge;
`serving-policy → oicp-types` is not caught by its two `[[forbid]]` rows
(`sovereign-*`, `commonwealth-*`; ARCH_LAYERS.toml:400-408), so the property
they pin — no cross-family edge — is unchanged; and `oicp-types` already holds
the sibling `model_aliases` table, whose header states the rule ("a mapping
from a name ... belongs to the protocol rather than to any one runtime",
oicp-types/src/model_aliases.rs:5-9). Only serving-policy's "empty in-repo dep
list" letter changes, and the row now says so.

**Evidence.** Reproduced red AND green with a two-line manifest experiment
(unused deps; layer-gate reads the declared graph). Adding
`sovereign-contracts → serving-policy` + `serving-policy → oicp-types` printed
the exact two violations the package reports — desktop via sovereign-contracts,
mobile via sovereign-turn-client → sovereign-contracts. Removing only the
`sovereign-contracts → serving-policy` edge printed "✓ ... no thin surface
reaches a backend it could become". Baseline was green; the experiment was
reverted (`git checkout -- serving-policy/Cargo.toml
sovereign/crates/sovereign-contracts/Cargo.toml Cargo.lock`). Glob facts:
ARCH_LAYERS.toml:400-408, :1179-1187, :350-354, :1126; DOMAINS.toml:1095-1104.
The row's second premise was ALSO false and is corrected: `routes_inference.rs:19-21`
names the seam (`MiddlewareError, MiddlewareSession, PipelineContext,
ResponseView`), not only `MiddlewareRegistry`.

**Falsified by.** A showing that `decision_extractor` does not need the seam —
that its `Middleware` impl can leave sovereign-api without naming the trait —
which would let the seam live in `sovereign-core` (DAEMON_CORE.md:350,419-421)
and keep serving-policy's empty dep list; or an operator decision to
grandfather the thin-surface closure instead (option 2).

**Landed in.** this commit — `ralph/STATE.md` (row `[~]`→`[ ]` and the
corrected resolution), this entry, and the removal of `ralph/NEEDS_HUMAN.md`.
The worker implements the corrected row next.

**REVIEW-AFTER:** the charter does not clearly cover relaxing a documented
crate contract (serving-policy's "ZERO in-repo deps" → "names only
`oicp-types`, the family-neutral floor"). If the operator reads that contract
as absolute, take option 3 (seam + `dm-decision-extractor-move` to
`sovereign-core`) instead.

## 2026-09-16 · REVIEW-build-next-edit-crate · the tree-sitter registry is package-illegal; the stub lands and the move row resolves it

**Fork.** The row creates `code-next-edit` in the code-intel package and lists
`corpus-engine` among its deps (`next_edit_symbols.rs:197,:237`;
`next_edit_syntax.rs:111` — `corpus_engine::extractors::code::language_for_extension`).
But the row's own DT pointer says the package may name only its own crates plus
`oicp-types` / `sovereign-contracts` / `oicp-client`, and the campaign forbids a
new `[[exception]]`. Declare `corpus-engine` and boundary-gate goes red; carve
the tree-sitter registry into a leaf now and the CREATE row grows a design; or
land the stub with no deps and hand the resolution to `dm-next-edit-move`.

**Choice.** The stub. `code-next-edit` is created at the repo root beside
`corpus-engine-notes` with an empty `[dependencies]` (as `sovereign-daemon` and
`sovereign-scheduler` were), added to the root workspace members, the code-intel
`[[package]]` and the `knowledge` layer, and one `SYSTEM_OVERVIEW.md` line. Its
`src/lib.rs` records the placement decision — the five pure modules in
(`next_edit`, `next_edit_model`, `next_edit_symbols`, `next_edit_syntax`,
`next_edit_journal`); `routes_edit_predictions.rs` to `sovereign-daemon` per
DC §4.1's placement test — and the one reach the move must resolve before
`next_edit_symbols.rs` / `next_edit_syntax.rs` land: the tree-sitter registry is
`corpus-engine`'s, and `corpus-engine` is not a package crate.

A second, smaller wrinkle is recorded for the same row: `next_edit_journal.rs`
carries one route shell (`OutcomeWire` + `edit_prediction_outcome`, registered
at `server.rs:175`), which DC §4.1's placement test would send to the daemon;
the move either splits it or takes an `axum` dependency.

**Evidence.** Reproduced red AND green with a one-line manifest experiment.
Adding `corpus-engine = { workspace = true }` to `code-next-edit/Cargo.toml`
printed `✗ [code-intel] code-next-edit → corpus-engine: a normal dependency
leaves the package closure` and `boundary-gate FAILED (1 violation(s))` (exit 1);
removing it printed `✓ every declared package reaches only itself + the shared
leaves` (exit 0). The rule is `quality/arch-layers/src/packages.rs:188`
(`evaluate_packages`); `corpus-engine` is neither a code-intel crate nor a
`[[package_leaf]]` (ARCH_LAYERS.toml:938-963, the leaf list). The row's DT
pointer is `quality/DOMAINS.toml`'s workbench cluster note. Green after the
revert: LINT exit=0, LAYER exit=0, BOUNDARY exit=0, DOCS exit=0, TOML exit=0.

**Falsified by.** An operator approval of an `[[exception]]` carrying
`package = "code-intel"` for `code-next-edit → corpus-engine` (then the crate may
name it and the stub may declare it); or a showing that the tree-sitter registry
is already reachable from a package crate (`corpus-engine-scip` does not export
`language_for_extension` / `LanguageConfig` today).

**Landed in.** this commit — `code-next-edit/` (the stub), the root
`Cargo.toml` member, `quality/ARCH_LAYERS.toml` (package + layer),
`sovereign/SYSTEM_OVERVIEW.md` (the §1 crate line) and `Cargo.lock`. The worker
implements `dm-next-edit-move` against the corrected dep constraint next.

## 2026-09-17 · dm-daemon-mesh-edge · the mesh host cluster is one atomic unit; two rows fold into one

**Fork.** `dm-daemon-mesh-edge` cannot execute as written. It moves ten modules
(`local_only`, `loopback_guard`, `http_response`, `types`, `slot_manifest`,
`mcp_router`, `mcp_config_http`, `features_http`, `enrich_http`,
`landscape_digest_http`) and repoints "their consumers (cli-daemon, cli-llm,
cli-dev) in the same commit". But six of the ten sit inside one
strongly-connected component with `daemon.rs` and the 21 route shells, and 24
mesh files that STAY reference them. Repointing a staying mesh file at
`sovereign_daemon::…` is `[[forbid]] sovereign-mesh -> sovereign-daemon`
(`quality/ARCH_LAYERS.toml:749-752`, no `except`). The row's three-wave lane
never reached the move — it hit the harness's external-directory wall (fixed in
`2882c79a2`) — but the wall behind it is the forbid, not the harness.

**Choice.** The cluster is atomic; fold the rows the design already says move
together. `dm-daemon-mesh-edge` becomes the ONE commit that moves the whole
mesh host cluster: the 34-module must-move-together closure (28,791 lines) plus
the three pure leaves it already named (`http_response` 120, `types` 13,
`slot_manifest` 35) — 37 files / 28,959 lines. `dm-daemon-mesh-http-a` and
`-http-b` are absorbed (marked `[x]`; every shell is in the SCC) and
`daemon_services.rs` moves out of `dm-daemon-mesh-jobs` (it is in the SCC). The
2026-09-16 re-sequence (`3bf3ce35d`, "the shells move first") is withdrawn:
`daemon.rs` mounts every shell (`crate::mesh_http::mesh_router` … `:3385-3517`),
so the shells cannot leave before the type and the type cannot leave before the
shells. `REVIEW-build-daemon-embedded-split` keeps its position (after
`dm-daemon-mesh-jobs` + `REVIEW-build-daemon-parts`) and now splits the
daemon-resident `daemon.rs` by owner, moving Fabric's membership operations
back to `sovereign-mesh` (DC §4.1).

**Evidence.** Reproduced 2026-09-17 by Tarjan SCC over the `crate::` module
graph of `sovereign/crates/sovereign-mesh/src`: the SCC containing `daemon` is
31 modules; the closure under "references a member" is 34 modules / 28,791
lines (30 host-tagged, 4 fabric-tagged — `venue_host`, `media_reach`,
`origin_fanout`, `roster_repair`, each carrying an `impl EmbeddedDaemon`,
E0116); no module outside it references a member. The cycle edges: `daemon.rs`
mounts the 21 shells and the four cyclic leaves (`mcp_router`,
`mcp_config_http`, `features_http`, `enrich_http`); the shells hold
`Arc<EmbeddedDaemon>`; those four hold `EmbeddedDaemon`. The forbid is read at
`quality/ARCH_LAYERS.toml:749-752` and has no exception
(`grep -n sovereign-daemon quality/ARCH_LAYERS.toml` hits :307, :735, :744,
:751, :756, :974 — :735/:744/:974 are comments; the only live rows are the
layer list at :307 and the two forbids at :751 and :756). Same shape as
`dm-daemon-api-edge` (`9a0ebfcb9`), folded hours earlier for the same reason.

**Falsified by.** A cycle break that lets a subset compile: a registry or port
that removes `daemon.rs`'s mount-list reach into the shells (and its three
shell-type reaches — `admin_http::{ConfigDiff,ReloadResponse}`,
`rpc_warm_http::MeshRpcShardWarmer`), so the shells can move first; or the
operator widening the `sovereign-mesh -> sovereign-daemon` forbid with an
`except` that makes a partial move legal.

**REVIEW-AFTER:** two things the charter did not clearly cover. (1) The fold
makes one row 28,959 lines — larger than the api fold's ~19k and likely past
one lane session, so it may fail its three waves too. The alternative is a
`REVIEW-build` row that breaks the `daemon.rs` <-> shells cycle first (a
mount-list registry plus the three shell types), which would let the move
chunk; the docs do not specify it, and DC §4.1 says the shells move with the
type, so the fold is the doc-backed reading. (2) The whole-then-split shape:
`daemon.rs` lands in `sovereign-daemon` with Fabric's membership operations
still on it, and `REVIEW-build-daemon-embedded-split` moves them back — the
same shape the api fold used for `state.rs` (`9a0ebfcb9`).

**Landed in.** this commit — `ralph/STATE.md` (the re-scoped `dm-daemon-mesh-edge`,
two ABSORBED marks, `dm-daemon-mesh-jobs`'s dep + `daemon_services` note, the
`REVIEW-build-daemon-embedded-split` note) and this entry. `git revert <sha>`
reverts it alone.

## 2026-09-17 · dm-daemon-api-edge · re-sequence after the mesh adapters; the crate's own tests are consumers

**Fork.** `dm-daemon-api-edge` still cannot execute, and the package's reason
(`ralph/NEEDS_HUMAN.md`, 2026-09-17) is directionally right but its evidence is
stale. Written at `c69405292`, it names five `host`-tagged mesh modules that
stay in `sovereign-mesh` and take `sovereign_api::state::AppState` in
production, so the moment `state.rs` lands in the daemon `LINT` goes red and
`[[forbid]] sovereign-mesh -> sovereign-daemon` (`quality/ARCH_LAYERS.toml:749-752`,
no `except`) blocks the repoint. Reproduced at HEAD `480b4a2b2`: only TWO of
those five remain — `newsworthy_host.rs:32` and `work_atlas_broadcaster.rs:52`
— because `dm-daemon-mesh-jobs` has since landed (`480b4a2b2`) and moved
`auto_ingest`/`auto_resume`/`work_donor` into the daemon. Both survivors are
`dm-daemon-mesh-adapters`'s (`ralph/STATE.md:181`). That row is not this row's
dependency and `dm-daemon-mesh-jobs` is now `[x]`, so the pool dispatches this
one first (`its depends are met`) and it cannot compile.

**Choice.** Option 1 of the package, not its option 2. Add
`dm-daemon-mesh-adapters` to this row's `depends` (transitively `-jobs` and
`dm-daemon-mesh-edge`, both `[x]`), and extend (c) to name the crate's OWN
integration tests as consumers. The fuse (`dm-daemon-api-edge` + the three mesh
rows into one commit, ~54k lines) is api-9's literal reading but one lane
cannot finish it and the mesh side has already landed separately; the
re-sequence is the smaller reversible step and keeps the row's scope.

**Evidence.** `git grep -n 'sovereign_api::' sovereign/crates/sovereign-mesh/src`
at `480b4a2b2` hits only the three fabric loops (`gossip.rs:45`, `ring_sync.rs:83`,
`rail_kv_pump.rs:108`), the two adapters, and `join.rs` — the loops are this
row's own (a)+(b), the adapters are `dm-daemon-mesh-adapters`. The tests:
`git grep -l 'sovereign_api::' -- sovereign/crates/sovereign-mesh/tests/main/`
is 29 files, all naming `state::{AppState, FabricSeed, MeshMutationHook,
NodeSeed, LocalInferenceService}`, `server::{internal_router, client_router}`,
`headers::parse_x_node_id`, `routes_inference`, `routes_status` — the items this
row moves — while 50 files already name `sovereign_daemon` (the mesh-edge
repoint). The dev edge the tests need is ALREADY in HEAD:
`sovereign/crates/sovereign-mesh/Cargo.toml`'s `[dev-dependencies] sovereign-daemon`
(added by `dm-daemon-mesh-edge`), and dev edges are never layer-enforced
(`DepKind::Dev`, `quality/arch-layers/src/lib.rs:166-172`), so this is a
repoint, not a new edge. The placement is the docs': `quality/DAEMON_CORE.md`
§4.2:412-415 ("Tests build the node the way production does: the 63 test sites
that construct `AppState` directly … move to a test node produced by the same
assembly") and §4.1:294-297 (the host sits at tier 5, not in `sovereign-cli-daemon`,
so "`sovereign-mesh`'s own tests could not reach it" does not bite). The
`sovereign_api::auto_recover::*` refs in the same files are `dm-auto-recover-move`'s
and are left for it.

**Falsified by.** An `except` on the `sovereign-mesh -> sovereign-daemon` forbid
that makes a partial move legal (then `dm-daemon-mesh-adapters` need not
precede this row); or a showing that the two adapter modules are not
`sovereign-mesh`'s to move (they are tagged `host`, `quality/DOMAINS.toml`).
The row's own scope (a)-(c) is unchanged otherwise.

**REVIEW-AFTER:** the charter clearly covers row order and re-scoping, but two
judgement calls are worth the morning's eye. (1) The package's option 1 offered
"repoint the tests at a dev-dep"; the dev-dep already exists, so this became a
pure repoint — I did not verify that all 29 files' non-`auto_recover` refs are
this row's rather than another row's beyond the grep above. (2) `dm-daemon-mesh-adapters`
was left as its own row rather than folded in; if it fails its waves for the
same E0116/atomicity reason the mesh cluster did, the fallback is to fold it.

**Landed in.** this commit — `ralph/STATE.md` (the `dm-daemon-api-edge` depends,
its (c) test list, the RE-SEQUENCED note) and this entry; `ralph/NEEDS_HUMAN.md`
removed. `git revert <sha>` reverts it alone.

## 2026-09-17 · dm-daemon-api-edge · the lane worktree's harness config is a stale snapshot; refresh a commit-less lane onto the base

**Fork.** `dm-daemon-api-edge` failed all three waves without producing a
diff, so the row's size and atomicity are NOT yet implicated — the harness is.
The lane worktree `.ralph/wt/dm-daemon-api-edge` is a snapshot of the base
branch taken when the lane was created (`9d1252262`, 11:42), so its
`.opencode/opencode.json` predates `bdb0f24e0` (12:19), the commit that added
the `.ralph/*` external-directory allow. Every wave hit the same auto-reject
and ended 13–19 minutes in, well inside the 7,200s session budget. Options: (a) refresh the stale
worktree by hand and resume; (b) make the pool refresh a resumed lane onto the
base when the lane has no commits of its own, so a harness fix reaches lanes
already in flight.

**Choice.** (b), the structural fix (principle 10 — make it not-remembered),
which also repairs the two stale lanes for free. `Pool.run_lane` now
fast-forwards an existing lane worktree onto `base_branch` when
`git rev-list --count <base>..HEAD` is 0; a lane with its own commits is left
alone because `--ff-only` refuses to rewrite it. `dm-daemon-api-edge` and
`dm-daemon-cli-composition` (both 0 own commits) are refreshed on the next
wave; `dm-rename-fabric` (2 commits, `.done` present) is untouched and merges
as before. The row is NOT re-scoped: no wave produced a diff, so nothing
supports splitting it, and `dm-daemon-mesh-edge` moved 28,959 lines in one
lane after the same class of harness fix, so size alone is not disqualifying.

**Evidence.** The lane `.out` ends on the auto-reject
(`target/ralph/lane-dm-daemon-api-edge.out:586-588`); the pool's own log shows
the three failures at 19:01:52Z, 19:16:21Z and 19:30:49Z with the fix landing
at 19:19:33Z (`bdb0f24e0`) — mid-wave-3. `git merge-base --is-ancestor
bdb0f24e0 ralph/dm-daemon-api-edge` is false and the worktree config carries
no `.ralph` entry (`.opencode/opencode.json`, the main tree's, does — :11).
Reproduced red and green: the new test
`PoolTests.test_resumed_lane_is_refreshed_onto_the_base` errors
`FileNotFoundError: …/harness.txt` without the `run_lane` block and passes
with it; `python3 scripts/tests/ralph.py` is 38/38. Also fixed in the same
commit: three lane fakes passed `lambda cwd: …` to `session_for`, which gained
`env=` with the per-lane cargo lock — the Pool lane tests had been erroring
before this change (`TypeError: … unexpected keyword argument 'env'`).

**Falsified by.** A wave that runs on the refreshed worktree and still ends
without its marker, with no auto-reject in its `.out` — then the failure is the
row (size/atomicity) or the model, and the next decision is to split it (the
mesh entry's fallback) or raise the lane timeout. Also falsified if the
refresh damages `dm-rename-fabric` (it must not: `--ff-only` refuses a diverged
branch).

**REVIEW-AFTER:** the charter covers row/execution defects and the harness
fix, but not the lane wall-clock budget. Every wave ended well inside the
7,200s session (`ps` on the live pool: `--session-timeout 7200`), so the
budget was never the binding constraint; if a refreshed lane times out rather
than finishes, that is a budget call for the operator.

**Landed in.** this commit — `scripts/ralph.py` (the `run_lane` refresh),
`scripts/tests/ralph.py` (the new test and the three fakes) and this entry;
`ralph/NEEDS_HUMAN.md` removed. `git revert <sha>` reverts it alone.

## 2026-09-17 · REVIEW-build-mesh-loops-decouple · the row is api-edge's consequence, not its prerequisite; absorb it

**Fork.** The row cannot execute as written. Its MOVE (`state/fabric.rs` →
`sovereign-mesh/src/fabric.rs`) is blocked by the holder: `sovereign-api` may not
name `sovereign-mesh` (`quality/ARCH_LAYERS.toml:711-712`) and holds the part at
`state.rs:325`, so the file cannot leave until `AppState` does; but the row that
moves `AppState` (`dm-daemon-api-edge`) depends on this one — a cycle. The
package's three options: (1) absorb into `dm-daemon-api-edge` (b); (2) re-scope
as a post-api-edge mint that declares the port; (3) approve a new leaf for
Fabric's vocabulary.

**Choice.** Option 1, absorb. `dm-daemon-api-edge` (b) already carries this
row's MOVE and its loop repoint, and api-edge runs *after* the holder move,
where the MOVE is legal. The MOVE is also **forced** there, not merely
convenient: `sovereign-mesh` may not name the daemon's `AppState`
(`[[forbid]] from = "sovereign-mesh" to = "sovereign-daemon"`,
`quality/ARCH_LAYERS.toml:749-752`, no except), so the three loops must be
repointed in the same commit that moves `state.rs`, and they can only take
Fabric's own state — which must therefore be in `sovereign-mesh` by then
(DC §4.2:347 already names `sovereign-mesh` as Fabric's home). Absorbing adds
no work to api-edge; it removes a row that could never run before it.
Option 2's port has no legal home, reproduced: Fabric's vocabulary is
`commonwealth-{core,state,rail,transport}` plus `sovereign-meshapp-registry`,
and `commonwealth-core` sits above `sovereign-contracts`' layer-0, so a port
payload there is the upward edge the layer gate refuses; a port declared in
`sovereign-mesh` needs a `sovereign-daemon` newtype adapter for `AppState` (the
orphan rule) — more work than the move, for a seam DC §4.2 does not ask for
(principle 11). Option 3 is an operator-scale design decision the docs do not
imply.

Also corrected the two collateral claims the package named. (i) The check
`git grep -n 'sovereign_api::' sovereign/crates/sovereign-mesh/src` → 0 is not
one row's: of the 16 sites, 3 are the loops (api-edge (b)), 3 are the wire
types (`REVIEW-build-peer-wire`), and 10 are tests (api-edge (c)); api-edge (b)
and the absorbed row now say so, so no worker tries to close a check
`peer-wire` owns. (ii) `REVIEW-build-daemon-parts` no longer claims the
`state/fabric.rs` relocation: it depended on api-edge and would have found the
part already moved; its fabric clause is now a pointer to api-edge (b), leaving
the other five parts to that row.

**Evidence** (all reproduced in this session).
- `git grep -n 'inner\.fabric' -- sovereign/crates/sovereign-api/src | wc -l`
  = **66**, across **17** files; `sovereign-api/src/state.rs:325`
  `pub fabric: fabric::FabricPart`.
- `quality/ARCH_LAYERS.toml:711-712` (`from = "sovereign-api" to = "sovereign-*"`,
  `except` without `sovereign-mesh`); `:749-752` (mesh → daemon, no `except`).
- `ralph/STATE.md:183` (`dm-daemon-api-edge`) depends on the row, and its (b)
  already reads "move `state/fabric.rs` -> sovereign-mesh and repoint the three
  loops … (closing REVIEW-build-mesh-api-decouple's check)".
- `git grep -n 'sovereign_api::' -- sovereign/crates/sovereign-mesh/src` = 16
  sites: `gossip.rs:45,1049`, `join.rs:47`, `rail_kv_pump.rs:108`,
  `rail_kv_pump/tests.rs:95,187`, `ring_sync.rs:80,83`,
  `ring_sync/{tests.rs:17,27,116,252,364,projection_tests.rs:9,61,snapshot_tests.rs:9}`.
- `ls sovereign/crates/sovereign-mesh/src/daemon.rs` → **No such file**; the
  loops' callers are `sovereign-daemon/src/daemon.rs:1057,1465,1753,2126` and
  `sovereign-daemon/src/work_atlas_broadcaster.rs:84` (`dm-daemon-mesh-edge`).
- `quality/DAEMON_CORE.md:347` (Fabric's home `sovereign-mesh`), `:381-388`
  (`SelfClaims` is the only new port; hosted corpora is not one).
- `python3 scripts/ralph.py plan` after the edit → head
  `REVIEW-build-harness-oicp-seam`; the campaign flows.

**Falsified by.** A working split that lets the three loops compile while
`AppState` still lives in `sovereign-api` after api-edge (then a port exists and
a separate post-api-edge row is the answer, option 2); or an operator widening
api's `except` or the mesh→daemon forbid, which would make a partial move legal.

**REVIEW-AFTER:** the charter clearly covers folding and re-scoping, but two
calls are worth the morning's eye. (1) I corrected `REVIEW-build-daemon-parts`
too — the package named only the loops row, and the two rows' fabric claims were
the same work in two names (principle 8). (2) api-edge (b) now carries the
loops' full detail, growing a row that has already failed waves; the alternative
was a new post-api-edge row, which would re-claim work api-edge must do to
compile.

**Landed in.** this commit — `ralph/STATE.md` (the row `[x]` ABSORBED with the
cycle recorded; `dm-daemon-api-edge`'s `depends` drops the row and its (b)/(c)
gain the loop sites, the callers and the `peer-wire` caveat;
`REVIEW-build-daemon-parts` points its fabric clause at api-edge) and this
entry; `ralph/NEEDS_HUMAN.md` removed. `git revert <sha>` reverts it alone.

## 2026-09-17 · REVIEW-build-peer-wire · the un-absorb's independence premise is false; the CREATE is api-edge (a)'s

**Fork.** `REVIEW-build-peer-wire` (`ralph/STATE.md:188`) was un-absorbed from
`dm-daemon-api-edge` 2026-09-17 (the lane's package, item 3) as "independent,
runs now", carrying the four-item correction. The package
(`ralph/NEEDS_HUMAN.md`) shows the row cannot run: the four items live in
`sovereign-api` (`routes_internal/ring_sync.rs:81-120`, `server.rs:39`) and
`sovereign-api` may not name the new leaf — `[[forbid]] from = "sovereign-api"
to = "sovereign-*"` (`quality/ARCH_LAYERS.toml:711-714`) matches
`sovereign-peer-wire` and its `except` omits it, and a `[[forbid]]` outranks
every allowance (`quality/arch-layers/src/lib.rs:268-274`, `forbidden_by` runs
before package membership). The package's two options: (1) widen the `except`
to add `sovereign-peer-wire`; (2) re-sequence onto `dm-daemon-api-edge` and fold
the CREATE into it.

**Choice.** Option 2, in the stronger form — re-absorb, not merely re-sequence.
Option 1 is the charter's operator-only list verbatim ("widening an `except`
list", `ralph/CHARTER.md` "Leave these for the operator"): the `except` is where
R6's own ledger lives (the comment at `:715-738` distinguishes the
contract-family leaves "never part of R6's measurement" from
`sovereign-serving-host`, "the R6 ledger gaining one entry"), so widening it for
a `mesh-api` peer wire leaf would move R6's number — a gate-measurement decision
the charter reserves. Option 2 is charter-covered ("Row order, re-scoping,
splitting, folding"). Re-sequence alone would leave a row with nothing to do —
`dm-daemon-api-edge` (a) already carries the CREATE — the same work in two names
(principle 8), so `REVIEW-build-peer-wire` is marked `[x]` ABSORBED into
`dm-daemon-api-edge` (a), matching the fold already applied to
`dm-daemon-api-state`, `dm-daemon-api-http-a/b1/b2` and
`REVIEW-build-mesh-loops-decouple`. The row's own four-item correction is now
api-edge (a)'s text, so nothing is lost: the leaf holds FOUR items
(`RingSyncRequest`/`RingSyncResponse` + `RING_SYNC_OPS_BUDGET_BYTES` +
`MAX_REQUEST_BODY_BYTES`), and `sovereign-mesh/src/{join.rs:47,gossip.rs:1049}`
repoint at `commonwealth_core::mesh::wire`, not at the leaf.

**Evidence** (reproduced in this session).
- `ls sovereign/crates/ | grep -i 'peer\|wire'` → empty.
- `grep -rn 'struct RingSyncRequest\|struct RingSyncResponse\|pub const
  RING_SYNC_OPS_BUDGET_BYTES\|pub const MAX_REQUEST_BODY_BYTES' --include='*.rs'
  sovereign/` → `routes_internal/ring_sync.rs:81,84,99` and `server.rs:39` only.
- `grep -rn 'struct JoinRequest\|struct GossipResponse' --include='*.rs'
  commonwealth/` → all four in
  `commonwealth/crates/commonwealth-core/src/mesh/wire.rs:30,57,73,91`;
  `ls sovereign/crates/sovereign-api/src/routes_internal/` → no `join.rs`.
- `quality/ARCH_LAYERS.toml:711-714` (`from = "sovereign-api" to =
  "sovereign-*"`, `except = [contracts, serving-host, grants,
  meshapp-registry, time]`); `quality/arch-layers/src/lib.rs:268-274`.
- `git grep -n 'sovereign_api::' sovereign/crates/sovereign-mesh/src | grep
  routes_internal` → `gossip.rs:1049`, `join.rs:47`, `ring_sync.rs:80`.
- `ralph/STATE.md:183` (a) already creates the leaf; `ralph/DECISIONS.md:280-284`
  and `:363-374` are the prior fold; `:418-420` is the un-absorb.

**Falsified by.** A route or a loop that can name the leaf without the
`sovereign-api` forbid applying (then the row is independent and the un-absorb
was right); or the operator widening the `except` at `:714` (option 1), which
would make the row runnable now and is theirs to take.

**REVIEW-AFTER:** the charter covers the fold, but the operator may prefer
option 1 — widening the `except` so the leaf is created by a row that runs now —
which is a gate decision reserved to them; this entry takes option 2 and leaves
that alternative named rather than taken.

**Landed in.** this commit — `ralph/STATE.md` (the row `[x]` ABSORBED with the
falsified premise recorded; `dm-daemon-api-edge` (a) corrected to the four-item
list with the Join/Gossip repoint) and this entry; `ralph/NEEDS_HUMAN.md`
removed. `git revert <sha>` reverts it alone.

## 2026-09-17 · dm-misnamed-coverage · the coverage hole was already closed; only the note was owed

**Fork.** `dm-misnamed-coverage` (STATE.md:212) states two premises: the
`[[module]]` row for `sovereign/crates/sovereign-contracts/src/env.rs` must be
added, and `sovereign/crates/sovereign-mesh/src/lib.rs` must be re-tagged
`host -> fabric`, so `misnamed`/`crate-lines`/`queue` stop exiting 4 on the
coverage assertion. Both were false on the tree as found.

**Choice.** Make the one delta the row still owed — the `env.rs` note naming
`REVIEW-build-mesh-host-decouple` — and record the two premises as already
satisfied rather than re-do them.

**Evidence.**

- `git log -L '11549,11552:quality/DOMAINS.toml'` → the `env.rs` row
  (`path`/`lines = 25`/`context = "kernel"`) was added by
  `7ea3d6aae REVIEW-build-middleware-seam`, with `note = ""`; the file was
  created by `REVIEW-build-mesh-host-decouple` (`sovereign-contracts/src/env.rs`
  doc comment, line 13).
- `git log -L '540,543:quality/DOMAINS.toml'` → `cafc95dd7
  REVIEW-audit-daemon-1` retagged the `lib.rs` row `host -> fabric` and
  re-measured it at 105 lines.
- `python3 scripts/domains-census.py misnamed` → exit 0;
  `crate-lines --crate sovereign-contracts` → exit 0; `queue` → exit 0.
- `wc -l sovereign/crates/sovereign-contracts/src/env.rs` → 25.

**Falsified by.** A tree where `misnamed`/`crate-lines`/`queue` still exit 4
(a coverage hole), or where the `lib.rs` row reads `host`, or where the
`env.rs` row is absent — then the row's original two edits are genuinely owed.

**REVIEW-AFTER:** none — the row's stated outcome holds; the note is the only
content this unit added. `DEMO-d5-misnamed` still reads `sovereign-mesh` at
62.4% fabric, which is the leavers' business, not this row's.

**Landed in.** this commit — `quality/DOMAINS.toml` (the `env.rs` note),
`ralph/STATE.md` (the `CORRECTED` clause on the row) and this entry.
`git revert <sha>` reverts it alone.
## 2026-09-17 · dm-daemon-cli-composition · the row's six-file move set is four false premises; the composition is 17 files and two reaches must come down first

**Fork.** The row says MOVE six files plus "the `run_daemon` half" of
`mod.rs`/`lifecycle.rs` into `sovereign-daemon`. Measured, four of its facts are
false, and each one alone breaks §3a or LAYER. Options: (a) execute the row as
written and hit the first red; (b) correct the row — enlarge the move set,
exclude `vram_plan.rs`, relocate the two `sovereign-cli-shared` reaches, and
drop the `run_daemon` split — then execute the corrected row.

**Choice.** (b). §6 (2026-09-17) makes a false premise mine to correct; the
correction changes the scope and the text, keeps the unit id, and takes no gate
decision (no `except`, no pass bar, no `HUMAN-` row).

**Evidence** (reproduced in this session, all paths under
`sovereign/crates/sovereign-cli-daemon/`).
- (1) `grep -n 'crate::' daemon_cmd/{bootstrap.rs,mod.rs}` and
  `grep -n 'super::' daemon_cmd/bootstrap.rs` name `crate::supervise` (bootstrap
  :633,:1163,:1223,:1459,:1486,:1547,:1663,:1688,:1730; mod.rs :305,:326,:1421),
  `crate::watcher_supervisor` (bootstrap :2682), `crate::listener_watch`
  (mod.rs :1422,:1529), `crate::corpus_maintenance` (mod.rs :771),
  `super::ocr_install` (bootstrap :2137), `super::workflow_trigger`
  (bootstrap :2151), `super::warn_orphaned_indexes` (bootstrap :13,:2011),
  `super::lifecycle::daemon_pid_path` (bootstrap :12,:2456). §3a step 1 requires
  each to be in the destination or named; none is. The move set becomes
  bootstrap, build/, discovery_policy, tool_registry, solve_http, solve_tools,
  ocr_install, workflow_trigger, atlas_builder, principal, provider, worker,
  workspace + supervise, watcher_supervisor, listener_watch, corpus_maintenance
  = 17 files, `wc -l` 8,875 — DC §4.1 row 4's "≈ 9,000".
- (2) `grep -n 'sovereign_cli_shared' daemon_cmd/vram_plan.rs` → :24,:128,:129,
  :171,:179; `python3` over quality/ARCH_LAYERS.toml places `sovereign-cli*` in
  `hosts` (layer 6) and `sovereign-daemon` in `mesh-api` (layer 5);
  `quality/arch-layers/src/lib.rs:367` emits `UpwardEdge` for `ti > fi` and
  `:350` has no `[[forbid]]`/`[[exception]]` covering it. `vram_plan.rs` is a CLI
  verb (its `HELP` and `wants_help`) and stays with the binary.
- (3) `grep -n 'sovereign_cli_shared' daemon_cmd/bootstrap.rs` → :2630
  `sovereign_cli_shared::repo::current_branch`; same forbid. `sovereign-contracts`
  is already a dep of BOTH crates, so the function moves there (a new `git`
  module beside `rebrand`/`run_lock`, which are the same class of dependency-free
  behaviour) and `sovereign-cli-shared::repo::current_branch` delegates, keeping
  its three `sovereign-cli-dev` callers (code_cmd.rs:659,:984;
  project_cmd/serve.rs:533) compiling. `daemon_cmd/workspace.rs:8` names
  `super::sovereign_root`; that wrapper's body is
  `sovereign_contracts::rebrand::svrnmesh_root()` verbatim
  (`sovereign-cli-shared/src/dirs.rs:22-24`, whose doc forbids re-deriving it), so
  workspace.rs calls the SSOT directly.
- (4) `daemon_cmd/mod.rs` `run_daemon` spawns `crate::log_rotation` (:294,:305)
  and `crate::memory_watch` (:326) and reads both for its exit code (:1526-1536);
  DC §4 preamble: "the memory watchdog, log files … stay with the binary". A
  `run_daemon` split therefore needs a seam (`assemble(...) -> RunningDaemon`
  plus a process wrapper) that this row does not describe, so the assembly
  sequence stays in the binary's `run_daemon` and only its callees move. The
  seam is named in this entry as the follow-on, not silently dropped.
- The `depends [dm-daemon-api-http-b2]` is spurious: `grep -rn 'sovereign_api'
  sovereign/crates/sovereign-cli-daemon/src` → 1 hit, lib.rs:59's tracing filter.
  Left as-is because it is already `[x]`.

**Falsified by.** A tree where the composition names none of those eight source
modules (then the six-file move set is right); or `sovereign-cli-shared` sits at
`mesh-api` or below (then `vram_plan.rs` and `current_branch` move as written);
or `run_daemon` does not touch the watchdog/rotation/exit code (then its half
moves too).

**REVIEW-AFTER:** (3) picks `sovereign-contracts` for `current_branch` on edge
cost — `sovereign-work-atlas` is the semantic consumer but would add a
capabilities dep to every CLI binary that links `sovereign-cli-shared`; a
reviewer may prefer the semantic home. The four duplicate `current_branch`
implementations (`sovereign-cli-dev/src/tools_cmd/registry.rs:254`,
`sovereign-cli-llm/src/claim_cmd.rs:842`, `sovereign-tdd`'s `git` module) are
left alone — consolidating them is a noun-convergence row, not this one.

**Landed in.** this commit — `ralph/STATE.md` (the row `[~]`, corrected) and this
entry. `git revert <sha>` reverts it alone.

## 2026-09-17 · dm-misnamed-coverage · the merge conflict is two append-only DECISIONS entries; keep both and complete the merge

**Fork.** The pool halted on `merge conflict merging ralph/dm-misnamed-coverage —
resolve in the main tree, then resume` (`ralph/NEEDS_HUMAN.md`). The lane (based
on `7c9f6c27a`) and the main tree (which merged `dm-daemon-cli-composition`,
`b7b64e617`) each appended a `## 2026-09-17` entry to the end of
`ralph/DECISIONS.md`; that file is the merge's only conflict. Options: (a) drop
one entry to make the merge trivial; (b) keep both, in commit order, and complete
the merge; (c) re-run the lane on the current base instead of merging.

**Choice.** (b). `ralph/DECISIONS.md` is append-only (its header, line 3): both
entries are real decisions, neither supersedes the other, and dropping one loses
exactly the record this file exists to keep. The entries go in commit order —
`dm-misnamed-coverage` (13:48) before `dm-daemon-cli-composition` (13:53) —
matching the append-at-end convention. The merge is then completed by hand,
including the pool's immediate bookkeeping (row `[x]`, lane worktree removed,
branch deleted): leaving the row `[ ]` for the pool to re-run would resume a
finished lane on a base (`7c9f6c27a`) that no longer matches main, which is the
condition that produced this conflict.

**Evidence** (reproduced in this session).
- `git merge-tree --write-tree --name-only HEAD ralph/dm-misnamed-coverage` →
  conflict in `ralph/DECISIONS.md` only; `quality/DOMAINS.toml` and
  `ralph/STATE.md` auto-merge.
- `git log --oneline ralph/dm-misnamed-coverage` → `0430732ae`, `8eb5d1742` on
  base `7c9f6c27a`; main carries `b7b64e617 dm-daemon-cli-composition: merged
  (pool)`.
- The lane's premises re-verified on the MERGED tree, not trusted from the lane:
  `python3 scripts/domains-census.py --self-test` exit 0 (11 axes, 11/11
  positives caught, 11/11 negatives refused); `misnamed` exit 0 with
  `sovereign-mesh  fabric  13100 / 20990  62.4%`; `crate-lines --crate
  sovereign-contracts` exit 0 (env.rs 25 lines, context `kernel`); `queue` exit 0.
- `quality/DOMAINS.toml:11555-11558` (the env.rs note) and `:539-543`
  (`sovereign-mesh/src/lib.rs`, `fabric`, 105 lines) present after the merge.

**Falsified by.** A `ralph/DECISIONS.md` conflict that is not two appends (then a
content decision, not an ordering one); or the merged tree failing the lane's own
checks (`misnamed`/`crate-lines`/`queue` non-zero), which would make its premises
false on the post-merge base; or the pool re-running the lane despite the `[x]`.

**REVIEW-AFTER:** none — the charter covers resolving the halt ("apply the
smallest change that makes the campaign flow"). The one judgment call is
completing the pool's bookkeeping by hand instead of leaving the lane to be
re-run; recorded here so the morning sees it.

**Landed in.** this commit — `ralph/DECISIONS.md` (both entries, ordered),
`quality/DOMAINS.toml`, `ralph/STATE.md` (the row `[x]`),
`ralph/lanes/dm-misnamed-coverage.done`, and `ralph/NEEDS_HUMAN.md` removed.
`git revert -m 1 <sha>` reverts the merge.

## 2026-09-17 · REVIEW-audit-daemon-2 · the audit's `depends` is missing the api host cluster, so the row is not ready

**Fork.** The pool selected `REVIEW-audit-daemon-2` as the ready review row
(its only `depends`, `dm-daemon-cli-composition`, is `[x]`). The row's first
clause is "no `host`-tagged module remains in sovereign-mesh or sovereign-api".
Measured on the tree: `sovereign-mesh` has zero `host` rows, but the api host
cluster is still in `sovereign-api`. Options: (a) run the audit now, record the
api cluster as a finding, and mark `[x]`; (b) re-scope the audit to the landed
clusters (mesh host + composition) and defer the api clause to
`REVIEW-audit-wave-2`, then mark `[x]`; (c) correct the missing dependency so
the audit runs after `dm-daemon-api-edge`, and leave the row `[ ]`.

**Choice.** (c). §6 (2026-09-17) names exactly this case — "a dependency it
does not name" — and directs a correction, not a stop. (a) would mark `[x]` on
an audit whose stated bar is false. (b) would drop the row's own first clause
to make it pass, which is the bar-weakening §6 forbids and, per the charter,
is the director's call ("Row order, re-scoping ... is a row defect"), not a
worker's. The row keeps its scope and bar; only the missing edge is restored.

**Evidence** (reproduced this session, on `701b67453`).
- `python3 scripts/domains-census.py crate-lines --crate sovereign-api` →
  60 rows / 39,953 lines; **52 of those rows are tagged `host`** and total
  32,310 lines (`admission.rs`, `frontend`/`frontdoor.rs` 5,820, `client_auth.rs`,
  `server.rs`, `state.rs` + its six parts, `routes_*`). The api host cluster has
  not moved.
- `python3 scripts/domains-census.py crate-lines --crate sovereign-mesh` →
  33 rows / 20,990 lines, **zero `host` rows** (fabric / workbench /
  back-of-house only). The mesh host cluster has moved.
- `python3 scripts/domains-census.py crate-lines --crate sovereign-daemon` →
  73 rows / 44,676 lines = mesh host 35,827 + the cli-daemon composition half
  8,849 (DC §4.1's first and fourth table rows). The composition is why the
  row's original "sum of the two host clusters" equality was already short by
  8,849 before the api cluster entered it.
- `ls sovereign/crates/sovereign-api/src` still holds `frontdoor.rs`,
  `client_auth.rs`, `headers.rs`, `reshaping.rs`, `server.rs`, `state.rs`,
  `state/`, `routes_internal/` and the `routes_*.rs` shells.
- The mint (`9dee0015e`) chained the audit after every move through
  `dm-daemon-cli-composition` -> `dm-daemon-api-http-b2`; `9a0ebfcb9` absorbed
  `dm-daemon-api-http-a/b1/b2` into `dm-daemon-api-edge` and marked them `[x]`,
  which severed the only edge from `cli-composition` to the api cluster. The
  corrected `depends` restores it explicitly.
- `git grep -nE 'daemon_cmd/(bootstrap|solve_http|solve_tools|provider|worker|...)'
  -- '*.rs' '*.md' '*.toml'` finds ~30 live references still naming the moved
  composition files (`quality/sabotage/all.toml:409`'s mutant target,
  `quality/DOMAINS.toml:966,2797`, `docs/specs/SOLVE_UX.md:4`,
  `sovereign/docs/specs/{MESH_N4_TOPOLOGY,DAEMON_RESILIENCE}.md`, several
  `sovereign-desktop` doc comments, `corpus-engine/examples/fact_spike.rs:19`).
  `dm-daemon-cli-composition`'s `503c66aac` repointed five citations; these
  remain. They are the audit's to fix, not this correction's.

**Falsified by.** A tree where `sovereign-api` holds zero `host` rows (then the
original `depends` was sufficient and the audit could run as minted); or an
operator ruling that `REVIEW-audit-daemon-2` is wave-1's close and the api host
cluster belongs to `REVIEW-audit-wave-2` (then (b) is the right correction and
the row's text, not its `depends`, was wrong).

**REVIEW-AFTER:** the row is now correctly blocked, and the pool's review lane
has no terminal state for a review that cannot run — it retries a non-`[x]`
review (`scripts/ralph.py:824`) and halts after `max_review_attempts`. No
package is owed under the charter (this correction weakens no bar, widens no
`except`, touches no `HUMAN-` row, and its evidence reproduces), so the halt, if
it comes, is the row-order fork the director owns: run `dm-daemon-api-edge`
(`dm-auto-recover-move` is its last unmet dependency) and let the audit follow,
or re-scope the audit to the landed clusters and give the api clause to
`REVIEW-audit-wave-2`.

Two findings are recorded here for the eventual audit, not fixed by this
correction. (1) `PREPUSH` is RED on the tree as found: `./scripts/pre-push.sh`
exit 1, `1 blocking: arch-gate`, `approach band GREW: lines 202703 -> 202846
(+143)` since the band's 2026-09-15 baseline (`abd718469`); a green needs a real
cut (the `ring_sync.rs`/`scoring.rs` split-out audit-daemon-1 used) and
`--update-baseline` is forbidden (`PROMPT §7`). (2) The ARCH-3 doc drift above
(~30 live references to the moved composition files). Both are the audit's
"fix what you find" work, and the audit cannot run until the api cluster lands.

**Landed in.** this commit — `ralph/STATE.md` (the row's `depends`, the
line-sum clause, the CORRECTED note) and this entry. `git revert <sha>` reverts
it alone.

## 2026-09-17 · REVIEW-audit-daemon-2 · director: the api cluster is wave 1's, and the approach-band red is cut, not banked

**Fork.** The worker corrected the row's `depends` to
`[dm-daemon-api-edge, dm-daemon-cli-composition]` (`7b2e304b8`) and left the
package. Two forks are the director's. (1) Does the api host cluster belong to
`REVIEW-audit-daemon-2` (wave 1) or to `REVIEW-audit-wave-2` (wave 2)? (2) The
`PREPUSH` red — `arch-gate`'s approach band grew 202,703 -> 202,846 (+143)
since `abd718469` — cut it in the audit, or accept the growth and re-baseline?

**Choice.** (1) Confirm the correction; the row's `depends`, not its text, was
wrong. `quality/DOMAINS.toml:4632` names the api host cluster (`api-9  host
(18,930), frontdoor.rs included, whole -> sovereign-daemon. LAST. INTERLEAVE:
with dm-mesh-host; the two host clusters land in ONE crate`) and `:4650` calls
`api-9` "the wave's verdict rung" whose close retires the three `[[exception]]`
rows; `quality/DAEMON_CORE.md:302` lists the same cluster as what the daemon
holds. The daemon host crate is the union of the mesh host and api host
clusters, so `REVIEW-audit-daemon-2` audits both; re-scoping the api clause to
`REVIEW-audit-wave-2` would move a wave-1 verdict into wave 2. (2) The growth is
not accepted and the baseline must not rise: `--update-baseline` is forbidden
(`PROMPT §7`), and raising a counter ratchet is the bar-weakening the charter
leaves to the operator. The audit cuts a band file back under 800 (the
`ring_sync.rs`/`scoring.rs` pattern `REVIEW-audit-daemon-1` used). The +143 is
real accretion, not a move artifact — the moves are net −71 and the shared band
files grew +214.

**Evidence** (reproduced this session, on `701b67453`).
- `./target/debug/xtask arch-gate` -> `207 file(s) / 202846 lines in the
  800-1200 approach band`; `✗ size: approach band GREW: lines 202703 -> 202846
  (+143)`; baseline `quality/baselines/approach_band.txt` = `207 files` /
  `202703 lines`.
- Per-file band diff vs `abd718469`: 14 files added / 14 removed (all moves,
  ~equal size), net −71; shared-file delta +214, led by
  `sovereign-serving-host/src/admission.rs` 800 -> 922 (+122).
- `python3 scripts/domains-census.py crate-lines --crate sovereign-api` ->
  `value: 39953 lines`, 52 `host` rows; `--crate sovereign-mesh` -> 20,990,
  zero `host`; `--crate sovereign-daemon` -> 44,676. The api cluster is unmoved.
- `quality/DOMAINS.toml:4632,:4650`; `quality/DAEMON_CORE.md:302`.
- The pool skips the row with the corrected `depends`:
  `Queue('ralph/STATE.md').first_ready_review()` -> `None`; `pick_wave(2)` ->
  `['dm-mesh-workbench-move-scip', 'dm-vocab-compile-fail-test']`. Removing the
  package resumes the campaign.
- ARCH-3 doc drift reproduced: `git grep -nE 'daemon_cmd/(bootstrap|solve_http|
  solve_tools|provider|worker|...)'` finds 55 live references (the package's ~30
  plus `HISTORY.md`, `.canon/sources/`, `quality/campaigns/`, `research/`). It is
  the audit's, recorded not fixed.

**Falsified by.** A tree where `sovereign-api` holds zero `host` rows (the audit
could run as minted); or a doc putting the api host cluster in wave 2 rather than
`DOMAINS.toml api-9`; or an `arch-gate` run whose band reads <= 202,703 on this
tree (then the red is stale); or a band delta that is entirely move artifacts
(then a path re-key, `PROMPT §3a.6`, clears it without a split).

**REVIEW-AFTER:** the `PREPUSH` ruling. "Fixing the code the gate names" is the
director's and "weakening a pass bar" is the operator's, so the cut is decidable
here — but declining to re-baseline is the operator's standing policy, so the
morning should confirm it. Also noted, no change made: the review lane has no
distinct terminal state for a review whose premise is false and whose `depends`
cannot be corrected; the worker's §6 correction plus the package is the intended
path (correct -> deps unmet -> skip; no correctable dep -> package -> director),
so the "no terminal state" is escalation, not a defect.

**Landed in.** this commit — `ralph/STATE.md` (the row's `DIRECTOR` clause),
`ralph/DECISIONS.md` (this entry), `ralph/NEEDS_HUMAN.md` removed. `git revert
<sha>` reverts it alone.

## 2026-09-17 · the domains campaign · director: the campaign's plan is superseded, so it is not resumed

**Correction to the entry above.** That entry resolved the row-order fork and
removed the package, which would resume the campaign. This entry records why the
campaign is NOT resumed, and restores the package carrying the operator fork.

**Fork.** `c35d235b2` (`docs/FIVE_PROGRAMS.md`) landed at 15:13:53, 34 seconds
before the director's commit at 15:14:27. It states it "Supersedes the
ten-context decomposition in `quality/DOMAINS.md` §4 and the `domains`
campaign's relocation plan" (`docs/FIVE_PROGRAMS.md:3-4`); `quality/DOMAINS.md`
now carries the banner "§4, §7 and §11 do not govern" (`:3-6`); §5 deletes "the
ten-context registry `quality/DOMAINS.toml` and its census script"
(`docs/FIVE_PROGRAMS.md:69-71`) and step 0 deletes the process apparatus
(`:104-106`). The campaign's remaining rows ARE that relocation plan. Continue
it, pause it and begin the new procedure, or finish wave 1 first?

**Choice.** Do not decide it: package it and halt. This is not the charter's
"Row order, re-scoping, splitting, folding, minting rows" — it is the operator
replacing the campaign's plan, authored by the operator minutes earlier, and the
charter's own instruction is "an honest package beats a guessed decision" and
"If the fork is one the charter leaves to the operator, say so in the package —
the options, their costs, and your recommendation — and stop." The director's
recommendation is to pause and begin `docs/FIVE_PROGRAMS.md` step 0; the package
(`ralph/NEEDS_HUMAN.md`) names the three options, their costs, and the one-line
resume.

**Evidence** (reproduced this session, on `ralph/domains-campaign`).
- `git log --format='%h %ci %s' -3` -> `2220dbf93` (15:14:27), `c35d235b2`
  (15:13:53), `7b2e304b8` (15:08:52).
- `quality/DOMAINS.md:3-6` banner; `docs/FIVE_PROGRAMS.md:3-4,:69-71,:104-106`.
- Ready rows are the relocation plan:
  `Queue('ralph/STATE.md').first_ready_review()` -> `None`; ready non-review
  lanes `['dm-mesh-workbench-move-scip', 'dm-vocab-compile-fail-test',
  'dm-decision-extractor-move', 'dm-next-edit-move', 'dm-auto-recover-move']`.
- No `ralph/STOP` exists (`ls ralph/STOP` -> absent), so the operator has not
  asked for a halt through the loop's own mechanism; the supersession is the
  only signal, which is why it is packaged rather than assumed.

**Falsified by.** An operator instruction that the campaign continues to the
transition (then remove the package and the director's row-order resolution
resumes the campaign); or a `docs/FIVE_PROGRAMS.md` revision that keeps the
relocation plan governing (then the campaign stands); or a `ralph/STOP` that
appeared with `c35d235b2` (then the halt is the operator's already).

**REVIEW-AFTER:** the whole entry. The director's row-order resolution of
`REVIEW-audit-daemon-2` (the entry above) is a valid record of the campaign's
own rules and stands if the campaign resumes; it is moot if the campaign is
retired. The morning should read this entry first.

**Landed in.** this commit — `ralph/DECISIONS.md` (this entry) and
`ralph/NEEDS_HUMAN.md` restored (untracked; `.git/info/exclude:21`). `git
revert <sha>` reverts the record alone; the package is a file, not a commit.

## 2026-09-17 · dm-mesh-workbench-move-watchers · two of the three files need an operator-only gate decision; commit_harvest lands, the other two defer

**Fork.** The row moves `commit_harvest.rs` (481), `projects.rs` (674) and
`reindexer.rs` (2,150) into `corpus-engine-watchers`. Measured, two of the three
cannot land and each resolution is a gate decision the charter leaves to the
operator: (a) do the full move and land three red gates; (b) move the one legal
file now and record the ask; (c) stop with a package.

**Choice.** (b). §6 (2026-09-17) makes a false premise mine to correct, and this
correction takes no gate decision — no `except`, no pass bar, no `HUMAN-` row —
so `commit_harvest.rs` moves (it adds no dependency: corpus-engine-notes,
tracing and tempfile are already carried) and the other two defer with the
decision recorded here. Their resolution IS an operator act: §7 forbids
re-baselining a ratchet and §6's hard stops name an `[[exception]]`, so this
entry states the ask instead of guessing it.

**Evidence** (reproduced this session; paths under the worktree root).
- The row calls `corpus_engine::facts` a *test* reach; it is production:
  `grep -n 'corpus_engine::' sovereign/crates/sovereign-mesh/src/reindexer.rs`
  → `:650 use corpus_engine::facts::{...}` and `:708
  corpus_engine::facts_store::FactStore::open`, both inside `run_overlay_merge`
  (a plain `async fn`); the `#[cfg(test)] mod tests` starts at `:1517`.
- `corpus-engine-watchers` is a code-intel `[[package]]` crate
  (`quality/ARCH_LAYERS.toml:963-976`). Adding `corpus-engine` to it prints
  `✗ [code-intel] corpus-engine-watchers → corpus-engine: a normal dependency
  leaves the package closure (docs/CODE_TOOLING_BOUNDARY.md)` and
  `boundary-gate FAILED (1 violation(s))` (exit 1) — reproduced with a one-line
  manifest experiment, then reverted. Clean tree: exit 0.
- The full move also fails the fan-in ratchet twice: `✗ fan-in of
  sovereign-contracts grew 31 → 32` (from `projects.rs:364
  sovereign_contracts::rebrand::projects_json()`) and `✗ fan-in of
  corpus-engine grew 20 → 21` (from `reindexer.rs`). `quality/baselines/fan_in.tsv`
  caps sovereign-contracts at `31` (`:11`) and corpus-engine at `20` (`:4`).
- No re-export reaches either fact (the ARCH-11 move): `corpus-engine-yield`'s
  `[dependencies]` is empty by contract, and `corpus-engine-notes` names
  neither `corpus-engine` nor `sovereign-contracts`.
- Landed: `CLEAN exit=0`; `LINT exit=0` (11 crates, 0 errors); `LAYER exit=0`
  (fan-in within caps).

**Falsified by.** An operator `[[exception]]` carrying `package = "code-intel"`
for `corpus-engine-watchers -> corpus-engine` plus a `fan_in.tsv` raise
(corpus-engine 20→21, sovereign-contracts 31→32) — then the row executes whole;
or a showing that `corpus_engine::facts` is reachable from a package crate today
(it is not: `corpus-engine-scip` exports no facts, and a
`corpus-engine-scip -> corpus-engine` edge is a CYCLE, because
`corpus-engine/treesitter` depends on `corpus-engine-scip`).

**REVIEW-AFTER:** the destination itself. `corpus-engine-watchers` is in the
`build-feedback` context (`quality/DOMAINS.toml:216-229`, `package = ""`) yet
`quality/ARCH_LAYERS.toml:963-976` puts it in the code-intel `[[package]]`; the
workbench cluster's dest is the watchers crate while the workbench context's
crates are `corpus-engine-scip`/`-sections`/`code-next-edit`. A reviewer may
prefer the whole cluster wait for the `code-facts` carve-out
(`docs/CODE_TOOLING_BOUNDARY.md` §2) that would make `facts` package-legal, or
send `reindexer.rs` to `corpus-engine` (ratchet- and package-legal, but it grows
the god-crate the campaign is decomposing).

**Landed in.** `a23d8b663` (the move) and this commit (the row correction, this
entry, and `ralph/lanes/dm-mesh-workbench-move-watchers.done`).

## 2026-09-17 · HUMAN-forbid-harness-except · approved — the harness may name sovereign-scheduler

**Fork.** `HUMAN-forbid-harness-except` (STATE.md:216): widen `[[forbid]] from
= "sovereign-mesh-test-harness" to = "sovereign-*"`
(`quality/ARCH_LAYERS.toml:759-762`) to except `sovereign-scheduler`, so the
Tier-1 simulator can name the routing records it replays — or leave the forbid
and drop the mesh-sim move's premise.

**Choice.** Approved by the operator 2026-09-17: the except gains
`sovereign-scheduler`. An operator-only act (widening an `except`, PROMPT §7 /
charter); it unblocks `dm-harness-except` → `dm-mesh-sim-move` → the
mesh-lines bar (with the merged workbench row, sovereign-mesh lands ~12.8k).

**Evidence.** The row; `quality/ARCH_LAYERS.toml:759-762`; the harness's
`Cargo.toml` and `src/simulated_node.rs` naming the scheduler's records (the
`REVIEW-build-mesh-sim-decouple` range).

**Falsified by.** A harness use of a `sovereign-scheduler` surface beyond the
routing records it replays; or the simulator ceasing to need them.

**Landed in.** this commit — `quality/ARCH_LAYERS.toml` (the except), the row
`[x]`, and this entry.

## 2026-09-17 · dm-harness-except · the simulator's host reach is CUT, not excepted

**Fork.** `dm-harness-except` (STATE.md:218) directs adding
`sovereign-serving-host` to the harness forbid's `except`
(`quality/ARCH_LAYERS.toml:759-762`) "only if the decouple row left that
edge". `REVIEW-build-mesh-sim-decouple` (`7009cc569`) cut the throughput-EWMA
reach but LEFT a second one: `sovereign_serving_host::recorder::new_decision_id()`
at `sovereign-mesh/src/mesh_sim/mod.rs:1541` (introduced by
`REVIEW-build-sched-split-sink`, `4ba55cbc6`). So the conditional resolves
TRUE — but the two rows it collides with say only `sovereign-scheduler` is owed
(`HUMAN-forbid-harness-except`, STATE.md:216: "the REVIEW-build row above
removes … the serving-host reach, so only sovereign-scheduler is owed"), and
widening an `except` is operator-only (`ralph/CHARTER.md:34`; PROMPT §6/§7).

**Choice.** Cut the reach; do not widen the `except`. The `except` keeps
`sovereign-scheduler` (already added by the human row's commit, `9b0b65080`);
`dm-mesh-sim-move` is re-scoped to mint the simulator's own deterministic id
(`d-{oicp_request_id}`) in place of the host mint, so the moved simulator names
only `sovereign-scheduler`. This is the smaller reversible step over the larger
one and the existing surface over a new one (charter "Decide these"), and it
honours the operator's stated design ("only sovereign-scheduler").

**Evidence** (reproduced 2026-09-17).
- `grep -rn 'sovereign_serving_host' sovereign/crates/sovereign-mesh/src/mesh_sim/`
  → exactly one hit, `mod.rs:1541`; `git blame` dates it `4ba55cbc6`
  (2026-09-15), so the 2026-09-16 decouple row's premise ("mesh_sim's
  `throughput_tracking` reach is its serving-host reach") was incomplete.
- The id is a join key, never parsed: `mesh_sim/scoreboard.rs:539-547`
  `origin_of` reads `oicp_request_id` (`sim-{origin}-{seq}`), and the
  scoreboard's own test mints `"d-sim-7-1234"` (`scoreboard.rs:804`) — the
  shape the cut uses. The host mint is random (`recorder.rs:263`
  `Uuid::new_v4`), so the cut also restores the module's stated determinism.
- `sovereign-mesh-test-harness/Cargo.toml` names no `sovereign-serving-host`
  today; after the move it would, and
  `[[forbid]] sovereign-mesh-test-harness -> sovereign-*` has no except for it.
- `quality/ARCH_LAYERS.toml:759-762`; `ralph/CHARTER.md:34`; STATE.md:216, :218, :220.

**Falsified by.** The simulator needing a host surface other than the id mint
after the move (then the widening is genuinely operator-only); or
`origin_of`/the scoreboard starting to read the decision id's shape.

**Landed in.** this commit — `ralph/STATE.md` (the two row corrections),
`quality/ARCH_LAYERS.toml` (the comment pinning the absence) and this entry;
the code cut lands in `dm-mesh-sim-move`.

## 2026-09-17 · dm-rename-leaf-words · `PeerAnswer` is kept; the row's "no DT row" premise is false

**Fork.** `dm-rename-leaf-words` (STATE.md:233) directs renaming a "leaf trio",
its third target `PeerAnswer` -> `AnswerEnvelope`, justified as "a post-adjudication
type with no DT row". The registry disagrees: `quality/DOMAINS.toml:1818-1827`
is a `[[noun]]` row for `PeerAnswer` with `disposition = "decided:keep"`, and the
campaign names it a carve-out. Decide whether to rename anyway (overturning the
keep) or correct the row.

**Choice.** Correct the row; rename only the two types whose DT rows say
`decided:rename`. `PeerAnswer` keeps its name. The row's stated reason is
factually false, and the registry's why — "renaming weakens a custody gate" — is
the one thing the four stop conditions protect (PROMPT §6: weakening a pass bar
stops the worker). Renaming would also spend the type that carries the C9 egress
custody signal (`quality/TARGET_ARCHITECTURE.md:495` records `PeerAnswer` as a
pass-bar type), so the conservative correction is the smaller reversible step.

**Evidence** (reproduced 2026-09-17).
- `quality/DOMAINS.toml:1818-1827` — `name = "PeerAnswer"`, `crate = "kernel-types"`,
  `file = "kernel-types/src/answer.rs:423"`, `disposition = "decided:keep"`,
  `why = "… CARVE-OUT … renaming weakens a custody gate. The one place the word
  peer is load-bearing and correct"`.
- `quality/campaigns/domains.toml:142-145` — the bar's own note names the two
  carve-outs: "… and `PeerAnswer` in kernel-types (C9, egress custody — the one
  place the word is load-bearing; disposition keep, with the why on the row)."
- `quality/DOMAINS.md:433-434` — "`PeerAnswer` in kernel-types is kept: egress
  custody, the one place the word is load-bearing."
- `scripts/domains-census.py:329-336` — `peer_defs` subtracts every noun with
  `disposition = "decided:keep"` by name, so `PeerAnswer` is NOT counted by
  `peer-outside`; `python3 scripts/domains-census.py peer-outside` on the tree
  lists corpus-engine, oicp-types, sovereign-cli-llm, sovereign-daemon and
  sovereign-desktop — kernel-types is absent. The type is no straggler.
- Contrast `PeerTransportReader` (the sibling `dm-rename-api-venues` row's
  "post-adjudication type with no DT row"): `grep -n PeerTransportReader
  quality/DOMAINS.toml` is empty, so that row's premise held. This one's does not.

**Falsified by.** A DT row (or a later operator adjudication) that re-dispositions
`PeerAnswer` to `decided:rename` with the custody reason addressed; or the
custody sweep ceasing to be the type's purpose.

**Landed in.** this commit — the two renames (`oicp-types`, `corpus-engine`), the
`ralph/STATE.md` row correction and this entry.

## 2026-09-17 · dm-rename-leaf-words · director: the conflict is one bookkeeping row; keep HEAD's done-marker and the lane's corrected row

**Fork.** The pool halted on `merge conflict merging ralph/dm-rename-leaf-words —
resolve in the main tree, then resume` (`ralph/NEEDS_HUMAN.md`). The lane (base
`654031d71`) and the main tree (which then merged `dm-rename-desktop-member`,
`c1228b1b3`) each edited the two adjacent row lines in `ralph/STATE.md`; the
lane also appended its own `ralph/DECISIONS.md` entry. Options: (a) drop the
lane's row correction to make the merge trivial; (b) take HEAD's `[x]` for the
already-merged desktop row and the lane's corrected leaf row, complete the
merge; (c) re-run the lane on the current base.

**Choice.** (b). The lane's correction is a premise correction the worker made
under PROMPT §6 and recorded in its own `DECISIONS.md` entry; dropping it would
re-introduce a row that directs a rename overturning a registry `decided:keep`.
The desktop row's `[x]` is real (its lane merged at `c1228b1b3`); the leaf row
stays `[ ]` in the merge commit and the pool's bookkeeping sets it `[x]`, exactly
as the pool does after a clean merge. Source files auto-merged with no conflict
— the two lanes touch disjoint files — so there is no code decision here. The
merge is completed by hand, including the pool's immediate bookkeeping (row
`[x]`, lane worktree removed, branch deleted).

**Evidence** (reproduced in this session).
- `git merge --no-commit --no-ff ralph/dm-rename-leaf-words` → the sole
  conflict is `ralph/STATE.md`; `corpus-engine`, `oicp-types`, `sovereign-api`,
  `sovereign-tools` and `ralph/DECISIONS.md` auto-merge.
- The lane's premise re-verified on the MERGED tree, not trusted from the lane:
  `quality/DOMAINS.toml:1818-1827` is the `PeerAnswer` `[[noun]]` row with
  `disposition = "decided:keep"`; `quality/campaigns/domains.toml:142-145`
  names it a carve-out (C9 egress custody); `scripts/domains-census.py:329-336`
  `peer_defs` subtracts `decided:keep` by name. `grep -rn
  'PeerDescriptor\|PeerAtomRef' --include='*.rs'` over the main tree is empty.
- `SOVEREIGN_CHANGED_PATHS=<the six changed .rs>` `./scripts/sovereign-lint.sh
  --human` → `errors: 0`, `cargo exit: 0`, scope 35 crates including
  corpus-engine, oicp-types, sovereign-api, sovereign-tools.
- `python3 scripts/domains-census.py --self-test` exit 0 (11 axes, 11/11
  positives caught, 11/11 negatives refused); `peer-outside` → `2 in 2 crates`
  (`sovereign-cli-llm`, `sovereign-daemon`), down from the lane's 4 because the
  desktop rename merged first; both remaining are the later `dm-peer-outside-zero`
  row's work.

**Falsified by.** The lane's `PeerAnswer` premise being false on re-check (it is
not — the DT row exists); or the merged tree failing lint, which would make the
rename unsound on the post-merge base; or the pool re-running the lane despite
the `[x]`.

**REVIEW-AFTER:** none — the charter covers resolving the halt ("correct the row
or the code") and the lane's correction was already a worker decision. The one
judgment call is completing the pool's bookkeeping by hand instead of leaving
the lane to be re-run; recorded here so the morning sees it.

**Landed in.** `10c57b68b` (the merge, `ralph/STATE.md` conflict resolved,
lane's `DECISIONS.md` entry included), `ralph/STATE.md` row `[x]` and the pool
marker in the following commit; this entry lands in a third commit. `git revert
-m 1 10c57b68b` reverts the merge.

## 2026-09-17 · dm-decision-extractor-move · the seam's one home collides with the fan-in ratchet; hand-raise, not `--update-baseline`

**Fork.** The row says the moved `decision_extractor`'s seam import "repoints to
sovereign-contracts". `REVIEW-build-middleware-seam` landed the seam in
`sovereign-contracts::middleware` and had `sovereign-api` name it through
`sovereign_core::middleware` precisely because a direct `sovereign-contracts`
edge grows that leaf's fan-in past `quality/baselines/fan_in.tsv`. The row was
minted before that unit landed. Options: (a) name the seam through
`sovereign_core`, as `sovereign-api` does; (b) name `sovereign-contracts`
directly and hand-raise the fan-in cap; (c) stop.

**Choice.** (b). (a) is illegal twice over for the destination:
`sovereign-core` is not a shared leaf, so the code-intel package's boundary
refuses it (`corpus-engine-notes` is a package crate), and `ralph/DECISIONS.md`
2026-09-16 (`REVIEW-build-middleware-seam`, "Why not 3") already rejected the
seam-through-`sovereign-core` shape for exactly this crate. `sovereign-contracts`
is the knowledge layer's one sanctioned sovereign edge —
`[[forbid]] corpus-engine* -> sovereign-*`, `except = ["sovereign-contracts"]`
(`quality/ARCH_LAYERS.toml:358-362`) — so the edge is the design working, not a
god-crate accreting. The ratchet's cap is raised by hand (one line) with a
`SYSTEM_OVERVIEW.md` §10.1ac ledger entry, mirroring `dm-daemon-mesh-edge`'s
§10.1ab: `--update-baseline` would snapshot the whole tree and absorb unrelated
growth (PROMPT §7).

**Evidence** (reproduced in this session).
- The collision is real, not inferred: a `tomllib` count of workspace members'
  non-dev `[dependencies]` + `[build-dependencies]` gives `sovereign-contracts`
  fan-in 31 — exactly the cap `dm-daemon-mesh-edge` set at `87f650f69`
  (`fan_in.tsv`), so `+corpus-engine-notes` is 32 > 31.
- The edge is permitted: `quality/ARCH_LAYERS.toml:358-362` is the
  `except = ["sovereign-contracts"]` row; `corpus-engine-scip` (a code-intel
  sibling) already names the leaf (`corpus-engine-scip/Cargo.toml:73`).
- `cargo xtask layer-gate` after the change and the hand-raise → exit 0, "fan-in
  within caps" (72 members, 391 edges).
- The row's line premises were ALSO stale and are corrected in `ralph/STATE.md`:
  the seam import is at `decision_extractor.rs:50` (not :51) and
  `crate::openai_types` at :51 (not :52), both shifted by the seam lift; and
  `notes_db_path` is `sovereign_core::middleware::notes_db_path` (:52), defined
  at `sovereign-contracts/src/middleware.rs:242` — already at the destination
  layer, so it is REPOINTED, not moved.

**Falsified by.** A showing that `decision_extractor` can implement `Middleware`
without naming `sovereign-contracts` (which would make the seam reachable
without the edge), or an operator reading the fan-in cap as absolute — in which
case the move has no legal destination and the row stops (§6).

**Landed in.** this commit — the two moves, the `sovereign-contracts`/`oicp-types`
deps on `corpus-engine-notes`, the shims, the `fan_in.tsv` hand-raise (31 → 32),
`SYSTEM_OVERVIEW.md` §10.1ac, the `ralph/STATE.md` row correction and this entry.

## 2026-09-17 · dm-next-edit-move · the shell cannot leave sovereign-api before `server.rs`; the registry rides a port; two fan-ins hand-raised

**Fork.** The row MOVEs five pure workbench modules to `code-next-edit` and
`routes_edit_predictions.rs` to `sovereign-daemon`, and says "every consumer
repoints (`sovereign-cli/src/journal_cmd/{mod,next_edit}.rs`,
sovereign-cli-daemon/{lib.rs, daemon_cmd/build/inference.rs, setup_cmd/fim.rs}`)".
Measured, three premises are false. (1) The shell cannot move: it is mounted by
`sovereign-api::server::client_router_for` (`server.rs:161-177`) and the daemon
builds its routers by calling that fn (`daemon.rs:3625-3661`), so
`sovereign-api → sovereign-daemon` is a Cargo cycle (the daemon already names
sovereign-api, `Cargo.toml:9`) on top of `[[forbid]] sovereign-api ->
sovereign-*` with no `sovereign-daemon` except (`ARCH_LAYERS.toml:711-714`).
(2) The named consumers do not name the moved modules at all — they read the
`sovereign_contracts` journal schema, the `next_edit` tracing target and
`NextEditFormat`. (3) `next_edit_model.rs` carries
`include_str!("prompts/instinct_system.txt")`, so `src/prompts/` must move with
it. The row's own correction still stands: the tree-sitter registry is
package-illegal, and the journal module carries one route shell.

**Choice.**

1. Move the five pure modules now. Leave `routes_edit_predictions.rs` in
   `sovereign-api`, repointed at `code_next_edit::*`; it lands with `server.rs`
   at `dm-daemon-api-edge`, whose scope already includes "the routes
   (`routes_internal/*` + `routes_*.rs`)". This is the same deferral
   `REVIEW-build-api-host-decouple` recorded for the shell's `AppState` reads
   (2026-09-16).
2. The tree-sitter registry rides a **port**, not a leaf carve. The row offered
   "carve the registry into a leaf or reach it through a port"; the port is the
   smaller behaviour-preserving step (ARCH 2), keeps ONE registry so `.tsx`
   routing cannot drift (ARCH 8), and adds no crate/workspace/layer/package
   rows. `code-next-edit/src/grammar.rs` declares `Grammar` + `GrammarLookup`
   (a `fn` pointer — the package's own injection shape,
   CODE_TOOLING_BOUNDARY.md §3 rule 5); `sovereign-api`'s
   `routes_edit_predictions::grammar_for` is the one place `corpus-engine` is
   named.
3. The journal outcome route splits to a host route shell,
   `sovereign-api/src/routes_edit_predictions/outcome.rs`, not to the daemon
   (blocked by (1)) and not an `axum` dep in the package crate (no code-intel
   crate carries axum; DAEMON_CORE.md §4.1's placement test puts a route shell
   with the surface that mounts it). `server.rs:176` repoints at it.
4. Two fan-ins are hand-raised — `corpus-engine-scip` 10 → 11 (the symbol lane
   opens a `ScipGraph`) and `sovereign-contracts` 32 → 33 (the journal schema is
   a shared leaf) — with a `SYSTEM_OVERVIEW.md` §10.1ad ledger. `--update-baseline`
   was not run (PROMPT §7).

**Evidence** (reproduced this session).

- The cycle and the mount: `sovereign-daemon/Cargo.toml` names `sovereign-api`;
  `server.rs:161-177` mounts `/v1/edit_predictions` and
  `/v1/edit_predictions/outcome` inside `client_router_for`; `daemon.rs:3625-3661`
  calls `sovereign_api::server::{client_router, client_router_for}` for four
  surfaces. `ARCH_LAYERS.toml:711-714` is the forbid, its except list lacking
  `sovereign-daemon`.
- The consumers: `git grep -n 'sovereign_api::next_edit\|next_edit_journal::'`
  hits only `routes_edit_predictions.rs`, `server.rs`, `examples/next_edit_score.rs`
  and `tests/main/next_edit_symbol_lane_e2e.rs`; the row's cli/cli-daemon files
  name only `sovereign_contracts::types::next_edit_journal`, the `next_edit`
  tracing target and `NextEditFormat`.
- The registry is package-illegal and the port fixes it: `cargo xtask
  boundary-gate` → exit 0, "code-intel 6/6 crates present"; the pre-port failure
  (`✗ [code-intel] code-next-edit → corpus-engine`) is the 2026-09-16 entry's.
- The fan-ins are real, not inferred: `cargo xtask layer-gate` before the
  hand-raise → `✗ fan-in of corpus-engine-scip grew 10 → 11` and `✗ fan-in of
  sovereign-contracts grew 32 → 33`; after → exit 0, "fan-in within caps".
- Checks: CLEAN exit=0; LINT exit=0 (WORKSPACE, errors: 0); LAYER exit=0;
  BOUNDARY exit=0; TOML exit=0; CENSUS exit=0 (11/11 positives caught, 11/11
  negatives refused); TEST(code-next-edit) exit=0 (75 pass, 0 fail);
  TEST(sovereign-api) exit=0 (481 pass, 0 fail).
- `docs-gate` is RED at HEAD with three pre-existing unresolved citations
  (`sovereign-api/src/middleware/decision_extractor.rs`,
  `sovereign-tools/src/notes/response_mine.rs`, `sovereign-mesh/src/mesh_sim/mod.rs`),
  each in an earlier lane's §10.1 ledger entry and none introduced here (verified
  against `HEAD:sovereign/SYSTEM_OVERVIEW.md`). This commit adds no docs-gate
  failure; the three belong to the lanes that moved those files.

**Falsified by.** A showing that `server.rs` can leave `sovereign-api` before
`dm-daemon-api-edge` (then the shell moves now); or that the grammar registry is
already reachable from a package crate (`corpus-engine-scip` exports no
`language_for_extension`, and a `corpus-engine-scip → corpus-engine` edge is a
cycle); or an operator `[[exception]]` for `code-next-edit → corpus-engine`
plus the fan-in raise, which would let the registry be named directly.

**Landed in.** this commit — the five moves plus `code-next-edit/src/prompts/`,
`code-next-edit/src/grammar.rs`, the host `routes_edit_predictions/outcome.rs`,
the `sovereign-api` shims, the DT `[[module]]` re-keys, the
`fan_in.tsv`/`oversized.txt` re-keys, `SYSTEM_OVERVIEW.md` §10.1ad, the
conformance row, the `ralph/STATE.md` row correction and this entry.
## 2026-09-17 · dm-vocab-compile-fail-test · the lane worktree never had the row's pointer; the pool must provision it

**Fork.** The lane failed three waves without producing a diff, so the row's
content is not implicated — the lane never reached it. The row says "read: O8
step 2 and check 10", and `O8` is defined at `ralph/STATE.md:48` as
`.sovereign/features/domains-8-understanding-readmodel/order.md`. That path is
gitignored (`.gitignore:44`, `.sovereign/features/`), so `git worktree add`
never brings it into `.ralph/wt/<unit>/`; the lane found the file only in the
main checkout and its read was auto-rejected as an external directory
(`target/ralph/lane-dm-vocab-compile-fail-test.out:103-105`), ending the
session. Options: (a) add `.sovereign/*` to the opencode external-directory
allow-list and let lanes read the main checkout's copy; (b) provision the
per-host pointers into the lane worktree, as `ralph/STATE.md:36-39` already
tells the operator to do for a peer checkout; (c) inline O8 into the row;
(d) stop.

**Choice.** (b), the structural fix (principle 10; principle 2 — fix the cause,
not the symptom), in `Pool.run_lane`, the same place and shape as the
2026-09-17 `dm-daemon-api-edge` lane-refresh fix. (a) leaves the row's
repo-relative path missing and depends on the worker re-deriving an absolute
path by `find`, and the config hardcodes `/Users/alexsbryan/…` (it is per-host
and the Fedora peer would need its own); (c) forks the order, which is the
campaign's design source, into the queue. The copy is unconditional for an
existing lane, so the current worktree (zero own commits, so it is refreshed
onto the base first) is provisioned on the next wave without a manual step.

**Evidence** (reproduced this session, on `ralph/domains-campaign`).
- `.gitignore:44` is `.sovereign/features/`; `git check-ignore -v
  .sovereign/features/domains-8-understanding-readmodel/order.md` →
  `.gitignore:44:.sovereign/features/`. `git ls-files .sovereign/` is the three
  tracked files only; `.sovereign/features` has zero tracked entries.
- The lane worktree has `.sovereign/` but no `features/`:
  `.ralph/wt/dm-vocab-compile-fail-test/.sovereign/` lists `SOVEREIGN.md`,
  `sovereign.toml`, `sovereign.toml.with-watchers` only.
- The lane `.out` ends on the auto-reject at
  `target/ralph/lane-dm-vocab-compile-fail-test.out:103-105`, and the
  `supervise-1.out` header is the pool's `lane … failed 3 waves`.
- The fix is a COPY, not a symlink, and that is measured: the ignore pattern
  ends in `/` (directory-only), so a symlink at `.sovereign/features` is NOT
  ignored — in a temp repo, `git check-ignore -v sub/.sovereign/features`
  exits 1 and `git add -A` commits the symlink. A copy is a real directory and
  is ignored as the main tree's already is.
- Red then green: `python3 scripts/tests/ralph.py
  PoolTests.test_lane_worktree_gets_host_pointer_dirs` errors
  `FileNotFoundError: …/.ralph/wt/dm-a/.sovereign/features/dm-a/order.md`
  without the `run_lane` block and passes with it; the suite is 39/39.

**Falsified by.** A wave on the provisioned worktree that still ends without
its marker, with the pointer present and no auto-reject in its `.out` — then
the failure is the row (size/model) or the vocab leaf's `trybuild` budget, and
the next decision is a re-scope or a stop. Also falsified if the copy damages
a lane (it must not: `.sovereign/features/` is gitignored, so `git add -A`
cannot commit it) or if a future `.gitignore` drops the trailing slash and the
copied tree becomes committable.

**REVIEW-AFTER:** the charter covers row/execution defects and fixing the code
the halt names, and this is the same class as the `dm-daemon-api-edge` harness
fix. The judgment call is copying a 4.3MB per-host tree into every lane (279
files; it is the campaign's documented peer-checkout step, and the alternative
was a permission entry keyed to this host's home). The morning should confirm
the copy-per-lane policy, not the correctness of the fix.

**Landed in.** this commit — `scripts/ralph.py` (the `_provision_host_pointers`
helper and its `run_lane` call, `import shutil`), `scripts/tests/ralph.py` (the
new test) and this entry; `ralph/NEEDS_HUMAN.md` removed. `git revert <sha>`
reverts it alone.

## 2026-09-17 · dm-next-edit-move · director: the merge conflict is two appended DECISIONS entries; keep both and complete the merge

**Fork.** The pool halted on `merge conflict merging ralph/dm-next-edit-move —
resolve in the main tree, then resume` (`ralph/NEEDS_HUMAN.md`). The lane (base
`239fd535f`) and the main tree (which merged `dm-vocab-compile-fail-test`,
`faa29914a`) each appended a `## 2026-09-17` entry to the end of
`ralph/DECISIONS.md`; that file is the merge's only conflict. Options: (a) drop
one entry to make the merge trivial; (b) keep both, in commit order, and
complete the merge; (c) re-run the lane on the current base.

**Choice.** (b). The file is an appended decision log — every entry is added at
the end (`ralph/PROMPT.md:185` instructs "add the DECISIONS entry"), and the
two entries are independent decisions, neither superseding the other, so
dropping one loses exactly the record this file exists to keep. Commit order:
`dm-next-edit-move` (19:02:48) before `dm-vocab-compile-fail-test` (19:06:29,
the director commit `aa6a12dd8`). The merge is completed by hand including the
pool's bookkeeping (row `[x]`, lane worktree removed, branch deleted); leaving
the row `[ ]` would re-run a finished lane on a base that no longer matches
main — the condition that produced the conflict.

**Evidence** (reproduced this session, on `ralph/domains-campaign`).
- `git merge-tree --write-tree ralph/domains-campaign ralph/dm-next-edit-move`
  → conflict in `ralph/DECISIONS.md` only; `Cargo.lock` and `ralph/STATE.md`
  auto-merge.
- The lane's own checks re-run on the MERGED tree, not trusted from the lane:
  `./scripts/sovereign-lint.sh --human` → exit 0, `errors: 0`, `cargo exit: 0`,
  scope WORKSPACE; `cargo xtask layer-gate` → exit 0, "every edge points down or
  sideways, fan-in within caps".
- The lane's row correction is in the merged `ralph/STATE.md:260` (three false
  premises, the registry port, the shell deferral) and the moved files are at
  `code-next-edit/src/` (`next_edit.rs`, `next_edit_model.rs`,
  `next_edit_symbols.rs`, `next_edit_syntax.rs`, `next_edit_journal.rs`,
  `grammar.rs`, `prompts/`) with the `sovereign-api` shims.

**Falsified by.** A `ralph/DECISIONS.md` conflict that is not two appends (then a
content decision, not an ordering one); or the merged tree failing the row's
checks; or the pool re-running the lane despite the `[x]`.

**REVIEW-AFTER:** a commit `78acad3c4` ("ralph: a lane with commits gets the
base merged IN, not skipped") landed on main at 19:25:53, after this halt
(19:24:04) and before this resolution's merge, and it is NOT in
`ralph/.director-commits` (whose last row is the vocab-lane attempt ending at
`aa6a12dd8`); the domains supervisor was blocked in `resolver_run`
(`~/.svrnmesh/ralph/commonwealth-ai-domains/launchd.log` ends at "dispatching
resolution session 1", 02:24:05Z) and no other `ralph.py` process writes this
tree, so the writer is a concurrent opencode session outside the pool. The
merge takes it as the first parent and its harness change is in the merged
tree; the morning should explain its provenance, because a second writer on the
campaign's main tree is the failure the atlas exists to prevent.

**Landed in.** `8fe3b48b4` (the merge, `ralph/DECISIONS.md` resolved with both
entries, the lane's `.done` included), `71061ce26` (`dm-next-edit-move: merged
(pool)`, the row `[x]`), and this entry. `git revert -m 1 8fe3b48b4` reverts the
merge.

## 2026-09-17 · dm-vocab-door-move · two of the four named readers cannot leave corpus-engine; the door lands with the two that can

**Fork.** The row names four readers to move into
`corpus-engine-vocab/src/read.rs` (`read_atlas_atoms`, `read_atlas_edges`,
`read_atlas_cross_corpus_edges`, `read_atlas_ontology`) on the premise that
"each body uses only std, serde_json and types defined in
corpus-engine-vocab". The premise is false for two of the four. Options:
(a) move the return types too, so all four readers can cross; (b) move the two
whose return types are already in vocab and drop/defer the rest; (c) stop.

**Choice.** (b), and the deferrals are not the same kind. `read_atlas_ontology`
is DROPPED, because the design already decided it: it is minted, not moved.
`read_atlas_cross_corpus_edges` is DEFERRED to a follow-on `REVIEW-build-` row,
because moving it needs a type-home decision (where `CrossCorpusEdgesFile` and
its closure live) that §4 puts in a review row. (a) was rejected because it
turns a mechanical MOVE into a product-type relocation the row never names —
"change nothing the row does not ask for" — and because the deferred types are
not in the DT collision row's survivor set; (c) is wrong because the two
functions that CAN move are the ones every bypass row in this wave needs
(`read_atlas_atoms`), so the wave is unblocked.

**Evidence** (measured this session, worktree `dm-vocab-door-move`).
- `read_atlas_ontology` (`corpus-engine/src/enrichment/atlas/writer.rs:442`)
  returns `AtlasOntologyFile`, defined at `writer.rs:378`, not in vocab; its
  body calls `tracing::warn!` (`:447`). `grep -rn tracing
  corpus-engine-vocab/src` is empty, and the leaf's budget admits no tracing
  (DT:3075-3094; O8 check 11 names the deps exactly). DM §10.5:484 and
  DT:3092 both say "read_atlas_ontology is MINTED, not moved"; DT's door
  collision row says "three fns MOVE" (`:3091`), so the row's four is one more
  than the design.
- `read_atlas_cross_corpus_edges` (`writer.rs:562`) returns
  `CrossCorpusEdgesFile`, defined at
  `corpus-engine/src/enrichment/atlas/cross_corpus.rs:132`, with closure
  `CrossCorpusEdge` (`:53`), `CrossCorpusAtomRef` (`:71`) and `MatchTrace`
  (`:108`). `grep -rn 'CrossCorpus' corpus-engine-vocab/src` is empty, so the
  leaf cannot name the return type.
- The two that moved satisfy the premise exactly: `read_atlas_atoms` ->
  `AtomsFile` (`corpus-engine-vocab/src/atoms.rs:1528`), `read_atlas_edges` ->
  `EdgesFile` (`corpus-engine-vocab/src/edges.rs:210`); both bodies are
  `fs::read` + `serde_json::from_slice`.
- Green on the corrected row: LINT exit 0 (scope includes corpus-engine-vocab,
  corpus-engine and 21 dependents); LAYER exit 0; TEST(corpus-engine-vocab)
  exit 0, pass 54 fail 0; TEST(corpus-engine) exit 0, pass 2299 fail 0.
- Census stays green: the new `corpus-engine-vocab/src/read.rs` gained its
  `[[module]]` row (`quality/DOMAINS.toml`, context `understanding`), so
  `misnamed` exits 0 and `crate-lines --crate corpus-engine-vocab` reads
  5,304 -> 5,346; `atom-outside` is unchanged at 12.

**Falsified by.** A follow-on finding that `CrossCorpusEdgesFile` (or a
vocabulary-side replacement) is movable without a type-home decision, or that
`read_atlas_ontology` can cross without `tracing` and without its return type
— either makes this narrowing unnecessary. Also falsified if the leaf could
not host `read.rs` without a new dependency: it needed none (std + serde_json,
both already present).

**REVIEW-AFTER:** the judgment call is deferring the cross-corpus reader
rather than moving its product types with it. The morning should confirm the
deferral (and mint the follow-on row) rather than the correctness of the two
that moved.

**Landed in.** this commit — `corpus-engine-vocab/src/read.rs` (new),
`corpus-engine-vocab/src/lib.rs`, `corpus-engine/src/enrichment/atlas/writer.rs`
and the `[[module]]` row in `quality/DOMAINS.toml`.

## 2026-09-17 · REVIEW-build-vocab-seal · the census's "one AtomsFile" predicate collides with the design's named wire twin

**Fork.** The seal (STATE.md:246) mandates "a private deserialize-only wire
twin", and the registry names it: `struct AtomsFileWire`
(quality/DOMAINS.toml:3113, "AtomsFile (the one door, structural)").
`corpus-engine/xtask/tests/atoms_file_census.rs`'s `is_atoms_file_decl` matches
every struct whose name starts with `AtomsFile`, so the mandated name makes
`the_atoms_json_shape_is_declared_exactly_once` read 2 (`atoms.rs:1536`
`AtomsFile`, `:1628` `AtomsFileWire`). Decide: rename the twin (contradicts the
registry), or widen the census predicate.

**Choice.** Widen the predicate — `name.starts_with("AtomsFile") && name !=
"AtomsFileWire"` — with the rationale in the doc comment and a planted negative
in `the_matcher_sees_the_three_shapes_that_were_deleted`
(`assert!(!is_atoms_file_decl("pub(crate) struct AtomsFileWire {"))`). The
census's invariant is that the shape has ONE home and is not re-derived outside
it; the wire twin is the deserialize half of that one declaration, in the same
home, and is the mechanism that makes `AtomsFile` not `Deserialize`. Renaming it
would violate the registry's explicit name, and the design cannot avoid a second
struct: a `Deserialize` impl on a public type cannot be private.

**Evidence.** `cargo test -p xtask` before: the census test FAILED, hits
`atoms.rs:1536 pub struct AtomsFile` and `atoms.rs:1628 pub(crate) struct
AtomsFileWire`; after: pass 117 fail 0 except the pre-existing
`conformance_tags_are_fresh` (`quality/conformance/sovereign-api.toml` line 90
committed vs 89 generated — `git show HEAD:.../routes_edit_predictions/outcome.rs`
has the fn at line 89, and neither file is touched by this unit). DT:3110-3133.

**Falsified by.** A finding that the wire twin can be avoided — a crate-private
`Deserialize` impl, or a `pub(crate)` field making the derive unreachable —
which would remove the second struct and let the census stay literally "exactly
one". Also falsified if a second `AtomsFile*` shape appears in `atoms.rs` and
the widened predicate lets it through: the census would then under-count.

**Landed in.** this commit — `corpus-engine/xtask/tests/atoms_file_census.rs`.

## 2026-09-18 · REVIEW-build-index-read-port · the "9 reaches" seam is a cross-file type cascade; the leaf absorbs the persisted-setting and row types, and stream-axes-split's per-corpus half

**Fork.** STATE.md:263 scopes the leaf to the six clean index files + `read.rs` +
"the part of `index/mod.rs`" and says "`mod.rs`'s 9 `crate::{recipe,...}` reaches
are the seam this row decides: each either moves down with the leaf or arrives
through the recipe." Measured, the seam is larger and the row cannot land
atomically as written: (a) the six files are 4,609 lines today, not 4,588
(`wc -l`: search 1245, create 1011, write 796, maintain 595, evidence 524,
provenance 438); (b) the leaf's closure also reaches `crate::error`
(`Error`/`Result`), `crate::types` (`IndexInfo`, `CorpusKind`, `ChunkRange`,
`IncompleteIngest`, `ScoredChunk`, `RerankConfig`, `RerankFn`, `EmbedFn`,
`DedupPicker`), `crate::stream_axes::StreamAxes`, `crate::corpus::Corpus::meta_in`,
`crate::chunkers::CommittedChunk`; (c) `REVIEW-build-stream-axes-split` (the
row that depends on this one) was to move `Stability`/`StreamAxes`/
`StreamAxesSource` into the leaf, but this row's `IndexMeta`/`IndexInfo` name
`StreamAxes` — so the leaf cannot compile without them, and the dependency order
as minted is circular. Decide: stop (§6) and re-mint the whole wave, or correct
the row to the closure it actually has and absorb the split.

**Choice.** Correct the row (§6, operator direction 2026-09-17) to the measured
closure, and let the row's own rule — "each either moves down with the leaf or
arrives through the recipe" — carry it: the persisted-setting and row types MOVE
DOWN, the recipe EMBEDS them. Specifically the leaf absorbs `Error`/`Result`
(moved whole so every `?` and every external `From<corpus_engine::Error>` keeps
one type identity — a narrow leaf error would have broken the 194 external
`CorpusIndex` sites), `Corpus`, `DisplayMeta`/`MutableMergePolicy`,
`FilterConfig`/`ComposeMode`/`KnowledgeDensityConfig`/`BoilerplateConfig`,
`Stability`/`StreamAxes`/`StreamAxesSource`, `CommittedChunk`, and the index row
types; `REVIEW-build-stream-axes-split`'s per-corpus half is absorbed (its
derivation half is a residual). `enrichment.rs` (an `impl CorpusIndex`) and
`readiness.rs` (a predicate on `IndexMeta`, called by `create.rs`) moved too —
an inherent impl cannot cross the crate line, and the predicate belongs with the
type it reads. `field_skeleton.rs` and `raptor.rs` stayed host (free functions;
`raptor` names `crate::enrichment`/`crate::atlas_context` in docs). The one
cross-crate visibility change: `Error`'s feature-gated `From<corpus_engine_scip::Error>`
impl is an orphan on both sides once `Error` moves, so it became a named
`corpus_engine::error::from_scip` the three `?` sites call; `GateInfo` +
`GATE_CACHE_TTL` + `gate_info`/`gate_cache_snapshot`/`gate_cache_backdate`/
`share_gate_cache_from` became `pub` (corpus-engine's `engine/mod.rs` uses them),
and the two test-seam methods lost their `#[cfg(test)]` (a cfg does not
propagate across a dependency edge).

**Evidence.** `ls -d corpus-index` empty before; `grep -o 'crate::[a-zA-Z_]*'`
over the moved files is the reach list above; `git grep` for the historical paths
is unchanged after the re-export shims. Checks after: LINT exit=0 (workspace
clean, 2404 warnings); LAYER exit=0; `TEST(corpus-index)` exit=0 (pass 82, fail
0); `TEST(corpus-engine)` exit=0 (pass 2197, fail 0). The `recipe_schema` and
`evidence_reds` gates both read SOURCE, so their file lists were repointed at the
leaf (`../corpus-index/src/{recipe,filters}.rs`) and the trybuild `.stderr`
regenerated for the new path — the invariant (E0624, `acquired` private) is
unchanged.

**Falsified by.** A later session showing the leaf can be built without the
`Error`/row-type move (e.g. a shared `corpus-error` leaf that keeps the type
identity with a narrower ownership), which would make the `Error` move the wrong
line; or a `corpus-engine` compile that does not need the widened visibility, in
which case `pub` was over-granted. Also falsified if `index::raptor` or
`index::field_skeleton` turns out to belong in the leaf (both are host-side
today).

**Landed in.** this commit — `corpus-index/` (new crate), the corpus-engine
re-export shims (`src/{error,corpus,types,recipe,stream_axes}.rs`,
`src/index/mod.rs`, `src/filters/{mod,boilerplate,knowledge_density}.rs`,
`src/chunkers/mod.rs`), `quality/ARCH_LAYERS.toml`,
`sovereign/SYSTEM_OVERVIEW.md`, `corpus-engine/tests/main/recipe_schema.rs`.

## 2026-09-18 · REVIEW-mint-wave-n · the queue is unreadable until corpus-index is tagged; the wave is corpus-engine's remainder

**Fork.** STATE.md:298 says "MINT the next wave from the head of `python3
scripts/domains-census.py queue`". On the tree as found the instrument refuses:
`queue` exits 4 (could-not-judge), `misnamed` and `crate-lines` with it, because
`corpus-index` — created by `REVIEW-build-index-read-port` (`503681188`) — has 19
`.rs` files under its `src/` and ZERO `[[module]]` rows in `quality/DOMAINS.toml`.
Decide whether to stop (§6) or correct, and what the wave is once the head is
readable.

**Choice.** Correct the row (§6, operator direction 2026-09-17) and proceed.
1. The premise is false; the row is corrected. The head was read with the
   coverage assertion bypassed in a throwaway import (`dc.coverage_holes = lambda
   root: []` before `dc.queue(dc.REPO)`); the queue computation itself is
   untouched, so the head is trustworthy: corpus-engine 171,513 lines / 35.9%
   own, sovereign-core 130,237 / 21.1% next. The wave's FIRST row closes the hole
   (`dm-corpus-index-tag`), because `queue`, `misnamed` and `crate-lines` are the
   instruments every later rung gates on.
2. The wave is corpus-engine's REMAINDER, not the next crate. The queue still
   heads corpus-engine because `REVIEW-mint-wave-3` took only its Understanding
   carve; `plan --crate corpus-engine` [4]-[8] names the leaving clusters left —
   workbench, kernel, back-of-house, workspace, build-feedback (ingest and
   retrieval stay). The campaign's ladder ("WAVES 4+ — whatever dm-queue lists
   next") reads the queue's head, so the head is the wave.
3. The three mid clusters wait on `REVIEW-mint-understanding-tiers` — the cycle
   `understanding -> workbench -> kernel` is real and measured: `code_intel`
   names `crate::enrichment::pipeline::{types,prompts}` (4 sites, DT :4036),
   kernel's `types.rs:17` names `ChatPrompt`, `harness/` names the atlas — so
   those rows carry the dependency and the two that do not (`dm-ce-move-notes-sync`,
   `dm-ce-move-build-feedback`) can land now.

**Evidence.** `python3 scripts/domains-census.py queue` → exit 4, 19 untagged
`corpus-index/src/**.rs`; `grep -c 'corpus-index/src' quality/DOMAINS.toml` = 0;
`git ls-files 'corpus-index/src/**/*.rs' 'corpus-index/src/*.rs' | wc -l` = 19
(10,046 lines by `wc -l`); `python3 scripts/domains-census.py plan --crate
corpus-engine` prints move order "understanding, workbench, kernel, back-of-house,
workspace, build-feedback" with `CYCLE (port or split ...): understanding ->
workbench -> kernel` and 2 problems (workbench dest not in context crates;
back-of-house tier needs the port); `git grep -n 'crate::enrichment::pipeline'
-- corpus-engine/src/enrichment/code_intel` = mod.rs:35,36 + pass.rs:18;
`wc -l corpus-engine/src/{types,error,corpus}.rs` = 127/33/10 (the read-port
carve already emptied the kernel cluster, so DT :4057's 1,523-line figure is
stale); `queue` exit 4 and `predicate` exit 1 mean no `ralph/DONE` row.

**Falsified by.** A queue read on a clean tree whose head is not corpus-engine
(e.g. a tag row landing first, or a registry edit that lifts corpus-engine's own
share to 100%); or `REVIEW-mint-understanding-tiers` changing the prompt/type
surface so that `code-enrich`/`corpus-engine-vocab` are the wrong destinations,
which would re-scope the three waiting rows; or the operator ruling that wave-n
is the next CRATE (sovereign-core) rather than the head's remainder.

**Landed in.** this commit — `ralph/STATE.md` (the corrected mint row and 12
minted rows), `ralph/DECISIONS.md`.

## 2026-09-18 · dm-corpus-mcp-exception · the read-port repoint, and what the exception actually still covers

**Fork.** STATE.md's row says "re-word the row: the exception now covers `ingest`
ONLY (which genuinely ingests) and its `tracking` says so", and cites the
exception at `ARCH_LAYERS.toml:1279-1284`. Decide whether to word the row
"ingest ONLY" as instructed and use the cited lines, or word it by what the tree
shows after the repoint.

**Choice.** Word it by the tree (§6). (1) The line pointer is stale by ~20: the
`sovereign-mesh-test-harness -> sovereign-api` retirement comment landed above
the three `corpus-mcp` rows, so the exception is at 1299-1304 and its siblings at
1285/1292; the row is corrected to those lines. (2) After repointing every
read-only index site to `corpus-index` — `serve.rs` (`CorpusIndex`), `ask.rs`
(`CorpusIndex`, `ScoredChunk`, `ChunkProvenance`), `host.rs` (`EmbedFn`),
`tools.rs` (`CorpusIndex`, `EmbedFn`, `ScoredChunk`) — `corpus-mcp` still names
`corpus-engine` for the atlas reads, the HTTP embedder, recipe templates and the
ingest verb. So "ingest ONLY" is false; the row reads "the engine's own work
(ingest + the atlas reads)". The row does NOT retire, because those sites do not
repoint to the leaf.

**Evidence.** `git grep -n 'corpus_engine' -- corpus-mcp/src` before: the sites
above plus `enrichment::atlas::*` (ask.rs:31-32, tools.rs:35-38),
`embed_http::http_embed_fn` (host.rs:96), `recipe_templates::*` (recipe.rs),
`CorpusEngine`/`CorpusSpec`/`IngestResult`/`snapshot` (serve.rs, ingest.rs);
`corpus-index` is a `[[package_leaf]]` (ARCH_LAYERS.toml:871-883) and already a
root `[workspace.dependencies]` entry (Cargo.toml:305), so the new dep passes
boundary-gate (`corpus-mcp 3/3 crates present`, exit 0).

**Falsified by.** A future move that relocates `corpus_engine::enrichment::atlas`
and `embed_http` off `corpus-engine`, leaving only `CorpusEngine`/`CorpusSpec` —
then "ingest ONLY" becomes true and the row's wording is right as minted.

**Landed in.** this commit — `corpus-mcp/src/{serve,ask,host,tools}.rs`,
`corpus-mcp/Cargo.toml`, `quality/ARCH_LAYERS.toml`, `ralph/STATE.md`,
`ralph/DECISIONS.md`.

## 2026-09-18 · dm-understanding-vocab-rename · the consumer set is nine manifests, not four, and the layer glob no longer matches

**Fork.** STATE.md:269 scopes the rename to "the four consumer manifests
(corpus-engine, corpus-mcp, oicp-types, corpus-engine-vocab)" and premises
"39 files / 87 sites" and "5 manifests". Measured at HEAD, the crate is named
by 43 `.rs` files / 91 sites and by 11 `Cargo.toml` files — the root, the
crate's own, and NINE consumers. The row also does not name a dependency that
the rename creates: the `knowledge` layer assigns crates by the
`corpus-engine-*` glob (`quality/ARCH_LAYERS.toml:153`), which stops matching
`understanding-vocab`, so layer-gate fails until the crate is named explicitly.
Options: (a) stop and re-mint; (b) correct the row and do the full rename,
taking the extra consumers and the layer entry with it; (c) rename only the
crate's own dir/manifest and leave consumers on a shim.

**Choice.** (b). The rename is mechanical and the extra consumers are the same
noun — a crate rename is not a design decision, and every consumer must repoint
or the workspace does not compile. (c) is rejected because the row asks for
"every `corpus_engine_vocab::` path", and a `pub use` shim in the crate's own
`lib.rs` cannot keep `corpus_engine_vocab::` resolving across crates (the crate
identifier itself is gone); a shim would also defeat the point of the rename
(ARCH 8: one name per concept). (a) is rejected because the correction is
mechanical: the tree, not the design, moved the count.

**Evidence** (measured this session, worktree `dm-understanding-vocab-rename`).
- `git grep -l 'corpus_engine_vocab' HEAD -- '*.rs' | wc -l` = 43;
  `git grep -c 'corpus_engine_vocab' HEAD -- '*.rs'` sums to 91.
- `git grep -l 'corpus-engine-vocab' HEAD -- '*Cargo.toml'` = 11:
  `Cargo.toml`, `corpus-engine-vocab/Cargo.toml`, `corpus-engine/Cargo.toml`,
  `corpus-mcp/Cargo.toml`, `oicp-types/Cargo.toml`,
  `sovereign/crates/{sovereign-cli-llm,sovereign-core,sovereign-enrichment-build,sovereign-eval,sovereign-mesh,sovereign-tools}/Cargo.toml`.
- `[workspace.dependencies]` is `Cargo.toml:304` (row said :298); the
  `[[package_leaf]]` is `ARCH_LAYERS.toml:862-869` (row said :856-863); the
  "carve-out history and actively misleads" phrase is `ARCH_LAYERS.toml:987`
  (row said :854, which is the `corpus-engine-sections` comment).
- LAYER failed on the first run with "crate `understanding-vocab` is not
  assigned to any layer" because the `knowledge` `[[layer]]` matched by
  `corpus-engine-*`; adding `"understanding-vocab"` explicitly
  (`ARCH_LAYERS.toml:167-172`) made it exit 0.
- Re-keyed in the same commit: the 13 DT `[[module]]` paths under the crate and
  the `[[context]]` `crates`/`roots`/`vocab_roots`; the two
  `quality/baselines/lines.tsv` keys and the `oversized.txt` path (path re-key,
  counts unchanged — 2484/522/1588); `quality/sabotage/all.toml`'s `en-21`
  target; the xtask `boundary_gate` budget pin and `atoms_file_census` roots
  and home; every live doc (`DOMAINS.md`, `TARGET_ARCHITECTURE.md`,
  `SYSTEM_OVERVIEW.md`, `DECOMPOSITION.md`, `EPISTEMIC_INDEX.md`, `SCHEMA.md`,
  `ENV_FLAGS.md`/`env-flags.toml`, `CONCEPTS.toml`, `campaigns/domains.toml`)
  and the `scripts/domains-census.py` fixtures. Three historical mentions are
  left as written: `DECOMPOSITION.md:130` ("landed as corpus-engine-vocab"),
  `DOMAINS.toml:3984` ("= corpus-engine-vocab RENAMED") and `DOMAINS.toml:3990`
  (the re-export precedent).
- Green on the corrected row: CLEAN exit 0; LINT exit 0; LAYER exit 0;
  TEST(understanding-vocab) exit 0 (pass 75 fail 0); TEST(corpus-engine) exit 0
  (pass 2197 fail 0). Extra gates touched by the diff, run as verification:
  BOUNDARY exit 0; TOML exit 0; CENSUS `--self-test` exit 0 (11/11);
  TEST(xtask) exit 0 (pass 118).

**Falsified by.** A consumer that names the crate through a path the rename
does not cover (a `build.rs`, an `include_str!`, a CI workflow, a
`[[package_leaf]]` glob) — the `git grep` above is exhaustive over tracked
files, so any such site would show as a residual `corpus-engine-vocab` or a
build failure. Also falsified if a shim could keep `corpus_engine_vocab::`
resolving cross-crate, which would make (c) viable.

**Not fixed, observed (out of scope).** `cargo xtask docs-gate` is RED on the
base tree, before this unit: `sovereign/SYSTEM_OVERVIEW.md:8654` and `:9986`
cite `corpus-engine/src/index/{search,provenance}.rs`, which
`REVIEW-build-index-read-port` moved to `corpus-index`; and
`Cargo.toml:73-74` puts a quoted phrase (`DE "The read-port leaf, measured\n
again"`) inside the `members` array, which `docs_gate.rs:439`'s
`split('"').skip(1).step_by(2)` mis-reads as a crate name. Both predate this
unit and belong to the index-read-port follow-up / the wave-close audit.

**Landed in.** this commit — the crate rename, its consumers, the registry,
baselines, docs and the xtask pins; `ralph/STATE.md` (the corrected row) and
`ralph/DECISIONS.md`.

## 2026-09-18 · dm-corpus-mcp-exception · director: the merge conflict is the vocab rename crossing the read-port repoint; combine both

**Fork.** The pool halted on `merge conflict merging ralph/dm-corpus-mcp-exception
— resolve in the main tree, then resume` (`ralph/NEEDS_HUMAN.md`). The lane (base
`3046c76e2`) repoints corpus-mcp's read-only index sites onto the `corpus-index`
leaf; the main tree then merged `dm-understanding-vocab-rename` (`17b2d7ace`),
which renamed `corpus-engine-vocab` -> `understanding-vocab`. The two edits
collide in four files: `corpus-mcp/Cargo.toml` (dep list),
`corpus-mcp/src/tools.rs` (import block), `Cargo.lock` (the `corpus-mcp` package
deps), and `ralph/DECISIONS.md` (two appended entries). Options: (a) take the
lane's side, reverting the rename in corpus-mcp; (b) take main's side, dropping
the `corpus-index` dep; (c) combine both — `understanding-vocab` AND
`corpus-index` — and keep both DECISIONS entries in commit order.

**Choice.** (c). The two changes are independent and both correct: the rename is
mechanical and every consumer must repoint or the workspace does not compile; the
read-port repoint is the lane's actual work and dropping it would leave the
`corpus-mcp -> corpus-engine` exception's burn-down step (1) undone. (a)/(b) each
lose a landed decision. The DECISIONS entries are independent appends (the file's
header, line 3), so both are kept in commit order: `dm-corpus-mcp-exception`
(02:40:12) before `dm-understanding-vocab-rename` (02:51:38). The pool's
bookkeeping is completed by hand (row `[x]`, lane worktree removed, branch
deleted): leaving the row `[ ]` would re-run a finished lane on a base that no
longer matches main — the condition that produced this conflict.

**Evidence** (reproduced this session, on `ralph/domains-campaign`).
- `git merge ralph/dm-corpus-mcp-exception` → CONFLICT (content) in
  `Cargo.lock`, `corpus-mcp/Cargo.toml`, `corpus-mcp/src/tools.rs`,
  `ralph/DECISIONS.md`; `corpus-mcp/src/{ask,serve,host}.rs`,
  `quality/ARCH_LAYERS.toml`, `ralph/STATE.md` auto-merge.
- The resolution on the merged tree: `corpus-mcp/Cargo.toml:27` is
  `understanding-vocab` and `:36` `corpus-index`; `corpus-mcp/src/tools.rs`
  imports `understanding_vocab::{atoms::AtomEnvelope, read::read_atlas_atoms}`
  and `corpus_index::{index::CorpusIndex, types::{EmbedFn, ScoredChunk}}`;
  `Cargo.lock:2466` lists `corpus-index`, with no residual
  `corpus-engine-vocab` in the `corpus-mcp` package.
- `git grep -n 'corpus_engine_vocab\|corpus-engine-vocab' -- corpus-mcp/` →
  none after the resolution.
- The lane's own checks re-run on the MERGED tree, not trusted from the lane:
  `./scripts/sovereign-lint.sh --human` → exit 0, scope WORKSPACE, `errors: 0`,
  `cargo exit: 0`; `./scripts/sovereign-test.sh --human --package corpus-mcp` →
  exit 0, pass 42, fail 0; `cargo xtask layer-gate` → exit 0 ("every edge points
  down or sideways, fan-in within caps"); `cargo xtask boundary-gate` → exit 0
  ("corpus-mcp 3/3 crates present", "every declared package reaches only itself
  + the shared leaves").

**Falsified by.** A merged tree that fails to compile or whose corpus-mcp tests
fail (the import paths or the lock entry wrong); or a `ralph/DECISIONS.md`
conflict that is not two appends; or the pool re-running the lane despite the
`[x]`.

**REVIEW-AFTER:** the judgment call is completing the pool's bookkeeping by hand
(marking the row `[x]`, removing the lane worktree, deleting the branch) rather
than leaving the lane to be re-run. The charter covers resolving the halt
("apply the smallest change that makes the campaign flow"); recorded here so the
morning sees the worktree/branch cleanup. `docs-gate` remains RED on the base
tree from `REVIEW-build-index-read-port` (both merged entries' "Not fixed,
observed" notes); it is not this resolution's check.

**Landed in.** `3ef54115c` (the merge — `corpus-mcp/Cargo.toml`,
`corpus-mcp/src/tools.rs`, `Cargo.lock`, `ralph/DECISIONS.md` with both entries
ordered, `ralph/STATE.md` row `[x]`, `ralph/lanes/dm-corpus-mcp-exception.done`),
and this commit (the director entry). `ralph/NEEDS_HUMAN.md` removed (it is in
`.git/info/exclude`, so its removal is not a commit change).
`git revert -m 1 3ef54115c` reverts the merge.

## 2026-09-18 · REVIEW-build-understanding-tier-crates · the registry wins: Understanding is a new `[[package]]`, not `corpus-mcp` growing

**Fork.** The row names two readings of the destination for the Understanding
tiers. DE "The shape" says "This is the `corpus-mcp` package growing, not a
second package"; the registry (quality/DOMAINS.toml) says a NEW `[[package]]
understanding` with `understanding-vocab` as its published leaf. The readings
differ on where `understanding-host`'s `corpus-engine` edge lands: a member's
grandfathered exception inside the package, or outside it (covered by
`corpus-mcp`'s existing exception).

**Choice.** A NEW `[[package]] understanding` with `understanding-atlas` and
`understanding-host` as members and `understanding-vocab` as the published
shared leaf. The row says to resolve with the registry, the campaign's data,
and the campaign's own floor_basis settles the tie explicitly. The package is
declared RED (ARCH §18.1): `understanding-host` is an empty stub, so the one
grandfathered `understanding-host -> corpus-engine` exception is
STALE-by-construction and boundary-gate fails until the host half names the
engine. Declaring it now is the point — a package is declared before the work
that fills it.

**Evidence** (reproduced this session, on `ralph/domains-campaign`).
- quality/DOMAINS.toml:3984 `dest = "understanding (package, SERVING_BOUNDARY
  shape): understanding-atlas ... + understanding-host ... over
  understanding-vocab"`; the cluster note at :3990 "the destination is a
  two-tier PACKAGE, not a crate".
- quality/campaigns/domains.toml:231 floor_basis: "corpus-mcp, which is a HOST
  that reads Understanding's output rather than Understanding's crate".
- Before the unit: `ls sovereign/crates | grep understanding` empty and
  `ls -d understanding-atlas understanding-host` empty. After: two stubs, and
  `boundary-gate` prints `understanding 2/2 crates present` and fails only on
  the stale exception.

**Falsified by.** A boundary-gate that passes the package on the day it is
declared (the red is the point); or DE "The shape" being the operator's
decision rather than a design proposal the registry corrects.

**Landed in.** `d7fabc49b` — the two crate stubs, the root `Cargo.toml`
members, the `knowledge` layer entries, the `[[package]] understanding` row and
its exception, the SYSTEM_OVERVIEW §2 lines, and the DOMAINS.toml context
package + module rows; `ralph/STATE.md` marked in the follow-up commit.

**REVIEW-AFTER:** the row's premise `ls -d understanding-*` was stale (the
dependency rename had already created `understanding-vocab`) and its DOCS check
was red on the base tree from `REVIEW-build-index-read-port`'s incomplete doc
update. Both are recorded in the row's CORRECTED note; the docs repair
(SYSTEM_OVERVIEW citation repoints, the `Cargo.toml` members-comment
de-quoting) is in `d7fabc49b`. `quality/baselines/oversized.txt` still keys
`search.rs` to `corpus-engine/src/index/search.rs` — a move re-key this unit did
not own (§7 keeps baselines out of its reach), left for the wave-close audit.

## 2026-09-18 · REVIEW-build-understanding-pass-port · the host extraction cannot ride this row

**Fork.** The row names one unit: move the port (trait/context/registry) to
`corpus-engine/src/engine/pass.rs` AND move the four `*Pass` impls +
`EnrichmentPassRegistry::builtin()` to `understanding-host`, handing the
registry in at construction. The tree says the second half cannot land in the
same commit.

**Choice.** Land the port move; keep the impls + `builtin()` in
`corpus-engine/src/enrichment/pass.rs` (the module that becomes
`understanding-host`), re-exporting the port at the historical
`crate::enrichment::pass::*` paths; add `EnrichmentPassRegistry::new()` so the
assembly no longer needs the registry's private field. Mint
`REVIEW-build-understanding-pass-host` for the extraction and the injection, and
record the premise fixes in the row.

**Evidence** (reproduced this session, on `ralph/domains-campaign`).
- `git grep -n 'EnrichmentPassRegistry::builtin()' -- '*.rs'` is SEVEN sites,
  not the row's five: `engine/ingest.rs:1749,:1833`, `engine/mod.rs:2864,:3086`,
  `recipe_parsing.rs:268`, `sovereign-tools/src/local_corpus/atlas_dispatch.rs:60`,
  and `recipe.rs:1903` (`Recipe::produces_enriched_atoms`) — a site the row does
  not name.
- `understanding-host/Cargo.toml` has an empty `[dependencies]`; the row's
  "the host already depends on corpus-engine" is false.
- `builtin()` leaving corpus-engine forces `check_enrichment_type` (called from
  `Recipe::from_toml`, recipe.rs:1879) and `produces_enriched_atoms` to take a
  registry: `git grep -c 'from_toml'` counts ~100 call sites and
  `git grep -c 'CorpusEngine::new'` 165. The row's own checks name only
  TEST(corpus-engine) and TEST(understanding-host), which cannot cover that
  cascade.
- After the move, `python3 scripts/domains-census.py crate-lines --crate
  corpus-engine` names 0 corpus-engine files (the new `engine/pass.rs` is covered
  by the `corpus-engine/src/engine/` directory row, re-counted 10006/11 →
  10166/12; the `enrichment/pass.rs` per-file row 646 → 547).

**Falsified by.** A `builtin()` that can stay in corpus-engine while the impls
live in `understanding-host` (it cannot: the engine may not name the host), or a
`CorpusEngine::new`/`Recipe::from_toml` that does not need the registry.

**Landed in.** `037f8285c` (the port move, the re-exports, `new()`, the engine
sites, the DT re-counts, the `SYSTEM_OVERVIEW.md` and `DOMAINS.toml` path
repoints).

## 2026-09-18 · REVIEW-build-understanding-pass-host · the extraction splits into five rows

**Fork.** The row names one MOVE: the four `*Pass` impls, `refuse_deferred`,
the test block and `builtin()` into `understanding-host`, plus the engine
taking the registry at construction and the recipe path taking it too. Its own
text says "SPLIT IT before building". The tree says the split has a forced
order, because `builtin()` cannot leave `corpus-engine` while any
`corpus-engine` site still calls it — and the engine may not name the host.

**Choice.** Split into five rows, ordered so every dependency sits above it:
(1) `dm-pass-registry-field` — `CorpusEngine` gains the registry field, a
`with_enrichment_passes` builder and an `enrichment_passes()` accessor, with a
TEMPORARY default of `builtin()` so the four engine sites and
`atlas_dispatch.rs:60` switch to the field with no behaviour change; (2)
`REVIEW-build-recipe-check-seam` — the `[enrichment] type` gate leaves
`Recipe::from_toml` for the checked boundary (engine load + daemon previews),
and `produces_enriched_atoms` takes the registry; (3)
`REVIEW-build-pass-assembly-injection` — the prod assemblers inject the built-in
registry explicitly, while it still lives in corpus-engine; (4)
`dm-pass-impls-move` — the file moves to `understanding-host`, `builtin()`
becomes a free function, the default flips to `new()`, the injections repoint;
(5) `REVIEW-audit-pass-host` — TESTALL; PREPUSH. The four `*Pass` impl names
appear nowhere outside `enrichment/pass.rs`, so only `builtin()`'s callers move.

**Evidence** (reproduced this session, on `ralph/domains-campaign`).
- `git grep -n 'EnrichmentPassRegistry::builtin()' -- '*.rs'` is TEN sites:
  `engine/ingest.rs:1749,:1833`, `engine/mod.rs:2865,:3087`,
  `enrichment/pass.rs:469,:503,:539` (tests), `recipe.rs:1903`,
  `recipe_parsing.rs:268`, `sovereign-tools/src/local_corpus/atlas_dispatch.rs:60`.
- `git grep -l 'Recipe::from_toml' -- '*.rs'` is 34 files; `git grep -o
  'Recipe::from_toml'` 157 sites. `CorpusEngine::new` is 165 sites, 45 non-test
  files. `understanding-host/Cargo.toml` `[dependencies]` is empty.
- The four impl type names (`FieldModelPass`, `TieredPass`, `AtlasPass`,
  `InvestigationPass`) and `refuse_deferred` appear NOWHERE outside
  `enrichment/pass.rs` except a comment at `engine/mod.rs:3095`.
- The recipe gate is called from `Recipe::from_toml` (`recipe.rs:1879`); the
  daemon's prod `from_toml` sites (`recipe_http.rs:85,:323,:528`,
  `recipe_project_http.rs:616,:648`) all hold an engine, so the checked boundary
  can reach the registry.
- The understanding package's grandfathered exception is `ARCH_LAYERS.toml:1388-1393`
  (`from = "understanding-host"`, `to = "corpus-engine"`), STALE until the host
  names the engine.

**Falsified by.** A `builtin()` that can leave corpus-engine while a
corpus-engine site still calls it (the engine may not name the host), or a
recipe-load gate that reaches the registry without either the engine's field or
a parser parameter — either would collapse the split to fewer rows.

**Landed in.** the mint commit under `REVIEW-build-understanding-pass-host`;
the children carry the work. Parent marked `[x]` in the follow-up
`ralph: REVIEW-build-understanding-pass-host done`.

## 2026-09-18 · REVIEW-mint-understanding-tiers · the pure/host line is measured, not the DE's estimate

**Fork.** The row says the cluster is "170 files ... corpus-engine/src/{enrichment,meta_atlas,atlas_traversal}, 102,595 lines" and cites DE's "106 of 170" pure. The tree says the `understanding` context tags 169 corpus-engine files / 101,324 lines, and three of them (`stream_axes.rs`, `wikipedia_columnar.rs`, `wikipedia_columnar/tests.rs`) sit outside the three directories the row names. Decide what "pure" means operationally, and which count the `[[tier]]` rows carry.

**Choice.**

1. The cluster is the registry's tag, not a directory glob: all 169 `corpus-engine` files `_module_context` returns `understanding` for (`scripts/domains-census.py`; `plan --crate corpus-engine` reads `169` tree files). The three outside the named dirs are classified like the rest.
2. `pure` = names no index (`crate::index`/`CorpusIndex`/`IndexMeta`/`IndexInfo`), no engine (`crate::engine`), no embed fn (`EmbedFn`/`embed(`), no filesystem or ANN I/O (`std::fs`, `File::open`, `OpenOptions`, `read_to_string`/`write_all`/`create_dir`, `.exists()`, `read_dir`, `metadata(`, `from_reader`, `lancedb::`/`arrow::`), AND no `crate::<m>` reach to a corpus-engine module — only the leaf shims `crate::error`/`crate::types` (both re-export `corpus-index` since `503681188`), the external `oplog` crate (`lib.rs:53` `pub use ::oplog`), and vocab's `crate::atlas_canonical` (`lib.rs:23`). Inline `#[cfg(test)]` modules are stripped first. Measured: **pure 96 files / 43,982 lines; host 73 files / 57,342 lines.**
3. Fifteen files the four-capability scan called pure were adjudicated host by hand: eleven name `recipe`/`recipe_ontology`/`chunkers`/`filters`/`WikiAtlasProvider` (`provider.rs`, `vital_tier.rs`, the five `investigation/` files, `ontology/mod.rs`, `ontology/validate.rs`, `configurable_atlas.rs`, `section_join.rs`); four do I/O the four patterns miss (`governance_change.rs`, `meta_atlas/index.rs`, `meta_atlas/bridge/lookup.rs` — `path.exists()`; `wikipedia_columnar.rs` — `lancedb`). DE's 106/108 is a design estimate; the measured split is 96/73.
4. The 12 pure and 3 host `mod.rs` shells are handled by `REVIEW-build-understanding-crate-tree`, not by a batch move. After the shells split, the only `pure`→host type edge is `AtlasOntologyFile` (`writer.rs:374`, named by `pipeline/pipelines/declaration.rs:22,:29`), which that row moves to the language.

**Evidence.** `python3 scripts/domains-census.py plan --crate corpus-engine` reads `101324→102227 lines, 169→170 files`; the `[[tier]]` rows (`quality/DOMAINS.toml`) carry all 169 paths; the 55 `pure`→host `crate::` edges were resolved file-by-file and all but `AtlasOntologyFile` land on `atlas/mod.rs`/`ontology/mod.rs` re-exports of pure or vocab items.

**Falsified by.** A file in the `pure` list whose `git grep -n 'crate::'` names a corpus-engine module after the shells split; or a file in the `host` list that moves to `understanding-atlas` without naming corpus-engine.

**Landed in.** the `REVIEW-mint-understanding-tiers` mint commit; the move rows carry the work.

## 2026-09-18 · REVIEW-build-understanding-crate-tree · the module tree is path-preserving, and the batch's "no corpus-engine reach" premise is false

**Fork.** The row names one landing: split the two MIXED host shells
(`enrichment/atlas/mod.rs`, `enrichment/ontology/mod.rs`), move `AtlasOntologyFile`
to the language, wire both crates, keep `corpus-engine` compiling, and re-key its
DT rows — "so the batch moves below are mechanical". The tree says the shell
split cannot precede the files: a shell's `pub mod <m>;` declarations do not
resolve until `<m>` is in the same crate, and the batch (which moves those
files) DEPENDS on this row. The batch rows' premise — "`git grep -n 'crate::'`
over them resolves only to the tier, the leaf shims, the `oplog` crate or
vocab's `canonical`, and to no corpus-engine module" — is also false.

**Choice.**

1. **The tree PRESERVES the source paths.** A moved pure file lands at
   `understanding-atlas/src/<same path under corpus-engine/src/` and a moved
   host file at `understanding-host/src/<same path>`. An intra-tier
   `crate::enrichment::<m>` / `crate::meta_atlas::<m>` / `crate::atlas_traversal::<m>`
   reach then resolves UNCHANGED; only a host file's reach to a pure sibling
   becomes `understanding_atlas::enrichment::<m>`. The alternative (a flat tree)
   collides on `registry.rs` (atlas, pipeline), `signals.rs` (reconciliation,
   bridge) and `classifier.rs` (atlas_traversal, meta_atlas), and §3a forbids
   renames inside a move.
2. **`understanding-atlas` gets the engine-leaf shims** — `pub use corpus_index::{error,types};`,
   `pub use ::oplog;`, `pub use understanding_vocab::canonical as atlas_canonical;` —
   plus `pub use understanding_vocab::articulation;` for the per-atom half of
   `stream_axes`. These are the only non-tier reaches the production pure code has.
3. **This row landed the landable split, not the shell moves.** `enrichment/ontology/mod.rs`'s
   pure half is real: `clock.rs` + `type_index.rs` moved to
   `understanding-atlas/src/enrichment/ontology/` and the engine's shell
   re-exports them. `enrichment/atlas/mod.rs`'s pure half is the language
   re-export surface, created in `understanding-atlas/src/enrichment/atlas.rs`;
   its pure submodule declarations ride the batch rows (each moves its file and
   adds `pub mod <name>;`). `AtlasOntologyFile` moved to
   `understanding_vocab::ontology` (the ONE host type a pure file names).
4. **The batch rows' rewrite clauses are corrected** (all 17, in place): the
   path-preserving rule replaces the `crate::<m>` rewrite; `crate::stream_axes::<articulation>`
   repoints to `crate::articulation::*`; and the four files with `#[cfg(test)]`
   reaches the purity scan strips (`recipe_templates`, `extractors`, `recipe`,
   `index`) carry a CORRECTED note naming the reach and its fix.
5. **`understanding-host` names `corpus-engine`**, which turns the package's
   grandfathered `[[exception]]` (ARCH_LAYERS.toml:1388-1393) from
   STALE-by-construction to LIVE — `boundary-gate` goes green.

**Rejected.** A `[dev-dependencies] corpus-engine` on `understanding-atlas` to
absorb the test-only reaches: `boundary-gate` counts dev edges ("dep closure
incl. dev+build edges") and failed with `[understanding] understanding-atlas →
corpus-engine: a dev dependency leaves the package closure`. The test reaches
must be repointed to leaf types or relocated to corpus-engine's tests.

**Evidence** (reproduced this session, on `ralph/domains-campaign`).
- LINT `exit=0`, scope WORKSPACE, `errors: 0`, `cargo exit: 0`.
- LAYER `exit=0` — "every edge points down or sideways … fan-in within caps".
- TOML `exit=0`.
- TEST(understanding-atlas) `exit=0`, pass 14 fail 0 (clock 3 + type_index 10 + the scaffolding smoke test 1).
- TEST(understanding-host) `exit=0`, pass 1 fail 0 (the exception-resolution smoke test).
- BOUNDARY `exit=0` — `understanding 2/2 crates present`, "every declared package reaches only itself + the shared leaves" (before the `corpus-engine` dep it read the exception STALE and failed).
- Non-tier reaches measured over the 96 pure files: `crate::enrichment` 256, `crate::error` 26, `crate::oplog` 16, `crate::recipe_templates` 8 (test-only), `crate::stream_axes` 8, `crate::types` 4, `crate::index` 3 (test-only), `crate::atlas_canonical` 2, `crate::extractors` 1 (test-only), `crate::recipe` 1 (test-only), `crate::atlas_traversal`/`crate::meta_atlas` 2.
- The row's dep list omitted `chrono` (`enrichment/ontology/clock.rs:23 use chrono::NaiveDate`); added.

**Falsified by.** A moved pure file whose `crate::` reach does not resolve under
the path-preserving tree; or a `boundary-gate` that fails after the
`corpus-engine` dep (it passed); or `clock`/`type_index` still present under
`corpus-engine/src/enrichment/ontology/` (they are not).

**Landed in.** the commit under `REVIEW-build-understanding-crate-tree`: the two
moved files, the new `understanding-atlas` modules, `AtlasOntologyFile` in
`understanding-vocab`, the two `Cargo.toml`s + root `[workspace.dependencies]`,
`corpus-engine`'s re-export shims and dep, the DT tier/module re-keys, and
`SYSTEM_OVERVIEW.md` §2.

## 2026-09-18 · dm-understanding-pure-1 · the first pure batch carries two forced extractions and one registry re-key

**Fork.** The row names one landing: MOVE the eight `pure`-tier files to
`understanding-atlas`, with the path-preserving tree and the four listed
rewrites. The tree forced three things the row does not name: two host
definitions the moved pure files name, and the registry paths the row's own
premise reads.

**Choice.**

1. **`fold` moves to the pure tier.** `atlas_traversal/classifier.rs` (this
   batch) reaches `crate::enrichment::atlas::fold`, which was defined in the
   HOST `enrichment/atlas/resolution.rs:2272`. A pure file may not name
   corpus-engine, and duplicating the fold would be two deciders for one key
   (ARCH 8), so `fold` + `transliterate_cyrillic` moved to
   `understanding-atlas/src/enrichment/atlas/fold.rs` and `resolution.rs`
   re-exports at the historical path. The pure files that move in later rows
   (`atlas::cross_corpus`, `atlas::resolution_ontology`) keep resolving through
   that re-export today and reach the pure module after they move.
2. **`BridgeRelation` / `BridgeSignal` move to the pure bridge.** The moved
   `meta_atlas/bridge/signals.rs` and `adjudicate.rs` name both enums, defined
   in the HOST `meta_atlas/bridge/edges.rs:30,63` (the persisted edge store,
   which does IO). They moved to `understanding-atlas/src/meta_atlas/bridge.rs`
   and `edges.rs` re-exports at the historical path.
3. **The classifier's recipe-template test reach is replaced by a leaf
   fixture.** The row's CORRECTED note names it: the moved test used
   `crate::recipe_templates::numismatics_policies` (recipe TOML parsing, host),
   and boundary-gate counts dev edges. `atlas_traversal/test_fixtures.rs`
   builds the same `OntologyV1` from `understanding_vocab::ontology::decl` and
   folds it through the language's `into_policies()`.
4. **The DT `[[tier]]` pure paths are re-keyed** (8 paths) from
   `corpus-engine/src/...` to `understanding-atlas/src/...`, matching the
   parent row's `clock`/`type_index` re-key; the wave-close audit
   (`REVIEW-audit-understanding`) requires every `[[tier]]` path to resolve.

**Rejected.** A `pub use corpus_engine::...` shim inside `understanding-atlas`
for `fold`/`BridgeRelation`: boundary-gate refuses the pure→corpus-engine edge,
even dev. A checked-in fixture that re-parses the shipped recipe TOML: it would
re-introduce the recipe parser the pure tier may not name.

**Evidence** (reproduced this session).
- CLEAN `exit=0` (debug target 15G, under 50G).
- LINT `exit=0`, scope WORKSPACE, `errors: 0`, cargo exit 0.
- LAYER `exit=0` — "every edge points down or sideways … fan-in within caps".
- TOML `exit=0`.
- TEST(understanding-atlas) `exit=0`, pass 118 fail 0 (the moved pure tests).
- TEST(corpus-engine) `exit=0`, pass 2080 fail 0 (the shims keep every
  in-engine reach resolving, including the fold and bridge re-exports).

**Falsified by.** A moved pure file whose `crate::` reach does not resolve under
the path-preserving tree; or a `fold`/`BridgeRelation` caller that the re-export
does not satisfy (a corpus-engine test failure); or a DT `[[tier]]` path that no
longer names a file.

**Landed in.** the commit under `dm-understanding-pure-1`: the eight moved
files, the two extractions, the leaf fixture, the corpus-engine shims, the two
`Cargo.toml`s, and the DT `[[tier]]` re-key.

## 2026-09-18 · dm-auto-recover-move · the AppState parameter becomes a struct of five reads, not two

**Fork.** The row says `auto_recover.rs` moves to `sovereign-grants` and its
one `&AppState` parameter "becomes the two things it reads — `corpus_engine`
and `active_ingests` — supplied by the caller", repointing
`routes_internal/corpus_collaborate.rs`. The tree says the function reads five
things and that corpus_collaborate does not call it.

**Choice.**

1. **The five reads become `FoldRecovery`.** `merge_from_fold_coverage` reads
   `state.inner.node.corpus_engine` (:242), `state.identity_reader().current()`
   (:252), `crate::routes_internal::peer_control_urls(state, …)` (:253),
   `state.inner.fabric.mesh_store` (:258) and
   `state.inner.fabric.contribution_emitter` (:260). `sovereign-grants` cannot
   name `AppState`, so the parameter becomes a public `FoldRecovery` struct
   carrying exactly those five; the function body is otherwise unchanged.
   `active_ingests` is NOT one of them — the caller (`auto_ingest.rs:216-217`)
   reads it and gates before the call, and the function never sees it.
2. **A `fold_recovery(state)` adapter gathers them in `sovereign-api`.** It
   lives in `routes_internal/corpus_queue.rs` beside `peer_control_urls`, which
   it calls; it is the one place the AppState→`FoldRecovery` mapping is spelled
   (ARCH 8). The four real callers use it: `sovereign-daemon/src/auto_ingest.rs`
   and the three `sovereign-mesh/tests/main/fold_ingest_*.rs` files.
3. **`corpus_collaborate.rs` is repointed, not re-plumbed.** It calls only
   `try_recover_stranded_partitions` (no `AppState`), so its `crate::auto_recover::`
   paths become `sovereign_grants::auto_recover::`; its behaviour is unchanged.
4. **`dirs = "5"` joins sovereign-grants.** The moved file's `dirs::home_dir()`
   (the alignment projector's self-heal hook, :587) is the one external crate
   the row's import-list premise missed; copied from sovereign-api's manifest
   per §3a step 3. Its `#[allow(clippy::disallowed_methods)]` rode along.

**Rejected.** Passing the five positionally: `too_many_arguments` is allowed,
but `MergePlan` is the crate's own precedent for bundling multi-input merge
calls as data. Splitting the file (pure half to grants, `AppState` half staying
in sovereign-api): the row says MOVE the file, and DT's cluster note sends
`auto_recover.rs` whole to `sovereign-grants`. A trait port on `AppState`:
`ralph/DECISIONS.md` 2026-09-16 already defers that to
`REVIEW-build-daemon-parts`, which cannot run before this row.

**Evidence** (reproduced this session).
- CLEAN `exit=0` (debug target 0G, under 50G).
- LINT `exit=0`, scope WORKSPACE, `errors: 0`, cargo exit 0.
- LAYER `exit=0` — "every edge points down or sideways … fan-in within caps".
- TEST(sovereign-grants) `exit=0`, pass 59 fail 0 (the moved file's own tests
  now run in their new home).
- TEST(sovereign-mesh) three filters `exit=0`, pass 1 fail 0 each:
  `two_donors_on_two_nodes_land_both_slices_in_the_canonical`,
  `a_two_donor_fold_missing_its_peer_refuses_and_writes_no_canonical`,
  `the_merge_proceeds_with_the_slices_that_exist` — the `merge_from_fold_coverage`
  callers that moved signature.

**Falsified by.** A `FoldRecovery` field that does not reproduce the read the
function made (a behaviour change in the e2e merge); or a caller the
`fold_recovery` adapter does not satisfy (a LINT failure); or a `dirs` use the
grants manifest does not link.

**Landed in.** the commit under `dm-auto-recover-move`: the `git mv`, the
`FoldRecovery` struct, the `fold_recovery` adapter and its re-export, the shim
at the old path, the four caller repoints, the DT module re-key and the two
`quality/baselines/` path re-keys.

## 2026-09-18 · dm-daemon-api-edge · the move lands; three premises the row did not carry

**Fork.** The row executed (all deps `[x]`), and the tree falsified three of its
unstated assumptions. Correct the row and land, or stop?

**Choice.** Correct and land. The three:

1. **`principal.rs` collides.** `sovereign-api/src/principal.rs` (the HTTP edge
   resolver, 502 lines, `impl AppState { resolve }`) and
   `sovereign-daemon/src/principal.rs` (the corpus-ceiling `LocalOwnerPrincipal`
   from `dm-daemon-cli-composition`) share a module name. An inherent impl cannot
   leave the crate defining the type, so the resolver had to move; it landed as
   `client_principal.rs`. The design's one resolver (`REVIEW-mint-principal`)
   collapses the two.
2. **mesh's own `src/` unit tests cannot name the daemon.** `ring_sync/tests.rs`,
   `ring_sync/snapshot_tests.rs`, `ring_sync/projection_tests.rs` and
   `rail_kv_pump/tests.rs` assemble `AppState`. A `#[cfg(test)]` module inside
   `sovereign-mesh` that names `sovereign-daemon` puts TWO builds of
   `sovereign-mesh` in the graph (the dev-dependency cycle:
   `sovereign-mesh` dev-depends on `sovereign-daemon`, which depends on
   `sovereign-mesh`), and the compiler refuses to unify the two `FabricPart`
   types ("multiple different versions of crate `sovereign_mesh`"). They moved
   to `sovereign-mesh/tests/main/` as integration tests, where Cargo unifies the
   two paths to one build. Four supporting items became `pub` for them
   (`ring_sync::exchange`, `ExchangeStop`, `ExchangeOutcome` and its fields,
   `MAX_CHUNKS_PER_EXCHANGE`; `rail_kv_pump::WORK_NAMESPACE`,
   `MEASUREMENTS_NAMESPACE`).
3. **`corpus-engine-scip` fan-in.** `sovereign-daemon` gained the dep when the
   workbench shell moved in, growing the god-crate's fan-in 11 -> 12. The row
   does not name `--update-baseline` (forbidden, PROMPT §7), so the unused dep
   was dropped from `sovereign-api`'s manifest instead — the modules that read
   it live in `code-next-edit` and the shell in the daemon. Fan-in is flat at 11.

Also carried: the `sovereign-api` crate is now shim-only (its host deps stay
declared so the `[[forbid]]` exceptions are not STALE; `REVIEW-build-sovereign-api-retire`
deletes it), `state/fabric.rs` moved to `sovereign-mesh/src/fabric.rs` with
`MeshMutationHook` and the 20 accessors the loops call, and the four
`quality/baselines/` rows naming moved paths were re-keyed in the same commit
(§3a step 6).

**Evidence.** `./scripts/sovereign-lint.sh --human` exit=0 (workspace,
`--all-targets`, 0 errors); `cargo xtask layer-gate` exit=0; `cargo xtask
boundary-gate` exit=0; `cargo xtask docs-gate` exit=0; `python3
scripts/domains-census.py --self-test` exit=0. The duplicate-crate error is
`error[E0308]: mismatched types ... note: there are multiple different versions
of crate sovereign_mesh in the dependency graph`.

**Falsified by.** A showing that `sovereign-mesh`'s `src/` unit tests can name
`sovereign-daemon` without a duplicate build (then the move to `tests/main/` was
unnecessary); or that the `corpus-engine-scip` edge belongs on `sovereign-api`
(no module there reads it).

**Landed in.** `0a2891ccf` (the move) and `8a2de5e6a` (rustfmt). `git revert
0a2891ccf` reverts the move alone.

## 2026-09-18 · REVIEW-build-daemon-parts · the row cannot move all twenty fields; the serving package may not name commonwealth-state

**Fork.** The row executes (all deps `[x]`) and says `state/serving.rs` (20
fields) -> `sovereign-serving-host`, `state/answering.rs` -> `sovereign-core`,
`state/workbench.rs` stays, `state/node.rs`/`state/ingest.rs` stay. Measure the
destinations before editing: is each move legal?

**Choice.** Split the row and land the legal half.

1. **Serving: 17 of 20 move; 3 stay.** The part holds
   `inference_store: InferenceStateStore` and
   `peer_preferences: PeerPreferenceStore`, both defined in
   `commonwealth-state`, plus `rpc_shard_warmer: Option<Arc<dyn RpcShardWarmer>>`
   whose trait method takes `AppState`. `commonwealth-state` is a member of the
   `commonwealth` package (`quality/ARCH_LAYERS.toml:1090-1094`), not a shared
   leaf of `serving` (the package text names `oicp-types`, `kernel-types`,
   `sovereign-contracts`, and the one grandfathered `commonwealth-core`
   exception at `:1378-1383`). A `sovereign-serving-host -> commonwealth-state`
   dep is a second `[[exception]]`; §7 makes adding one operator-only, and the
   campaign's own kill clause says split, never widen
   (`quality/campaigns/domains.toml:288`). The daemon holds the three in a new
   `state/store.rs::StorePart`; both stores are `MeshStore`-backed, so their
   home is the node the daemon assembles. The row's answering clause is false
   for the same class of reason (below).
2. **Answering does not move.** `AnsweringPart.middleware_registry` is
   `crate::middleware::MiddlewareRegistry`, which DC §4.2:351 calls host
   composition, and `session_store` is `sovereign_atos::session::SessionStore`;
   `sovereign-atos` depends on `sovereign-core` (`Cargo.toml:22`), so the
   reverse edge is a Cargo cycle. The part stays scaffolding in the daemon and
   a follow-up row is minted for the three-way split the design actually needs.
3. **The "never a flat field" clause is DC §4.2's end state, not this row.**
   The parts remain the daemon's assembly bundle on `AppStateInner`; moving
   every handler to take a part is `quality/DAEMON_CORE.md:412-416`'s
   "Handlers take a part, never the node", a workspace-wide surface change the
   row's ten-file grammar does not carry.

**Evidence** (reproduced this session).
- `quality/ARCH_LAYERS.toml:1090-1094` (`commonwealth` crates include
  `commonwealth-state`); `:1161-1171` (the `serving` package's leaves and the
  one exception); `:1378-1383` (the `commonwealth-core` exception).
- `grep -n "pub struct InferenceStateStore\|pub struct PeerPreferenceStore"` ->
  `commonwealth-state/src/store_adapter.rs:51`,
  `commonwealth-state/src/peer_preferences.rs:105`; both hold `MeshStore`.
- `grep -n "sovereign-core" sovereign/crates/sovereign-atos/Cargo.toml` ->
  `:22`; `sovereign-core` does not depend on `sovereign-atos`.
- `grep -n "middleware_registry" sovereign/crates/sovereign-daemon/src/state.rs`
  -> `crate::middleware::MiddlewareRegistry` (host).
- CLEAN exit=0; LINT exit=0 (WORKSPACE, `--all-targets`, 0 errors); LAYER
  exit=0 ("fan-in within caps"); CENSUS `--self-test` exit=0.

**Falsified by.** A showing that `commonwealth-state` is nameable from the
`serving` package (then all twenty fields move in one row); or that
`sovereign-core` can name `sovereign_atos::session::SessionStore` without the
cycle (then answering moves); or an operator widening the serving `except`,
which would make the store fields legal and this split unnecessary.

**Landed in.** `6942d96e1` (the move) and `d52b7ab39` (rustfmt). The row is
marked `[x]` with the correction; `REVIEW-build-daemon-answering-part` is minted
under it. `git revert 6942d96e1` reverts the move alone.

## 2026-09-18 · REVIEW-build-daemon-embedded-split · the row is two units after the whole-cluster move; the reach reads land, the lifecycle is minted

**Fork.** The row executed (deps `[x]`) and its own text is stale. It says
"SPLIT `sovereign-mesh/src/daemon.rs` (5,619 lines)", "`daemon_services.rs` and
mesh's `lib.rs` re-exports split the same way", and "the external consumers
(cli-daemon 35, cli-llm 24, cli-dev 3 sites) repoint in the same commit" — but
`dm-daemon-mesh-edge` already moved the whole cluster, so `daemon.rs` is now
`sovereign-daemon/src/daemon.rs` (5,648), `daemon_services.rs` and the four impl
files are daemon-side, mesh's `lib.rs` names no daemon module, and every external
consumer already points at `sovereign_daemon::…`. What remains is the half DC
§4.1 says moves back: Fabric's membership operations. Build it whole, split it,
or stop?

**Choice.** Split and land the tractable half, per the `REVIEW-build-daemon-parts`
precedent (`6942d96e1`). The row is two units:

1. **The reach reads move now.** `eligible_anchors` (`daemon.rs:2439`),
   `origin_offers`/`origin_reach` (`media_reach.rs:60,87`) and `origin_fanout`
   (`origin_fanout.rs:59`) were the daemon reading Fabric's roster and
   projecting it through `commonwealth-media`. They are now `FabricPart`
   methods in `sovereign-mesh/src/fabric.rs`; the daemon keeps the "is there a
   node at all" gate and the iroh path snapshot (`peer_paths`), passed in
   because the endpoint is the daemon's. Landed at `306005a82`.
2. **The lifecycle is a redesign, not a move.** `create_mesh`/`join_mesh`/
   `leave`/`switch_mesh`/`forget_mesh`/`rotate_invite`/`try_resume` and
   `forget_member` orchestrate `DaemonState` (the listeners, the routers, the
   on-disk `mesh.json`/`join_key.secret`), so "Fabric's methods" requires
   Fabric to own `join_key_plaintext`, the persisted-mesh pointer and the
   roster mutations, with the daemon observing through readers (DC §4.1,
   ARCH 12). Minted as `REVIEW-build-daemon-membership-lifecycle`.

**Evidence** (reproduced this session).
- `wc -l sovereign/crates/sovereign-daemon/src/daemon.rs` -> 5,648;
  `find sovereign/crates -name daemon.rs` -> only the daemon's.
- `git grep -l 'EmbeddedDaemon'` -> the external consumers are
  `sovereign-cli-daemon`/`sovereign-cli-llm`/`sovereign-cli-dev` naming
  `sovereign_daemon::…`, plus mesh's `tests/main/` integration tests.
- CLEAN exit=0 (debug target 24G, under 50G).
- LINT exit=0, scope `sovereign-daemon,sovereign-mesh,sovereign-cli-daemon,sovereign-cli-dev,sovereign-cli-llm`, errors 0.
- LAYER exit=0 — "every edge points down or sideways … fan-in within caps".

**Falsified by.** A showing that `FabricPart` cannot name `commonwealth-media`
(it already deps it, `sovereign-mesh/Cargo.toml:62`, and `iroh_access.rs` uses
it); or that the lifecycle methods do not touch `DaemonState` (then they would
have been a pure move and the split into two units unnecessary).

**Landed in.** `306005a82` (the reach reads) and this commit — `ralph/STATE.md`
(the row marked `[x]` with the correction, the minted row, `DEMO-d5-misnamed`
re-pointed to it) and this entry. `git revert 306005a82` reverts the reads alone.

## 2026-09-18 · REVIEW-build-daemon-membership-lifecycle · the stopped-state half needs Fabric to exist before `AppState`

**Fork.** The row names `create_mesh`/`join_mesh`/`leave`/`switch_mesh`/`forget_mesh`/
`rotate_invite`/`try_resume`/`forget_member` and says "the daemon half keeps the
listeners and the Fabric half keeps the roster/identity". Execute it whole, or split
the tractable running-only half and mint the prerequisite?

**Choice.** Land the half Fabric can own today, then stop at the construction-order
gap. LANDED: `fabric::JoinKeyReader` (Fabric owns the cached join key, created first
and shared into `FabricPart` through `FabricSeed`, DC §4.2); `FabricPart::adopt`
(roster + identity in one step); `FabricPart::forget_member` with `ForgottenMember`
and `ForgetMemberError` moved to `sovereign-mesh`, `MeshError` mapped at the daemon
boundary. The row stays `[~]`: the stopped-state operations are not buildable yet.

**The prerequisite the row does not name.** `known_meshes` (`daemon.rs:1025`),
`forget_mesh` (`:1084`), `switch_mesh` (`:1042`) and `try_resume` (`:958`) are `pub`
methods that answer **while the daemon is `Stopped`** — they read only `data_dir` and
the persisted pointer. `join_key_plaintext` (`:257`) is cleared in `stop_inner`
(`:1881`) *after* `std::mem::replace(&mut *state, DaemonState::Stopped)` drops the
running `AppState` (`:1807`). `FabricPart` is reachable only as
`AppStateInner.fabric` (`state.rs:291`), so it does not exist while stopped.
Therefore Fabric must become a standalone object the daemon holds across stop —
DC §4.2's staging order already puts Fabric before the engine and Serving — and that
construction reorder is its own unit. Making `forget_member` a Fabric method needed
none of this because it runs only while Running (`app_state()` returns `None`
otherwise).

**Evidence** (reproduced this session).
- `grep -n 'join_key_plaintext' sovereign/crates/sovereign-daemon/src/daemon.rs`
  -> the field at `:257`, cleared at `:1886` inside `stop_inner`'s `Leave|Park` arm,
  after the state replace at `:1807`.
- `grep -n 'FabricPart' sovereign/crates/sovereign-daemon/src/state.rs` ->
  `pub fabric: std::sync::Arc<fabric::FabricPart>` at `:291`; the only constructor is
  `AppStateInner` at `:1017`, reached from `start_daemon`.
- CLEAN exit=0 (debug target 25G, under 50G).
- LINT exit=0, scope `sovereign-daemon,sovereign-mesh,sovereign-cli-daemon,sovereign-cli-dev,sovereign-cli-llm`, errors 0.
- LAYER exit=0 — "every edge points down or sideways … fan-in within caps".
- TEST(sovereign-mesh) exit=0 (613 pass); TEST(sovereign-daemon) exit=0 (706 pass).

**Falsified by.** A showing that the stopped-state methods can be Fabric methods with
`FabricPart` still built inside `AppState` — i.e. a Fabric handle the daemon can hold
before `start_daemon` and after `stop_inner` without moving `FabricPart`'s
construction out of `state.rs`.

**Landed in.** This commit. The row stays `[~]` with the PROGRESS note; the
standalone-Fabric prerequisite is the next unit under it.

## 2026-09-18 · REVIEW-build-daemon-membership-lifecycle · attempt 2 finalizes the row as its landed half; the standalone-Fabric prerequisite is minted

**Fork.** Attempt 1 committed the running-only half at `8a8936b87` and left the
row `[~]` with a PROGRESS note. Attempt 2 is told to finish the row, run its
checks, and mark it `[x]` — but the row's own VERB is not complete: the
stopped-state operations still cannot be Fabric's methods while `FabricPart`
lives only inside `AppStateInner`. Complete the construction reorder here, or
finalize the row for its landed half and mint the remainder?

**Choice.** Finalize for the landed half and mint the remainder, per the
`REVIEW-build-daemon-embedded-split` precedent (`306005a82`, which marked that
row `[x]` for its landed half and minted this row). Re-deriving the
construction reorder is explicitly out of scope for this attempt ("do not
re-derive the analysis"); the prerequisite is a distinct unit whose evidence
attempt 1 already recorded. Minted `REVIEW-build-daemon-fabric-standalone`
(depends on this row) and re-pointed `DEMO-d5-misnamed` to it, so the demo
cannot run before the lifecycle actually moves.

**Evidence** (reproduced this session, tree unchanged since `8a8936b87`).
- CLEAN exit=0 (debug target 38G, under 50G).
- LINT exit=0 (WORKSPACE, `--all-targets`, errors 0, warnings 2405).
- LAYER exit=0 — "every crate assigned, every edge points down or sideways …
  fan-in within caps".
- TEST(sovereign-mesh) exit=0 (613 pass); TEST(sovereign-daemon) exit=0 (706 pass).

**Falsified by.** A showing that `FabricPart` can be constructed before
`AppState` without moving its construction out of `state.rs` — i.e. a Fabric
handle the daemon can hold across `stop_inner` while `AppStateInner` still owns
the only instance; or an operator ruling that the stopped-state operations stay
on the daemon and the row's VERB is satisfied by the running-only half.

**Landed in.** `8a8936b87` (the running-only half) and this commit
(`ralph/STATE.md`: the row marked `[x]` with the CORRECTED note, the minted
prerequisite, `DEMO-d5-misnamed` re-pointed; this entry). `git revert 8a8936b87`
reverts the half alone.

## 2026-09-18 · REVIEW-build-daemon-fabric-standalone · the standalone construction lands; the method-move half is re-scoped out as daemon assembly

**Fork.** The row's first clause is buildable and is the enabling step: construct
`FabricPart` before `AppState` and hold it across `stop_inner`. Its second clause
("then move `known_meshes`/`forget_mesh`/`switch_mesh`/`try_resume`/`resume_active`
and the running-side `create_mesh`/`join_mesh`/`leave`/`rotate_invite`/
`current_invite` onto Fabric") is not. Do both anyway, or land the construction and
correct the row?

**Choice.** Land the construction and correct the row. The second clause's methods
are the daemon's assembly orchestration, which DC §4.1 reserves for the daemon
("the daemon's assembly STAYS: `DaemonState`, the listeners,
`start_daemon`/`stop_inner`/`shutdown`"), and Fabric lives in `sovereign-mesh`,
which may not name the daemon (`[[forbid]] sovereign-mesh -> sovereign-daemon`,
`quality/ARCH_LAYERS.toml:749-752`) — so it cannot call `start_daemon`/`stop_inner`
at all. The pure membership state those methods would carry (join key, roster,
identity, `adopt`, `forget_member`) already moved at `8a8936b87`. Re-scoping is a
§6 correction of scope, not a weakened bar: the row's checks (LINT/LAYER/
TEST(sovereign-mesh)/TEST(sovereign-daemon)) are unchanged and green.

**Evidence** (reproduced this session).
- `grep -n` on `daemon.rs`: `try_resume` :963 -> `resume_active` :985 ->
  `self.start_daemon(mesh, self_node_id)` :999; `switch_mesh` :1047 ->
  `self.stop_inner(StopMode::Park)` :1069 + `resume_active` :1075;
  `create_mesh_with` :1219 -> `start_daemon` :1280; `join_mesh` :1410 ->
  `start_daemon`; `leave` :1769 -> `stop_inner(StopMode::Leave)` :1776;
  `current_invite` :1993 reads `DaemonState::Running`/`iroh_access`; `rotate_invite`
  :2134 requires `app_state` (running) and drives a gossip round.
- `sovereign-mesh/src/lib.rs:30` `pub mod fabric;` and `daemon.rs:193`
  `use sovereign_mesh::persist;` — Fabric can reach `persist`, but not
  `EmbeddedDaemon`/`DaemonState`/`start_daemon`.
- CLEAN exit=0 (debug target 39G, under 50G).
- LINT exit=0, scope `sovereign-daemon,sovereign-mesh,sovereign-cli-daemon,sovereign-cli-dev,sovereign-cli-llm`, errors 0.
- LAYER exit=0 — "every crate assigned, every edge points down or sideways … fan-in within caps".
- TEST(sovereign-mesh) exit=0 (613 pass); TEST(sovereign-daemon) exit=0 (706 pass).

**Falsified by.** A showing that the lifecycle methods can be Fabric's methods —
i.e. that Fabric (or a port Fabric declares and the daemon implements) can drive
`start_daemon`/`stop_inner` without `sovereign-mesh` naming the daemon; or an
operator ruling that the stopped-state operations stay on the daemon and this
row's VERB is satisfied by the standalone construction alone.

**Landed in.** `c1cd6e217` (`FabricPart::new` in `fabric.rs`;
`AppState::new_with_fabric_and_serving_and_node` in `state.rs`; the
`EmbeddedDaemon.fabric` field in `daemon.rs`, set before `AppState`, kept across
`stop_inner`, cleared on Leave, exposed by `EmbeddedDaemon::fabric()`) and this
commit (`ralph/STATE.md`: the row marked `[x]` with the CORRECTED note; this
entry). Behaviour-preserving.

## 2026-09-18 · REVIEW-build-daemon-answering-part · the three-way split dissolves the bundle rather than moving it

**Fork.** The parent row (`REVIEW-build-daemon-parts`) found `state/answering.rs`
is not one move: `middleware_registry` is host composition (DC §4.2:351),
`session_store` is `sovereign_atos::session::SessionStore` and `sovereign-atos`
depends on `sovereign-core` (`Cargo.toml:22`), so the reverse edge is a Cargo
cycle, and `repo_root` is Answering's fact with no home in the daemon. The row
asks to "resolve the three-way split … then the `AnsweringPart` struct
dissolves", offering three shapes: a daemon construction argument/reader for the
registry, the ATOS registration entry point (or a port) for the store, the
pipeline's reader for the repo root.

**Choice.** Dissolve the bundle into three `AppStateInner` fields, each supplied
by its owner, with no new part and no new `[[exception]]`:

1. `middleware_registry: Arc<crate::middleware::MiddlewareRegistry>` — the
   daemon's own composition root; the daemon constructs it (as it already did)
   and the route reads it directly. The one field whose owner is the daemon.
2. `session_store: Option<sovereign_atos::session::SessionStore>` — built by
   `sovereign_atos::middleware::session_store(mesh, origin)`, a new ATOS-owned
   entry point beside `registrations()`, so the daemon never constructs an ATOS
   type.
3. `repo_root: Option<PathBuf>` — taken from
   `sovereign_core::answering::repo_root()`, the Answering context's home, so the
   fact lives with its owner and the daemon holds the resolved value.

No port was needed: the store's constructor takes only `MeshStore` and `NodeId`,
both of which ATOS already names. The parent's "never a flat field" clause is
DC §4.2's end state ("Handlers take a part, never the node",
DAEMON_CORE.md:412-416), already recorded as out of scope for this grammar.

**Evidence** (reproduced this session).
- `grep -rn 'AnsweringPart\|inner\.answering' sovereign/crates` -> only
  `state.rs`'s definition and construction; the three reads are
  `routes_inference.rs:1530,1555,1577`, all inside
  `#[cfg(feature = "atos")] run_atos_pipeline`.
- `wc -l sovereign/crates/sovereign-daemon/src/state/answering.rs` -> 28.
- `grep -n sovereign-core sovereign/crates/sovereign-atos/Cargo.toml` -> :22;
  `sovereign-core` does not depend on `sovereign-atos`.
- CLEAN exit=0 (debug target 39G, under 50G); LINT exit=0 (18 crates,
  `--all-targets`, errors 0); LAYER exit=0 ("fan-in within caps").

**Falsified by.** A showing that the middleware registry belongs to a context
other than the daemon; or that `sovereign-core` can name
`sovereign_atos::session::SessionStore` without the cycle (then the store travels
to core); or an operator ruling that the three fields stay bundled as a part.

**Landed in.** this unit's code commit (`state.rs`, `routes_inference.rs`,
`sovereign-atos/src/middleware/mod.rs`, `sovereign-core/src/answering/mod.rs`,
`quality/DOMAINS.toml`, `quality/DAEMON_CORE.md`; `state/answering.rs` deleted)
and the `ralph:` commit that marks the row `[x]`. Behaviour-preserving.

## 2026-09-18 · REVIEW-build-sovereign-api-retire · the row's coordinates were stale and the crate was already shim-only; the dissolved crate's plan rows are dead data and go with it

**Fork.** The row's premise (measured 2026-09-16) assumed the crate still held
its clusters and named coordinates: root Cargo.toml :171/:327, ARCH_LAYERS
:1298-1314, consumers `sovereign-mesh :63` and `sovereign-mesh-test-harness :13`,
162 `sovereign_api::` doc refs, and a `quality/conformance/sovereign-api.toml`.
Measure before editing: none hold.

**Choice.** Correct the row and land the retire.

1. The crate is shim-only: `src/lib.rs` is 41 lines of re-exports and `src/`
   holds nothing else. `git grep 'sovereign_api::' -- '*.rs' | grep -v
   'sovereign-api/'` is 0 (was 162). The live consumers are
   `sovereign-daemon/Cargo.toml:29` (unused — no `sovereign_api::` site) and
   `sovereign-mesh/Cargo.toml:63`; the harness edge was already repointed at
   `dm-daemon-api-edge`. `quality/conformance/sovereign-api.toml` was renamed to
   `sovereign-daemon.toml` when the host cluster moved.
2. Coordinates moved: members :195, workspace-dep :355; the three
   `[[exception]]` rows at :1415-1431. The `sovereign-scheduler -> sovereign-api`
   forbid (rule 4) and the `sovereign-api -> sovereign-*` forbid are dead once
   the crate is gone, and are removed with the `mesh-api` layer entry.
3. The `atos` feature chain: `sovereign-mesh`'s `atos = ["sovereign-api/atos"]`
   was the only forwarding left, and `sovereign-daemon`'s `atos` feature carried
   `sovereign-mesh/atos`; both removed. The pipeline lives in the daemon's own
   `dep:sovereign-atos` / `dep:corpus-engine-atos`.
4. Dead DOMAINS registry rows removed: the `[[module]]` row for the deleted
   `lib.rs`, the nine `[[cluster]]` rows and the `[plan."sovereign-api"]`
   order/exceptions tables. `plan --crate sovereign-api` now reads "no
   [[cluster]] rows". The frozen `[[noun]]` / `[[cluster.own_deps]]` /
   `external_consumers` strings are historical measurements and stay.
5. `crate-lines --crate sovereign-api` cannot read 0: the command's first act is
   a repo-wide coverage assertion (`coverage_holes`), which fails on crates other
   rows created without a module row (`sovereign-peer-wire`, `corpus-index`,
   `understanding-atlas`). The crate itself has zero module rows; `plan` is the
   operative proof. Reported, not defaulted (ARCH 6).
6. The tracing filters `sovereign_api=info` in `sovereign-cli-daemon`,
   `sovereign-cli-llm` and the desktop were repointed at
   `sovereign_daemon=info` — the moved modules' target — so the daemon's logs do
   not go dark.
7. `scripts/daemon-route-census.py` HOSTS repointed `sovereign-api/src` ->
   `sovereign-daemon/src` (the script errored on the missing dir; now reads 302
   registrations / 284 unique paths).

**Evidence.** CLEAN exit=0 (debug target 96G, cleaned 98.8GiB, then warm under
50G); LINT exit=0 (workspace, `--all-targets`, errors 0); LAYER exit=0 (the
three exceptions retired without a STALE verdict); TOML exit=0; CENSUS exit=0
(11/11 axes); `plan --crate sovereign-api` -> no rows;
`daemon-route-census.py` -> 302 registrations / 284 unique paths.

**Falsified by.** A consumer that still names `sovereign_api::` (a repoint was
missed); a gate that reads the frozen `[[noun]]` / `own_deps` strings as live;
or a showing that `crate-lines` should skip the coverage assertion for a deleted
crate.

**Landed in.** this unit's code commit and the `ralph:` commit marking the row
`[x]`.

## 2026-09-18 · DEMO-d5-misnamed · the demo cannot pass: its expected verdict is false and one dependency was only partially landed

**Fork.** The row runs `domains-census.py misnamed` expecting sovereign-mesh at
100% fabric. The tree disagrees in two independent ways: the instrument's
coverage assertion exits 4 before any crate table, and — bypassing it —
sovereign-mesh reads 83.1%. Options: (a) mark the row `[x]` and paste the
failure; (b) correct the row's premise and dependencies, record the
operator-only blocker, and escalate; (c) attempt the deferred move.

**Choice.** (b). §2/§6 make a failed DEMO a §6 stop, and the move is blocked by
an operator-only gate decision (§7 forbids re-baselining a ratchet or widening an
`except`), so (c) is out of a worker's hands. The row keeps its expected verdict
— a pass bar is not weakened — and gains two dependencies that name the missing
work.

**Evidence** (reproduced this session; paths under the worktree root).
- `python3 scripts/domains-census.py misnamed` → exit 4, coverage hole: 33
  untagged files — corpus-index (19), understanding-atlas (13),
  `sovereign/crates/sovereign-peer-wire/src/lib.rs` (1); none in sovereign-mesh.
- `misnamed()` called directly (coverage bypassed): sovereign-mesh
  `13860 / 16684` (83.1%, MISNAMED); its only non-fabric `[[module]]` rows are
  `workbench 674 sovereign-mesh/src/projects.rs` and
  `workbench 2150 sovereign-mesh/src/reindexer.rs`.
- Those two files are exactly the ones `dm-mesh-workbench-move-watchers`
  (STATE.md, `[x]`) deferred on 2026-09-17; the blockers reproduce:
  `reindexer.rs:650,708` reach `corpus_engine::facts` / `facts_store` in
  production (so `corpus-engine-watchers -> corpus-engine` is code-intel
  package-illegal, `docs/CODE_TOOLING_BOUNDARY.md:427`), and `projects.rs:364`
  reaches `sovereign_contracts::rebrand`.
- `quality/baselines/fan_in.tsv`: corpus-engine 20 (`:8`), sovereign-contracts 33
  (`:11`) — the contracts cap moved 31→32→33 since the 2026-09-17 note
  (`dm-decision-extractor-move`, `dm-next-edit-move`).

**Correction.** DEMO-d5-misnamed's `depends` gains `dm-registry-coverage`
(restores the instrument's coverage) and `REVIEW-build-mesh-workbench-deferred`
(moves projects.rs + reindexer.rs), the latter depending on the new
`HUMAN-mesh-workbench-gates`. The decision package is `ralph/NEEDS_HUMAN.md`. The
row stays `[ ]`; no `.done` was written.

**Falsified by.** A showing that `corpus_engine::facts` is reachable from a
package crate today (it is not), or that the fan-in caps already admit the two
moves, or a tag change that makes sovereign-mesh read own == total without a
`git mv` (the goodhart smell the bar names).

**Landed in.** this commit (the row correction, this entry, `ralph/NEEDS_HUMAN.md`).

## 2026-09-18 · DEMO-d5-misnamed · director: the workbench destination is already decided and needs no exception; mint the `code-facts` prerequisite and drop the operator row

**Fork.** `DEMO-d5-misnamed` (the `dm-mesh-closed` rung's D5) expects
sovereign-mesh at 100% fabric; its last two non-fabric modules are
`projects.rs` (674) and `reindexer.rs` (2,155), the two files
`dm-mesh-workbench-move-watchers` deferred. The package asks the operator to
choose between (a) widening the code-intel `[[exception]]` plus the fan-in caps,
(b) waiting for the `code-facts` carve-out, or (c) re-homing one/both. Option
(a) is operator-only (`ralph/CHARTER.md:34`; PROMPT §7), and the package framed
the whole fork as such.

**Choice.** Decide it: option (b), the doc-named path. The premise that this
fork needs the operator is FALSE. `domains-11-workbench-next-edit`'s "Done
when" already fixes the gate — "`boundary-gate` green for `code-intel` **with no
new exception**" (`.sovereign/features/domains-11-workbench-next-edit/order.md:32`)
— and `CODE_TOOLING_BOUNDARY.md` §2 names the crate that makes the reach legal:
`code-facts` from `corpus-engine/src/{facts,facts_check,facts_store}.rs`
(`:404`, table `:67`). The destination is likewise decided: the code-intel
package (`quality/DOMAINS.md:160`) and, for this cluster, `corpus-engine-watchers`
(`quality/DOMAINS.toml:5581`). So:

1. Remove `HUMAN-mesh-workbench-gates`; mint `REVIEW-build-code-facts` (CREATE
   the crate per §2 Phase 2, move the three files, repoint the four consumers).
   It is the prerequisite that turns `corpus_engine::facts` into a package edge.
2. `REVIEW-build-mesh-workbench-deferred` (the MOVE of `projects.rs` +
   `reindexer.rs` to `corpus-engine-watchers`) now depends on it, and carries
   the two fan-in hand-raises — `corpus-engine-scip` 11→12 and
   `sovereign-contracts` 33→34 — done by hand with a `SYSTEM_OVERVIEW.md` §10.1
   ledger, exactly as `dm-decision-extractor-move` (2026-09-17) and
   `dm-next-edit-move` (2026-09-17) did. That is a ratchet cap raised to admit a
   sanctioned edge, not a pass bar weakened; `--update-baseline` is still
   forbidden (PROMPT §7).
3. No `[[exception]]` is added and no pass bar changes, so nothing here is the
   operator's.

**Evidence** (reproduced this session, on `ralph/domains-campaign`).
- The demo fails as the package says: `python3 scripts/domains-census.py
  misnamed` -> exit 4, 33 untagged files (`corpus-index` 19, `understanding-atlas`
  13, `sovereign-peer-wire` 1), none in sovereign-mesh. The per-crate row
  (coverage bypassed) is `sovereign-mesh fabric 13860 / 16684 83.1% MISNAMED`,
  its non-fabric rows `workbench 674 projects.rs` and `workbench 2150
  reindexer.rs`.
- The reaches reproduce: `reindexer.rs:650` `use corpus_engine::facts::{...}` and
  `:708 corpus_engine::facts_store::FactStore::open`, both in production
  `run_overlay_merge`; `projects.rs:364
  sovereign_contracts::rebrand::projects_json()` in `Registry::default_path`.
- The gate is already decided: `order.md:32` "no new exception"; the boundary
  doc names `code-facts` (`docs/CODE_TOOLING_BOUNDARY.md:67,:404`); the
  workbench cluster's registry dest is `corpus-engine-watchers`
  (`quality/DOMAINS.toml:5581`) and DOMAINS §4 puts Workbench in the code-intel
  package (`quality/DOMAINS.md:160`).
- `sovereign-contracts` is package-legal for code-intel: it is a
  `[[package_leaf]]` (`quality/ARCH_LAYERS.toml:850`) and the siblings
  `corpus-engine-scip/Cargo.toml:73` and `code-next-edit/Cargo.toml:18` already
  name it. So `projects.rs`'s edge needs only the fan-in cap moved, not an
  exception.
- The caps are real, not inferred: `quality/baselines/fan_in.tsv:10,11` read
  `11 corpus-engine-scip` and `33 sovereign-contracts`.
- Re-homing to `corpus-engine` was rejected: it contradicts the operator's own
  rung ("workbench leaves for code-intel", `quality/campaigns/domains.toml:429`)
  and grows the god-crate the campaign is demolishing.

**Correction.** `ralph/STATE.md`: the DEMO row's `depends` gains
`dm-registry-coverage` (restores the instrument's coverage) and
`REVIEW-build-mesh-workbench-deferred`; `HUMAN-mesh-workbench-gates` is replaced
by `REVIEW-build-code-facts`, and `REVIEW-build-mesh-workbench-deferred` now
depends on it. `ralph/NEEDS_HUMAN.md` is removed. The worker's own entry for
this unit is on branch `ralph/DEMO-d5-misnamed` (commit `183949c17`); its facts
are re-measured above and its correction is carried here.

**Falsified by.** A showing that the code-intel package may NOT reach
`sovereign-contracts` (then `projects.rs` needs a port or an exception); that
`code-facts` cannot be built without a `corpus-engine` edge (then the carve
needs a different shape); or an operator ruling that a hand-raised fan-in cap is
an operator-only act (then this decision is the one to revert, and the fork is
the package's).

**REVIEW-AFTER:** the choice to pull `code-facts` (a wave-3 corpus-engine
carve) forward to unblock a wave-1 mesh move, and the fan-in hand-raise as a
director act. Both are within the charter's "row order, re-scoping" and its
"placements the docs already imply", but the campaign's rungs put mesh workbench
(wave 1) before corpus-engine workbench (wave 3), so the reorder is the part a
reviewer should read first.

**Landed in.** this commit — `ralph/STATE.md` (the rows) and this entry;
`ralph/NEEDS_HUMAN.md` removed (untracked; `.git/info/exclude:21`).

## 2026-09-18 · REVIEW-build-code-facts · the `corpus-engine-scip` fan-in cap the next row names was already spent

**Fork.** `REVIEW-build-mesh-workbench-deferred`'s text says it hand-raises
`corpus-engine-scip` 11→12 (reindexer's `ScipGraph`). But `REVIEW-build-code-facts`
landed first and already spent that raise: `code-facts` (the code-intel package's
new fact base) depends on `corpus-engine-scip` for `facts_check.rs`'s `ScipGraph`
dispatch, so the cap is `12` at HEAD.

**Choice.** Correct the next row to `12→13` and record the raise in this unit's
§10.1ae ledger. The alternative — leaving the next row's premise — makes it raise
the cap to `12` when it is already `12`, a no-op that fails `LAYER` when the
reindexer edge lands (fan-in `13 > 12`).

**Evidence** (reproduced this session, on `ralph/domains-campaign`).
- `quality/baselines/fan_in.tsv:11` now reads `12 corpus-engine-scip`, raised by
  this unit's §10.1ae ledger (`sovereign/SYSTEM_OVERVIEW.md`).
- `code-facts/src/facts_check.rs:20` names
  `corpus_engine_scip::scip_graph::ScipGraph`; `code-facts/Cargo.toml` carries
  `corpus-engine-scip = { workspace = true, optional = true }`.
- `corpus-engine-watchers/Cargo.toml` names no `corpus-engine-scip` today, so the
  next row's move does add the dependent — the raise is real, only its base moved.

**Falsified by.** A showing that `code-facts` need not depend on
`corpus-engine-scip` (then the cap is `11` again and the next row's original
`11→12` stands); or that `internal_dep_edges` exempts optional deps (then this
unit's own `LAYER` run would not have needed the raise).

**Landed in.** this unit's code commit and the `ralph:` marker commit.
