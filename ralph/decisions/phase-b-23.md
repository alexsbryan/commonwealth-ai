<!-- ledger -->

**phase-b-23 · 2026-09-26 · pb-svrn-dials-serve → fp-68 split off to pb-serve-ranks; inbound peer inference kept via serve's manifest; FIM/NES model routes on serve · director** — this commit
- Needed: pb-svrn-dials-serve stopped at census before any code. Its own premise was false at fc5478ffb. After the switch the terminal arm advertises no models, so inbound peer inference goes dark, and FIM/NES have no route on serve. fp-68 is also held by the daemon's own mesh routing, admission and `entry_endpoint`, not by in-process serving.
- Chose:
  - Inbound peer inference, option (a). serve mounts `/oicp/v1/capabilities`. The terminal arm's loopback mode reads serve's manifest and slots, and the daemon's peer listener forwards to serve. A remote entry node keeps the empty manifest.
  - FIM/NES. serve mounts `/v1/completions` and `/v1/edit_predictions` over its own adapter. The daemon's routes stay the editor's door, keep their context assembly and dial serve for the model call.
  - fp-68, option (b). pb-svrn-dials-serve closes the two fp-10 edges only, plus the five setup/preflight reads. A new row, pb-serve-ranks (depends pb-svrn-dials-serve and pb-rails-origins), moves ranking behind serve, places the admission middleware and `entry_endpoint`, closes pb-serving-proofs' reload-router defect (a), and retires fp-68. pb-serve-package depends on it. pc-inference-origin keeps only its foreign-server half.
  - LIFT: pb-svrn-dials-serve ~1,300 → ~1,500. pb-serve-ranks ~900.
- Because:
  - Option (b) for peer inference is an end-user-observable loss, and the charter leaves that to the operator. Option (a) keeps the behaviour, and the route it adds is the one pb-mesh-exit-mesh already has serve register ("serve registers the member client and its OICP manifest"). Extend, never re-own.
  - phase-b-21 gives ranking to serve, reaching peers only through `RailsTransport`, which pb-rails-origins builds and this row did not depend on. The ranking move is proved on a mesh-of-two and the switch by a loopback chat turn. The proofs differ, so the charter's split rule applies. Until then the daemon keeps `InferenceRouter`, with the loopback arm as its local venue, so outbound routing is unchanged.
  - `fim_adapter` is serving-host, which is serve's library (rung 1). §2 gives the editor door to `svrn code`, and pb-meshapp-rest moves the door there. The door is not the model.
  - Boundary gate: 49 at fc5478ffb (EXIT=1). No code is in this commit.

<!-- appendix -->

## phase-b-23 · 2026-09-26 — the switch keeps both peer directions working; fp-68 goes to the row that moves ranking

<details><summary>reasoning, evidence, package</summary>

Reproduced at fc5478ffb from the host:
- `sovereign-serve/src/lib.rs:304-310`: openai_bundle mounts only `/v1/chat/completions`, `/v1/embeddings` and `/v1/models`. `grep -rn 'capabilities"\|edit_predictions\|"/v1/completions'` over sovereign-serve and sovereign-compute finds no route, only a log string at compute assembly.rs:403.
- oicp-client defines neither `edit_slot_info` nor `resident_slots`, so `SplitInferenceProvider` inherits the empty defaults. build/inference.rs:72-75 documents the empty manifest as deliberate for a remote entry node.
- `grep -rhoE 'sovereign_serving_host::[a-z_]+' sovereign-daemon/src | sort | uniq -c` gives admission 22, peer_inference 7, openai_http 6, venue_host 3, entry_endpoint 1, model_fetch 2, worker_eligibility 2, pinned_worker_source 2, slot_manifest 2, slot_select 1, state 1, tool_profile 1, inference_adapter 1. This matches the package.
- `entry_endpoint::EntryNodeEndpoint::parse` is called inside the terminal arm (build/inference.rs:119).
- The sovereign_inference sites in the daemon are embedded 24, capacity 4, served_kind 3, hardware 2, remote 2, setup_planner 1, llama_logs 1, llama 1, rpc_worker_main 1 and fast_exit_skip_destructors 1. The five setup/preflight lines are at assets_http.rs:39-40, build/preflight.rs:118-125, daemon_cmd/vram_plan.rs:22 and bin/sovereign-daemon.rs:211.
- daemon_cmd/boot.rs is 1,196 lines.
- ARCH_LAYERS' own fp-68 reason names "admission, venue routing and the InferenceRouter", so the row's claim that deleting in-process serving retires fp-68 contradicted the exception it was meant to retire.
- `cd corpus-engine && cargo xtask boundary-gate` in the toolbox: 49 violations, EXIT=1.
- `work_in_flight` could not judge: cw-rails on :9747 was unreachable. The files edited here are the campaign's own queue.

Placement of the five setup reads: the fit checks read serve's sections, which the row already says svrn stops reading, so they are serve's by rung 1. The setup UI's hardware and planner reads dial serve, and an unreachable serve is a named absence like every other dial in the row.

What would falsify this:
- The loopback manifest mode cannot tell a loopback serve from a remote entry node without a new config key. Then the choice between a key and the default-base test is a user-facing config change, and it goes to the operator.
- pb-serve-ranks' census finds the admission middleware fits no ladder rung without growing the host kit past its cap. That is the operator's.
- The mesh-of-two proof shows the forwarded inbound turn misses the pre-registered first-token bar. Report the numbers and stop.

</details>
