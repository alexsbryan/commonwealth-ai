<!-- ledger -->

**phase-b-16 · 2026-09-26 · seat review of pb-shell, pb-serving-assembly, pb-serving-kinds · seat** — this commit
- Needed: the seat's review of the three rows found proofs that cannot fail, a success-shaped rerank path and dark refusals, all owned by rows already marked `[x]`. HUMAN-pb-lanes-rerank's reading was also blind to the delta it exists to measure.
- Chose:
  - A new row, pb-serving-proofs, placed after pb-serving-kinds. It takes the defects whose owner is a closed serving row.
  - Findings that belong to open rows are routed to those rows as one bullet each: pb-serving-ner, pb-serve-program, pb-code-server, pb-meshapp-rest and pb-daemon-adopts.
  - HUMAN-pb-lanes-rerank keeps `[ ]` and carries its reading. The seat does not mark it: the lane cannot see the delta, and a blind instrument's pass is not a pass. Its disposition is the operator's.
- Because:
  - Principle 5: a proof whose PLANT stays green is not a proof, and it cannot be fixed by a later row that builds on it.
  - Principle 6: a reranker that fails to install still arms the lane, which is success-shaped.
  - The CHARTER splits work by proof. These proofs are pb-serving-assembly's and pb-serving-kinds', not pb-serving-ner's.
  - Boundary gate: 51, unchanged. No code is in this commit.
- FLAG: the reload refusal (compute assembly.rs:270) is a user-observable change the pb-serving-assembly row never listed. The row keeps it until the operator rules.

<!-- appendix -->

## phase-b-16 · 2026-09-26 — the serving rows' proofs get an owner, and the rerank reading is recorded as blind

<details><summary>reasoning, evidence, package</summary>

The reviews were read-only, one per row, with the seat checking the cited sites at 164e87069. These citations were confirmed by hand:
- `serves_rerank` reads `plan_serving` (assembly.rs:177-195);
- the idle-monitor gate's `include_str!("assembly.rs")` (:700);
- the reload refusal (:270) and containment's eprintln-only Refuse (containment.rs:193);
- the silent child→in-process fallback (manager.rs:1237-1242);
- the timeout-less compute client (client.rs:38);
- doctor's own `compute.enabled && distributed_primary` (checks_sovereign.rs:994);
- pb-shell's spelling scan (commonwealth-rails tests/solo.rs:217);
- the host kit's allow list (ARCH_LAYERS.toml:1111).

The reviewer's `reranker_standalone.rs:160` is :187 at HEAD, and its engine.rs:4040 is embedded/engine.rs:4042.

What the old reload did is what the FLAG turns on. At 6233a6b82^, `build_provider` (sovereign-daemon provider.rs:37-110) rebuilt only the in-process engine and never looked at the compute children. So a reload that changed `[compute]` or a child's model returned 200 and changed nothing in the child. The refusal replaces that success-shaped answer with a named one.

Two commit-body claims are corrected here, because published history is not rewritten:
- e0eaa79b6's "svrn exceptions 3 -> 2": ARCH_LAYERS.toml's `package = "svrn"` rows went 5 → 4 (c2efc8d20^ against HEAD).
- da819e9e2's "16 importers inside sovereign-daemon": 30 src files name `crate::loopback_guard` at da819e9e2^ (`git grep -l`).

Falsifier for pb-serving-proofs: if driving `build_provider` against a running generation needs a live compute child, and a test cannot provide one, the reload-branch proof becomes a process-level test in sovereign-daemon/tests, as pb-handover-first's was.

</details>
