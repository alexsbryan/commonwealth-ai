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
