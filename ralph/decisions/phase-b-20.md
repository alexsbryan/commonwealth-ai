<!-- ledger -->

**phase-b-20 · 2026-09-26 · pb-serve-program → split off pb-serve-package · director** — this commit
- Needed: pb-serve-program stopped at census. Its ARCH_LAYERS bullet (a `serve` package of inference, compute, serving-host, serving-policy and gliner) cannot meet the addendum's rule that every row is net-decreasing or delta 0, and no membership the worker tried could. It also asked whether the "serving process" rename covers every cw-rails-meaning site or the four named.
- Chose: option (iii). pb-serve-program lands serve's binary and RUN smoke with `[lift.serve]` still judged under cmnwlth's rule, and declares no package. A new row, pb-serve-package, owns the declaration: split serving-host by half, place the mesh half and the GGUF helpers by the ladder, then declare `serve` with delta ≤ 0. pb-distribution depends on it. The rename covers every site where the phrase means cw-rails.
- Because:
  - The two outcomes need different proofs (a RUN smoke vs a gate delta), which is the charter's split rule. Option (ii), accepting +5, contradicts the addendum's delta rule, so it is not the director's. Option (i) folded into this row prices past its ~1,300-line lift and carries a placement question (§2 says cmnwlth "never ranks", and peer_inference ranks) the new row's census answers first.
  - Principle 12: serving-host holds two things, and the red edges are all in its mesh half.
  - Principle 8 for the rename: a partial rename leaves two names for one thing.
  - Boundary gate: 49 at 9e6ec2113, unchanged. No code is in this commit.

<!-- appendix -->

## phase-b-20 · 2026-09-26 — serve's package declaration is its own row, after serving-host splits

<details><summary>reasoning, evidence, package</summary>

Reproduced at 9e6ec2113 in the toolbox, `cd corpus-engine && cargo xtask boundary-gate`:
- HEAD: 49 violations.
- The row's set moved from `[[package]] cmnwlth` to a new `[[package]] serve` (sovereign-inference, sovereign-compute, sovereign-serving-host, serving-policy, sovereign-gliner): 54. New red: `[cmnwlth] sovereign-cli-mesh → sovereign-inference`, `[cmnwlth] sovereign-mesh → sovereign-inference`, `[cmnwlth] sovereign-mesh → sovereign-serving-host`, `[serve] sovereign-serving-host → commonwealth-core`, `[serve] sovereign-serving-host → sovereign-scheduler`. None closed.
- The same set without serving-host: 52. New red: cli-mesh → inference, mesh → inference, `[cmnwlth] sovereign-serving-host → serving-policy`.
- ARCH_LAYERS.toml restored with `git checkout` after each trial.

What the edges carry (git grep at HEAD): sovereign-mesh reaches serving-host through re-export shims left by the domains campaign (lib.rs:58,61,72,76,91,99,100) and guest_source.rs:21, and inference through capabilities.rs:77 (`local_gpu_total_vram_gb`). cli-mesh reaches inference in mesh_cmd.rs (13), mesh_bench.rs:1777 and remote_gguf.rs:67,156: GGUF shard and tensor inspection, RPC headroom, `projected_overheads`, one `LlamaBackend::init`.

Rename scope: `git grep -in "serving process"` outside ralph/ and docs/ is 125 lines. The listed sites drifted (setup_config.rs :1456 → :1468, admin_http.rs :91 → :92). sovereign-compute assembly.rs uses the phrase for serve's own meaning, which keeps it. daemon.rs:5257 is a user-visible error sentence, a wording change the row now names.

Falsified if: the serving half of serving-host cannot be separated from the mesh half without a serve → cmnwlth edge (then the split is wrong and the fork goes to the operator), or pb-serve-program's lift under cmnwlth's rule lets serve's binary pull a mesh crate that the RUN smoke then needs at runtime (then "runs alone" was not proved by this row).

</details>
