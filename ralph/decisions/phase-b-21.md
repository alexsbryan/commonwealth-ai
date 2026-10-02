<!-- ledger -->

**phase-b-21 · 2026-09-26 · pb-serve-package → serve owns ranking; row re-ordered, pb-serve-cli-face split off · director** — this commit
- Needed: pb-serve-package stopped at census before any code. The §12 3a ladder did not place serving-host's mesh half with sovereign-scheduler (peer ranking and the peer dial) or cli-mesh's GGUF placement planner. The worker recommended amending §2 so that cmnwlth ranks inference venues.
- Chose:
  - serve owns peer ranking and the peer-inference client. sovereign-scheduler and all of serving-host join serve, and serving-host is not split.
  - The planner, warm-cache and peer model-pull verbs move to serve's CLI face. A new row, pb-serve-cli-face, does that, and the `svrn` spellings are kept.
  - The VRAM advert is declared by serve and advertised by cw-rails.
  - pb-serve-package now depends on pb-serve-cli-face, pb-svrn-dials-serve, pb-rails-origins, pb-mesh-exit-mesh and pb-pods-verb. Those rows retire most of the shim consumers. The row names what closes each remaining edge.
- Because:
  - The design already answers the ranking fork, so it is not a new question. phase-b-1 (operator) says cw-rails "never ranks". FIVE_PROGRAMS §2 says the same of `cmnwlth`. pb-mesh-exit-mesh says serve registers the member client. pc-inference-origin says "serve's router ranks venues", reading cw-rails' roster. Option (ii) would reverse an operator decision, and the charter does not delegate that.
  - §4 rule 8 holds: serve reaches peers only through cw-rails' reach door (`RailsTransport`, pb-rails-origins).
  - Principle 11: with the scheduler in serve, `slot_aliases` needs no move and serving-host needs no crate split.
  - Boundary gate: 49 at fe9c49529 (EXIT=1), unchanged. No code is in this commit. The trial re-map reads 55.

<!-- appendix -->

## phase-b-21 · 2026-09-26 — peer ranking is serve's; the planner verbs are serve's; the package flip waits on the rows that retire its consumers

<details><summary>reasoning, evidence, package</summary>

Reproduced at fe9c49529 in the toolbox (`cd corpus-engine && cargo xtask boundary-gate`):
- HEAD reads 49.
- Trial: sovereign-scheduler, sovereign-serving-host, serving-policy, sovereign-inference, sovereign-compute, sovereign-serve and sovereign-gliner moved from `[[package]] cmnwlth` to a new `[[package]] serve` reads 55. ARCH_LAYERS.toml was restored after the trial. The six new edges:
  - `[cmnwlth] sovereign-cli-mesh → sovereign-inference`
  - `[cmnwlth] sovereign-mesh → sovereign-inference`
  - `[cmnwlth] sovereign-mesh → sovereign-scheduler`
  - `[cmnwlth] sovereign-mesh → sovereign-serving-host`
  - `[cmnwlth] sovereign-mesh-test-harness → sovereign-scheduler`
  - `[serve] sovereign-serving-host → commonwealth-core`

  phase-b-20's trial without the scheduler read 54, with `serving-host → scheduler` red. The scheduler's only normal-dependency users are serving-host, sovereign-mesh (shims) and the test harness (`git grep -l sovereign_scheduler`; the two sovereign-contracts hits are doc comments).

What each edge carries (git grep at fe9c49529):
- sovereign-mesh's non-shim src uses are two lines: capabilities.rs:77 (VRAM) and guest_source.rs:21 (guest_lender). Everything else is the domains re-export shims (lib.rs:37-100).
- The shim consumers are:
  - the daemon: decision_log, decision_trace, guest_lender, inference_adapter, peer_inference, worker_eligibility and pinned_pod_snapshot, all of which pb-svrn-dials-serve or pb-mesh-exit-mesh removes;
  - cli-llm's pod verbs: pinned_pod_snapshot and pinned_worker_source, moved by pb-pods-verb;
  - cli-mesh: model_fetch, moved by pb-serve-cli-face;
  - sovereign-mesh's own scheduler tests.
- serving-host → commonwealth-core is 15 lines in 8 files:
  - NodeId is already kernel-types' (commonwealth-core ids.rs:18 re-exports it);
  - ModelFileInfo and ModelFileListing are the model-transfer wire;
  - PeerHealthTracker;
  - one comment.
- The harness's 18 scheduler refs are all in mesh_sim, which is the ranker's fleet simulator.

Rejected:
- Option (ii), cmnwlth ranks. It reverses phase-b-1.
- Option (i) as written, svrn ranks. svrn is not the capability owner: serve registers the member client in pb-mesh-exit-mesh.
- Option (iii), leave serving-host in cmnwlth. It fails the delta rule, and it leaves the serving cluster in the package that must take no llama.
- A llama-free GGUF leaf. It is a new leaf, which is the operator's, and it cannot run `projected_overheads`.

REVIEW-AFTER: §2's `cmnwlth` row says it "owns … decision log". If that cell means the scheduler's routing decision log, it contradicts "never ranks" and moves to serve's row at the declaration commit. The row tells the worker to check this. If it means something else, the cell needs a clearer name.

What would falsify this:
- The pb-serve-package census finds a use of the ranker by cw-rails itself (not through sovereign-mesh's shims) that cw-rails needs in order to answer a peer. That would make ranking shared, and it goes to the operator.
- A moved verb's `svrn` spelling cannot be kept through the dispatcher. That is end-user-observable, so it goes to the operator.
- The re-map after the five dependencies land still carries an edge that no bullet in the row names.

</details>
