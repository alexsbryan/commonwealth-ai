<!-- ledger -->

**five-programs-65 · 2026-09-25 · fp-solo-a halt + row sizing · operator** — this commit
- Needed: fp-solo-a halted on a red `cw-rails-lift.sh --sandbox`. The operator also judged the rows too granular and asked for them to merge down. The seat measured both before recommending.
- Chose:
  - (1) fp-solo-a is accepted on its other checks.
  - (2) fp-solo-b..e fold into two outcome rows. fp-solo-lift covers cw-rails' own root lock plus the lift repair. fp-solo-clients covers one sibling locator, `ensure_rails` as a `ServingHost` call, and the real-binary e2e. The `cw-rails ensure` verb is struck.
  - (3) The shared worker prompt now sizes a row by OUTCOME. A row carries its own proof, may take several commits, and runs its heavy checks once at the end. Its bound is a stated lift, not a verb count. `audit_every` goes from 15 to 5.
  - (4) When a launched backend stays silent, `ServingHost` reports the tail of its log. It never retains or reaps the child.
- Because:
  - The lift break predates the row (see the appendix).
  - Principle 12: cw-rails owns its root, and a client owns reaching it.
  - Principle 8: bring-up already has one decider, the `bring_up_decider` in ARCH_LAYERS.toml. fp-solo-c's `ensure` would have been a new copy of it inside the one crate that is forbidden to reach it.
  - The sizing is measured, not asserted (principle 7).
  - Boundary gate: 51, unchanged. There is no code in this commit.

<!-- appendix -->

## five-programs-65 · 2026-09-25 — accept fp-solo-a, repair the lift, fold the solo rows, size rows by outcome

<details><summary>reasoning, evidence, package</summary>

**The lift.** It last held at b25c04b3e (2026-09-11) and has two independent breaks. The first: fp-40 (cba47e483, 2026-09-22) moved transport's vocabulary into sovereign-contracts and gave commonwealth-transport a sovereign-contracts dep. The lift walk exits FORBIDDEN on that edge (scripts/cw-rails-lift.sh:149,167). The second: hakari (44f9a1bdc, 2026-09-17) gave commonwealth-rails, -transport and -core, among others, `workspace-hack = { version = "0.1", path = "../../../workspace-hack" }`. The walk rejects that as HAND-SPELLED-PATH, but FORBIDDEN exits first and hides it. cw-work-lift.sh walks a closure that also carries it (commonwealth-work/Cargo.toml:52).

The seat walked the manifests. cw-rails' closure is 15 crates and includes sovereign-contracts and sovereign-time. cw-work's closure is 7 crates and reaches only kernel-types and oicp-types.

fp-40's move set, sovereign-contracts/src/transport.rs (234 lines, 65 of them code), depends only on kernel_types::{NodeId, NodePubkey}, std SocketAddr and async_trait. §12 3a rung 1 puts it back with cmnwlth, because Phase B retires the daemon's mesh endpoint. The mesh-join-vocab admission (2026-09-23) states the rule fp-40 broke: vocabulary that a commonwealth crate must name cannot live in sovereign-contracts. Letting sovereign-contracts into the rails closure was rejected: it would pull 42k lines into the small daemon for 65.

**Bring-up.** `ServingHost::ensure_reachable` (sovereign-turn-client/src/reach.rs:276) is declared the workspace's one `bring_up_decider` (quality/ARCH_LAYERS.toml:1501). The daemon (Cargo.toml:96) and cli-llm (Cargo.toml:59) already depend on turn-client. The settled bar forbids bring-up "on a timer or a health signal" (ARCH_LAYERS.toml ~:1495), so fp-solo-d's re-ensure on a refused dial is struck. There are seven identical locator copies; the six `fn locate()` plus serve_cmd.rs:302 `locate_dev_bin_for_spawn`, which fp-solo-b's premise grep missed.

**Sizing, measured on this queue's 150 worker transcripts and git history.**
- 154 row ids. 16 were written up front; 69 were minted by director or operator commits, and about 22 exist only because a halted row was split.
- The median session was 5.4 min (7.3 when the row got marked). 39% of worker time went to checks, 46% in row-marking sessions. The first code edit came at about 77 s.
- 60 NEEDS_HUMAN halts across 51 of 120 units (43%). At least 25 were a false premise and 6 were mint-cap overruns. Halted sessions plus resolution sessions cost 6.3 h, against 13.4 h for sessions that finished a row.
- The median work row changed 182 lines, and 36 of 98 consecutive pairs share a code file.
- The runner already supports multi-commit rows (scripts/ralph.py:1144-1151, and `[~]` resumes); the prompt text was the limit.
- The counter-example: the fw waves grouped rows by SHAPE with no per-outcome proof. fw-1 took 11 sessions for −1 against a −8 target. Grouping by outcome is the fix.

Falsified if, after 10 outcome rows, halts per row or fixed-check share do not fall, or if a row regularly exceeds a 7200 s session.

</details>
