<!-- ledger -->

**five-programs-18 · 2026-09-24 · fp-7 (ingest serving surface absent) · director** — this commit
- Needed: fp-7 halted before editing anything (ctl/NEEDS_HUMAN.md). The row dials "the ingest program's serving surface", and that surface does not exist. Building one, or a CLI that carries the member proof, is new capability.
- Chose: park fp-7 behind a new last-placed row, HUMAN-fp7-ingest-surface, with three options and a recommendation. fp-29 depends on the surface for real, so it now waits on that row. fp-10, fp-42, fp-45 and both REVIEW mints needed fp-7 only for ordering, so they lose it. No code changed. Boundary gate unchanged at 63 (toolbox, `RALPH_QUEUE=five-programs scripts/ralph-check.sh boundary`, at 8ded9aadb).
- Because: §2 says ingest's wire is "CLI only", and §12 D2 decides the serving cluster's dial, not an ingest surface. The one CLI form, `svrn corpus pull`, would lose multi-candidate pulls and the member proof. The executor's shared-engine cancel and progress semantics have no verb at all. Even if the surface were built, the gate would not move (delta 0). New capability that also amends §2's table is the operator's call under the charter.

<!-- appendix -->

## five-programs-18 · 2026-09-24 — park fp-7 on an operator row; strike the ordering-only fp-7 deps

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp7-20260924.md. I reproduced it at 8ded9aadb:

- auto_ingest.rs:354-360 calls `sovereign_mesh::canonical_pull::pull_canonical_from_peer(&lead.candidate_urls, …, state.mesh_proof_stamp().await.as_ref())`.
- sovereign-cli-llm/src/corpus_cmd/partitions.rs:110-126 wraps a single URL and passes `None` for the stamp. Its comment says the fix is "for the CLI to ask its own daemon to pull". That argues for the dial going the other way from the row.
- ingest_executor.rs imports `corpus_engine::{CorpusEngine, IngestProgress, ProgressCallback}`. It registers in `self.engine.cancel_registry()` and calls `ingest_with_overrides(.., Some(progress), ..)`, which feeds `ctx.progress`.
- The boundary gate FAILED with 63 violations, and it still lists the row's two edges.
- FIVE_PROGRAMS.md §2 table: `svrn ingest` | recipe pipeline | CLI only.
- §12 D2 is titled "The serving cluster is cmnwlth's own process; the daemon DIALS it". It names cw-rails and does not mention ingest.
- TSV:77 (sovereign-mesh→corpus-engine) has a prerequisite cell that reads "ingest canonical-pull route". That is a prerequisite nobody built, not a surface that exists.

Both halves already belong to a class this ledger records as NEEDS-OPERATOR: the ingest-dial class (the STATE appendix lines for sovereign-runtime-recipe→corpus-engine and corpus-mcp→corpus-engine, and §12's open corpus-mcp membership bullet). The charter reserves new capability and end-user behaviour changes for the operator. A CLI dial would lose auto-pulls against `internal_auth = "member"` peers, which is a behaviour change. So this is a package, not a guess.

I re-pointed the dependents one at a time. fp-10 is the inference dial and consumes nothing fp-7 would build, so it now waits on fp-6, fp-7's own predecessor. fp-42 is already parked on its own operator question, so its dep is ordering only. fp-45 has its serve and dial halves landed at 49578c1e2, and the executor's CorpusEngine use is HUMAN-fp7's residue. The mesh-dial mint counts canonical_pull's sovereign_mesh:: sites as HUMAN-fp7's and mints nothing for them. The core-dial mint keeps its other dial deps. fp-29 is a second client of the surface, so it waits on the HUMAN row as well.

The recommendation is (a): keep both sites daemon-side. The member proof belongs to whoever holds membership (principle 12), and (b) and (c) buy zero gate movement for new capability (principle 11).

Falsified if a serving surface for ingest already exists somewhere the census missed. A route or verb that takes a unit slice with progress and cancel, or a stamped pull, would make fp-7 a plain dial again. Also falsified if dropping fp-7 from fp-10, fp-42, fp-45 or the mints exposes a real consumer of fp-7's output; that row would then halt naming it.

</details>
