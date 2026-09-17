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
