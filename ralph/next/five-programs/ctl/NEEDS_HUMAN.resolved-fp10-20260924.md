# NEEDS_HUMAN — fp-10 (sovereign-daemon → sovereign-inference / sovereign-compute)

## (a) Unit

`- [ ] fp-10 — depends [fp-6] — … DIAL inference/rpc-worker + compute (§12 decision 2): the daemon's in-process inference, rpc-worker and compute supervisions become serving-surface clients reporting ABSENCE ("a daemon alone serves no model"). TSV pairs sovereign-daemon→sovereign-inference (42) / →sovereign-compute (25). — check: scoped lint 0; gate delta recorded`

fp-6 is `[x]` (86b03b9b7); the row is dep-ready. Premise check at ba832a97d
fails; no edit was made, STATE.md untouched.

## (b) What I ran, at ba832a97d

`scripts/ralph-check.sh boundary` → `boundary-gate FAILED (62 violation(s))`,
both edges red:

    ✗ [svrn] sovereign-daemon → sovereign-inference
    ✗ [svrn] sovereign-daemon → sovereign-compute

`grep -rn sovereign_inference|sovereign_compute sovereign/crates/sovereign-daemon`:
inference 42 sites over 14 src files (bootstrap.rs 10, build/inference.rs 6,
mesh_http.rs 5, build/preflight.rs 4, rpc_warm_http.rs 3, bin/sovereign-daemon.rs 3,
provider/discovery_policy/boot/assets_http 2 each, …); compute 19 src + 6 in
tests/main (compute_child_e2e, distributed_primary_respawn_e2e,
named_model_routes_after_child_serves_e2e).

The row's premise is "become serving-surface clients". There is no serving
surface to be a client of:

1. **The daemon IS the serving process.** `build/inference.rs:271`
   `engine_factory::build_engine(config)`, `:454`
   `build_compute_layer_with_distributed`, `bootstrap.rs:25/572-741` (the
   EmbeddedLlamaCpp + child-slot spawn policy), `bin/sovereign-daemon.rs:59,64`
   (`Launch::ComputeChild` → `sovereign_compute::child_main::run`,
   `Launch::RpcWorker` → `sovereign_inference::rpc_worker_main::run`). Outside
   the daemon, only svrn-side crates construct engines (cli-llm chat_cmd/
   bootstrap.rs, atlas_cmd, desktop state.rs) — no cmnwlth crate does.
2. **D2's named host cannot host it.** D2 says "extend cw-rails, do not build a
   new binary". `quality/ARCH_LAYERS.toml:676-679` forbids
   `commonwealth-rails → sovereign-*` ("BUILT AND RUN outside the monorepo by
   scripts/cw-rails-lift.sh"), and `:682-692` refuses the commonwealth
   application crates too. cw-rails can never link sovereign-inference or
   sovereign-compute, so it cannot own engine construction.
3. **fp-16 already recorded this** (d1aaa2843): "no serving surface exists to
   dial — cw-rails serves /v1/mesh/* only … Building the serving mount is
   FIVE_PROGRAMS Phase B itself (~20 edges; the largest single phase), not one
   atomic row, and §11's 'do not fake' forbids a stub dial." fp-10 is the
   engine that surface would wrap, so it inherits the same missing prerequisite.
4. Doing the row literally — stubbing the construction sites to report absence
   with nothing to dial — turns every `/v1/chat/completions` on a stock install
   into "a daemon alone serves no model". That changes the answer of every
   inference route; the PROMPT's behaviour rule sends that here, not to a commit.

## (c) What the operator must decide

1. **Which process owns engine construction and the compute/rpc-worker
   children?** Options: (a) a NEW cmnwlth serving binary in the `cmnwlth`
   package (ARCH_LAYERS.toml ~1287-1308 already puts sovereign-inference,
   -compute, -serving-host, serving-policy there) — this reverses D2's "do not
   build a new binary" and is Phase B, i.e. a REVIEW-mint of its own, not one
   row; (b) grandfather `sovereign-daemon → {sovereign-inference,
   sovereign-compute}` with `[[exception]] package = "svrn"` as the honest
   state until (a) exists (D4's "the edge stays red — that is the honest state"
   reasoning, applied here); (c) re-home the daemon's serving half into the
   cmnwlth package (a placement decision, D5-shaped).
2. **The rpc-worker exec target** (fp-25 depends on this answer, see its row):
   whichever binary 1 names must be the one `sovereign-inference/src/embedded/
   rpc_distribution.rs:2390` `current_exe() --rpc-worker` re-execs; today both
   `sovereign-daemon.rs:64` and `sovereign-cli-daemon/src/lib.rs:161` carry it.
3. **Sequencing:** REVIEW-mint-fp-mesh-dial (ce4e2aeac) and fp-25 both depend on
   fp-10; if 1 is (a), fp-10 should become a mint row under a cap, and those
   two re-point to its children.

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
