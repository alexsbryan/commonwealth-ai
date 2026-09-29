<!-- ledger -->

**phase-b-51 · 2026-09-29 · the operator's rulings on phase-b-50 and phase-b-49, by ARCH_PRINCIPLES · operator** — this commit
- Needed: phase-b-50 left one operator question, whether the flip keeps a node on the mesh after a reboot. phase-b-49 asked the operator to confirm the director's reading of phase-b-43's falsifier. The seat's principle review of phase-b-50 found five rulings with a sharper answer.
- Chose (operator: "Ok approved", 2026-09-29):
  1. `svrn mesh up` installs and enables a cw-rails user unit, written by install-service's writer, which is shared through host-kit. `svrn status` and doctor name its state (pb-mesh-exit-transport).
  2. phase-b-49 is confirmed. FIVE_PROGRAMS §2c defines "engine-internal" as spoken only by ingest's own code, so a type a cross-program port names is that port's vocabulary. The threshold that moved with `ConvBucket` goes back to the engine in phase-c (pc-tiered-classify-back).
  3. The turn wire fields follow the `intent`/`mode` precedent, sovereign-contracts types/turn.rs:446-468 (pb-bench-dials).
  4. One sibling list decides the release binaries, which install.sh reads (pb-distribution).
  5. One RAPTOR implementation, ingest's, with svrn's store as the sink (pb-cli-llm-ingest-move).
  6. The pinned-pod snapshot is serve's data, and serve alone writes it (pb-mesh-dissolve).
  7. Host load is recorded as a covariate on the speed bar (pb-serve-distributes).
  8. ralph waits out a plan usage limit instead of counting a stall (a separate code commit).
- Because:
  - Principle 12. Each program owns its own restart and its own data. A unit started by sovereign.service or installed by svrn's setup verb would hand cw-rails' lifecycle to svrn. The pod verb writing serve's file is the same fault.
  - Principle 6. A node silently off the mesh, or a never-ran session parked as a blocked row, is a substituted verdict.
  - Principle 8. One unit writer, one sibling list, one RAPTOR, and one snapshot accessor.
  - Principle 10. One list is structural; three lists held together by a pin test only agree by convention.
  - Principle 11. The turn wire already has a precedent for a parameter with no wire form, and corpus-index already holds five ingest ports.
  - Principle 5. A load precondition taught nothing; a covariate records load and gates nothing.
  - Boundary gate: 23 at 4e81c1b40. This commit changes no Rust.

<!-- appendix -->

## phase-b-51 · 2026-09-29 — operator rulings on the pre-flight

<details><summary>reasoning, evidence, package</summary>

Evidence for the reboot ruling: the only enabled user units are sovereign.service and sovereign-toolbox.service, and there is no cw-rails unit. The unit writer is sovereign-cli-daemon install_service_cmd.rs (91 lines) and what it calls. host-kit's rung-4 test (a mechanism each program's binary needs about itself; every path supplied by the caller; no store) admits a service-unit writer, and host-kit's cap is `max_code_lines = 2500` (quality/ARCH_LAYERS.toml:1145).

For the phase-b-49 caveat: `classify`/`classify_note` decide the bucket boundaries, and svrn's incremental path calls one directly at conv_tiered_provider.rs:537. Keeping one decider is correct now, and the leaf holding a threshold is recorded for phase-c rather than reworked mid-row.

</details>
