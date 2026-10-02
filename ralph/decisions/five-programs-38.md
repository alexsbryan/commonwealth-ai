<!-- ledger -->

**five-programs-38 · 2026-09-24 · the three HUMAN rows + three NEEDS-OPERATOR classes · operator** — this commit
- Needed: at boundary 54 (53f9d8434) about five red edges had a live code owner: commonwealth-state ×2 through fp-75..87, commonwealth-media through fp-47, and the core and mesh residues. The rest waited on HUMAN-fp25, HUMAN-fp11, HUMAN-fp12 or a NEEDS-OPERATOR appendix line. The operator asked for the decisions to be cleared now rather than after the queue drained, and the seat asked once, with its recommendation first.
- Chose: (1) Phase B starts now. Every edge whose only closing arm was an `[[exception]]` or a new serving host closes by building the host, not by exception: HUMAN-fp25 (a), HUMAN-fp11 (a), all seven HUMAN-fp12 pairs, and the fp-9/fp-10 exceptions granted "until Phase B". REVIEW-plan-fp-phase-b maps the edges to hosts and mints one campaign per host. (2) One node identity, owned by cw-rails, landing with finish condition 2. (3) sovereign-cli-shared's thin half is admitted as a shared leaf (§12 3a), minted by REVIEW-mint-fp-cli-shared-leaf. (4) corpus-engine's build.rs: "Can we decouple corpus engine from the declarative recipe definitions and just agree on abstractions and interfaces?" corpus-engine keeps the recipe contract and stops vendoring the definitions, minted by REVIEW-mint-fp-recipes-decouple.
- Because: operator's word. On (1) the seat recommended a uniform exception and named what it would cost: a gate at 0 that records the boundary without making the programs liftable. The operator chose the liftable version. Boundary gate 54, unchanged; no code in this commit.

<!-- appendix -->

## five-programs-38 · 2026-09-24 — Phase B starts now; one node key; the cli-shared leaf; recipes decouple

<details><summary>reasoning, evidence, package</summary>

Asked as four questions. First, the three HUMAN rows as one policy ("edges where a program embeds a capability no other process serves yet"), with three arms: a uniform `[[exception]]` until Phase B (the seat's recommendation), exceptions for the three HUMAN rows only, or start Phase B. Second, the node-identity question REVIEW-fp54-signer-identity (7ea05c6fc) left open. Third, the cli-shared leaf admission (the appendix lines for cli-mesh→cli-shared and cli-dev→cli-shared). Fourth, the `[ingest] corpus-engine: has a build.rs` line.

Costs stated when asked, which the minted rows must carry with a reader rather than discover:

- **Phase B.** It is several campaigns, each a REVIEW-mint under a cap. Under HUMAN-fp11 (a), the code tools leave the daemon's MCP surface whenever the code program is down; the daemon proxies them and reports the absence. §2 places notes with the code program, so notes and session_state follow unless the plan finds a reason in §2 that they should not. HUMAN-fp25 (a) changes the documented Windows sidecar build contract (stage-daemon-sidecar.sh:91, ENV_FLAGS `SOVEREIGN_SIDECAR_FEATURES`). The pods exec split touches a pod contract that only a Vast pod can check.
- **One node key owned by rails.** The daemon's `~/.svrnmesh/node_key` retires, and fp-74's attestation is signed by rails at the daemon's request. Existing installs have the daemon key in their peers' rosters, so the migration ships in the same commit as the flip. It is sequenced with condition 2 because the daemon's second mesh endpoint (sovereign-mesh `iroh_access`) goes away there.
- **cli-shared thin half.** §12 decision 5 already says cli-shared keeps only the thin dispatcher helpers; the admission is the rung that 3a reserves for the operator. fp-5's refusal stands unless the census shows otherwise: sovereign-cli's light verbs use code_index and scip in-process.
- **Recipes.** Six sites read OUT_DIR today: recipe_builtin.rs, registry.rs:38, recipe_schema.rs:22, recipe_templates.rs:31, filters/assets.rs:55-73, and the configurable_atlas.rs:251 test. `RecipeId` is a closed enum over an open set (ARCH 9), used in 3 files across 12 arms, mostly tests. An offline first install must keep working (principle 6).

This decision is falsified if the Phase B plan finds an edge whose owning program §2 does not name, or a host that cannot be built without a sovereign-* dependency in a commonwealth-* crate where ARCH_LAYERS forbids it with no except. Either finding is a NEEDS_HUMAN line with the count, never a quiet exception.

</details>
