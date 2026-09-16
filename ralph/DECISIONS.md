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
