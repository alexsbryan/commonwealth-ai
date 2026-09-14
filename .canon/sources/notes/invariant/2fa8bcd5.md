# Disk on this macOS host hit 100% (327Mi free of 926Gi) during the nc-kill-chain-proof rung on 2026-08-20, and cargo build REPORTED EXIT 0…

Disk on this macOS host hit 100% (327Mi free of 926Gi) during the nc-kill-chain-proof rung on 2026-08-20, and `cargo build` REPORTED EXIT 0 while producing an 864-byte `target/debug/sovereign-cli-dev` — the strip step failed with "LLVM ERROR: IO failure on output stream: No space left on device" as a WARNING, and cargo printed "Finished". A green build that emits a truncated binary is the exit-0-but-wrong failure this workspace's principle 5 is about; check `df -h .` before trusting a build here.

`target/` was 142G for this one repo. What is safe to delete, both fully regenerable and neither used by the definition-of-done sweep:
  - `target/sovereign-test-scoped` (36G) — the alternate target dir `sovereign-test.sh --package/--changed` builds into. Costs peers one cold scoped build.
  - `target/tests/trybuild` (7.8G) — trybuild's compile-fail scratch. Do NOT delete while a trybuild test is running: deleting it mid-run made `corpus-engine::evidence_reds::evidence_has_exactly_one_door` TIMEOUT at 180s, which reads as a red gate and is not one. It passed on re-run.
Together those freed 44G. `target/debug/deps` (80G) has no safe partial GC.
