<!-- ledger -->

**phase-b-7 · 2026-09-26 · phase-b queue · seat** — this commit
- Needed: operator direction "make sure the campaign chunks work in the most intelligent way possible". Read against HEAD (fe2edc7bd), four chunk defects stood in the 21 open rows.
  - Two rows carried a check a worker cannot run honestly. `svrn quality check --lane retrieval-prod`/`synth` read the deployed daemon on :9741, and a worker never restarts it.
  - Three rows were several sessions each by their own census.
  - Two orderings would have stranded a user journey mid-queue.
  - A live CLI defect had no owner.
- Chose:
  - Measurement moves off the worker. HUMAN-pb-lanes-rerank and HUMAN-pb-lanes-dials-serve halt the loop at a clean row boundary so the seat can measure on the deployed daemon. The latency bars stay with pb-svrn-dials-serve's worker, on temp roots.
  - Split by edge: pb-daemon-mesh-exit into 3 rows, pb-ingest-dial into 2 and pb-meshapp-rehome into 3.
  - pb-membership moves up to right after pb-hostkit.
  - A new row, pb-atlas-kv, runs next.
  - Three premises are written into the rows that meet them: FIM/NES, published apps and the grants host.
- Because:
  - Principle 5: a lane run against the code the operator last started makes no claim about the row.
  - Principle 8: `DaemonReplicatedKv` is `RailsKv`'s twin.
  - Principle 6: a route deleted before its clients move is a silent 404.
  - Boundary gate: 51, unchanged. There is no code in this commit, and the owner histogram still sums to 51.

<!-- appendix -->

## phase-b-7 · 2026-09-26 — the queue re-chunked: measurement off the worker, split by edge, membership first

<details><summary>reasoning, evidence, package</summary>

**Measurement.** `quality/instruments.toml` gives `retrieval-prod` (57-90 s) and `synth` (272-330 s) the precondition `port-listening:9741`, and the probe connects to 127.0.0.1 on that literal port (sovereign-cli quality_check_cmd/exec.rs:33). A worker may not restart the deployed daemon (PROMPT.addendum hard rules), so pb-serving-kinds' "measure retrieval-prod" would have scored the code the operator last started. The same holds for pb-svrn-dials-serve's two lane bars, which also could not be taken "before the switch", because the switch is what they measure. The HUMAN rows sit right after their delta rows, so `Queue.current()` reaches them at a clean boundary and the supervisor exits with its approval notice (ralph.py:1250, :1385). A reading is never taken on a tree a worker is editing. The pre-registered bars are unchanged; only who measures them, and when, changed. retrieval-prod is a HARD recall lane where any delta is real (RUNBOOK §6), so it runs twice, and the second run shows it was deterministic. synth runs three times against its ±0.04-0.06 band.

**Splits.** Non-test src reference lines at fe2edc7bd (`git grep` per crate, tests excluded):
- daemon → commonwealth-transport 82/21 files, → commonwealth-core 218/52, → sovereign-mesh 185/32. pb-daemon-mesh-exit's own census already said "split by edge from the start".
- tools → corpus-engine 259/56, daemon → corpus-engine 115/33, mesh 5/4, runtime-recipe 12/1, tools → recipe-author 2/1.
- The daemon's eight meshapp/media edges: 169 lines in 40 files, each a ladder decision with its own journey proof (five-programs fp-12's census: grants has no host, and code-next-edit needs the code server).

The five-programs calibration is fw-1: 6,544 lines in one row took 11 sessions and halted three times: two NEEDS_HUMAN, and one stall of three sessions without a commit. Its rows over 1,000 lines finished in one session only when they were mechanical moves (fp-60, 4,161 lines, 13 min). Each split row closes one or more edges with its own BOUNDARY delta and PLANT, which meets CHARTER's "split only when two outcomes need different proofs". pb-cli-llm and pb-ingest-rehome stay whole. Their census depends on the rows ahead of them, and pb-cli-llm's first bullet is already a re-census.

**Order.** pb-membership has the largest downstream subtree (mesh-exit ×3, work-doors, pods-verb, meshapp ×3, and through work-doors the ingest-dial pair). It carries the queue's one node-key migration. It also turns LIFT(cmnwlth) from "join the operator's mesh, abstain when the Macs are offline" into a self-contained mesh of two. pb-shell's proof is LIFT(cmnwlth), so pb-shell now reads a real pass. pb-hostkit still precedes pb-membership, which depends on it, so its LIFT proof is judged against pb-lift-instrument's recorded verdict.

**Stranded journeys.**
- pb-daemon-mesh-exit deleted the daemon's `/v1/mesh/kv/*` proxy. `DaemonReplicatedKv`, which serves the CLI's claim, brief, project serve and tools registry, dials that proxy (mesh_kv_client.rs:51, :88), and pb-code-server, which moves it, ran later. pb-atlas-kv moves the client onto cw-rails first, and pb-mesh-exit-mesh depends on it.
- `svrn run` apps are accepted by the daemon's iroh `AppRoutes` (daemon.rs:4355-4378). pb-mesh-exit closed that iroh before pb-meshapp-rehome gave fp-47 its owner. pb-meshapp-apps now runs before pb-mesh-exit-transport, and pb-membership checks the same premise first.
- FIM `/v1/completions` and NES `/v1/edit_predictions` ride the daemon's in-process edit slot (routes_completions.rs, serving-host fim_adapter.rs). pb-serve-program's route list does not name them, and pb-svrn-dials-serve deletes that engine. The premise now stands in pb-serve-program, and svrn's proof covers both routes.

**pb-atlas-kv.** Reproduced at fe2edc7bd: `sovereign tools describe session_state` exits 101 after 39.8 s wall and 38.8 s user, with the panic "Cannot drop a runtime in a context where blocking is not allowed". The backtrace runs reqwest::blocking::ClientBuilder::build ← DaemonReplicatedKv::new ← open_tools_registry ← cmd_describe. It entered with fp-33 (e3c223fcb). `RailsKv` (rails_client/kv.rs) is the same port done right, and its header names this exact panic.

**Falsifiers.**
- The split rows each finishing in under half their stated lift would mean the splits cost more in fixed checks than they saved. Refold them.
- Two HUMAN halts idling the loop for over 2 h each would mean the clean-boundary rule costs more than a worktree build. Revisit with measured build times.
- A resolution session that finds pb-membership needs pb-shell's routes would mean the order goes back.

</details>
