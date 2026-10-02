<!-- ledger -->

**phase-b-28 · 2026-09-27 · pb-svrn-dials-serve → a reload on the dialing path swaps one provider cell every reader shares · director** — this commit
- Needed: the seat's review of cfb83f03b asks for a reload test at `/v1/models` or `/status` that sees serve's new primary. The worker showed it cannot pass with the alias republish alone. `AppState`'s `local_inference` is built once at boot (daemon.rs:3217; a plain `Option<Arc>`, state.rs:1019), and the reload swaps only `EmbeddedDaemon.inference_provider` (daemon.rs:930, :865). So `/v1/models`, `/status`, `/oicp/v1/capabilities` and the daemon's turns all answer from the boot provider. The worker asked whether fixing that is a delta the row does not state, and offered (A) one shared cell, (B) dialing path only, or (C) a narrower test with the fix moved to another row, recommending (C).
- Chose: (B), built by reusing (A)'s mechanism. The cell half of serve's `ReloadableProvider` (sovereign-serve reload.rs:32) moves to sovereign-contracts beside `InferenceProvider` (traits.rs:298), with its forwarding-completeness test. On the dialing path only, boot wraps the loopback provider in that cell, and `ReloadSource::Serve` stores the rebuilt provider into it. The seat's test stands as written, plus a peer-manifest assertion and a context-size change. The in-process path's reload does the same thing wrong, predates this row, and is routed to pb-serve-distributes, which deletes that path.
- Because:
  - The row already states the dialing-path fix. Seat d8e903313, in the row: "Rebuild it wherever the daemon changes what serve holds: after the reload forward (a reload test asserts the peer manifest names the post-reload model)". The existing test (provider.rs `a_reload_reloads_serve_and_the_manifest_names_the_new_model`) asserts on the rebuilt provider, a seam no reader sees. That makes the worker's premise false for this path, and (C) would close the row on a defect the row names (principle 6: peers are offered a model serve no longer holds).
  - What goes stale is the whole provider, not only `ServedSelf`. The embed id, context window and query prefix are fixed at construction (serve_client.rs:456-480), and `models.embed` and `models.context_size` are hot-reloadable (admin_http.rs:263, :294). A cell over `ServedSelf` alone would need a second fix later.
  - Extend, never re-own. The cell and its test that every trait method is forwarded already exist in serve, and a second copy in the daemon would be a second decider (principle 8). §12 3a places it: two programs use it, and it belongs to the serving contract, so sovereign-contracts.
  - The in-process path stays unchanged in this row, because changing it would be a behaviour delta the row does not state. It is a success-shaped reload that pb-serve-distributes removes along with the path.
  - Boundary gate: `cargo xtask boundary-gate` EXIT=1, 49 violations at eb56e669e. This commit contains no code.

<!-- appendix -->

## phase-b-28 · 2026-09-27 — the dialing path's reload is fixed by one shared provider cell, not by a narrower test

<details><summary>reasoning, evidence, package</summary>

Checked in this session: daemon.rs:3217 (`serving_seed` built from
`self.inference_provider()` once), state.rs:1019 (`local_inference:
serving_seed.local_inference`, a plain field), daemon.rs:930
(`factory.build_provider` then `swap_inference_provider`, which writes only the
daemon's own slot), provider.rs:63-97 (`ReloadSource::Serve` forwards, re-reads
`ServedSelf`, republishes aliases, and builds a NEW loopback provider),
oicp-client lib.rs:1534 (`served: Option<ServedSelf>` held by value),
admin_http.rs:258-294 (embed and context_size are in `models_changed`, so they are
hot-reloadable). The worker's live run (target/ralph/phase-b/reload_live.sh)
could not tell before from after, because the mock engine always names its model
`mock`. The stub-serve harness in status_answers_from_serve.rs can: its stub
reports a different primary and context size after the reload.

Rejected:
- (C) alone. It leaves the row's own stated requirement unmet, and it closes on a
  test at a seam no reader sees.
- (A) on both paths inside this row. On the in-process path that changes what a
  turn runs on after a reload, which the row does not state. It is the right
  end state, and pb-serve-distributes reaches it by deleting that path.
- A cell over `ServedSelf` in oicp-client. It misses the embed id and context
  window, and it adds a second swappable holder next to serve's.

Falsifiers:
- The moved cell cannot sit in sovereign-contracts without pulling a dependency
  that leaf does not already carry (the layer-gate or boundary-gate grows). Then
  the rung is wrong: return with the gate output.
- The router or runtime caches a fact from the raw provider at construction, so
  a shared cell underneath still answers stale. Then the test goes red for a
  reason this decision did not foresee: census the cache and name it.
- The move grows this row's LIFT by more than ~200 lines.

</details>
