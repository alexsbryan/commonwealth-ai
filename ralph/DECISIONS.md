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
