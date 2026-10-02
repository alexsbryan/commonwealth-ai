<!-- ledger -->

**phase-b-17 · 2026-09-26 · operator rulings on phase-b-13 and phase-b-16 · operator** — this commit
- Needed: two items waited on the operator. phase-b-13's REVIEW-AFTER put `oicp-types` on the host kit's allow list, behind the kit's `mcp` feature. phase-b-16's FLAG covers the reload refusal (sovereign-compute assembly.rs:270), a user-observable change that the pb-serving-assembly row never listed.
- Chose: the operator ratified both on 2026-09-26.
  - The allow-list edge stands. The ARCH_LAYERS comment that credits "cw-rails does not enable the feature" gets corrected where pb-code-server already carries it.
  - The reload refusal stands. pb-serving-proofs gives it a tracing event.
- Because:
  - The kit edge. oicp-types is a shared leaf in every package's closure, so the edge moves no closure. The allow list, which the operator owns, is what guards the next dep behind `mcp`. A kit-local envelope would be a twin (principle 8).
  - The refusal. The old reload returned 200 and never touched the compute children (sovereign-daemon provider.rs:37-110 at 6233a6b82^), a success-shaped answer for a change it did not apply (principle 6).
  - Boundary gate: 51, unchanged. No code is in this commit.

<!-- appendix -->

## phase-b-17 · 2026-09-26 — the operator ratifies the kit's oicp-types edge and the reload refusal

<details><summary>reasoning, evidence, package</summary>

The seat asked in session and the operator answered: "I'm aligned with all the recs, except [HUMAN-pb-mesh-traffic]." That answer covered five recommendations:
- HUMAN-pb-lanes-rerank (b): measure rerank on against rerank off now. It gets its own commit with the numbers.
- phase-b-13: ratify.
- phase-b-16 FLAG: ratify.
- The atlas_grounding ledger violation: it needs an owner outside Phase B.
- HUMAN-pb-mesh-traffic: not accepted. The operator asked for more due diligence to find a more principled option.

</details>
