<!-- ledger -->

**five-programs-66 · 2026-09-25 · fp-solo-clients port leak + solo write window · operator** — this commit
- Needed: after fp-solo-clients closed, the seat found an orphaned test cw-rails on the host's default port 9747. Its data root was already deleted, and it bound an iroh endpoint on the default n0 relays. Two things caused it. `ensure_rails` starts a bare `cw-rails run`, which binds cw-rails' own `listen` (default 9747), not the port the daemon probes. And the tests that boot the daemon binary never pin `rails_base`. The e2e also measured an up-to-2 s window in which an acknowledged KV write is not yet durable.
- Chose:
  - (1) One more five-programs row, fp-solo-hermetic, runs before HUMAN-phase-b.
  - (2) `ensure_rails` passes the port it probes, as `cw-rails run --listen <port>`.
  - (3) cw-rails exits when the lock file it claimed is unlinked or replaced, and it names why.
  - (4) Every test that boots a daemon binary pins `rails_base` to an ephemeral port and puts `CW_RAILS_DIR` under its temp root, and a census fails the next test that does not.
  - (5) The solo write window becomes a staged phase-c row, pc-solo-durable. It is not "by design".
- Because:
  - Principle 8: a port with two deciders diverged into a plausible absence plus a leaked process.
  - Principle 12: cw-rails owns its root and its lifetime, so it reaps itself when its root is gone. Neither the daemon nor a test harness does it for cw-rails.
  - Principle 10: once your own cw-rails serves 9747, an unpinned test writes into your real store. So hermeticity is a census, not a convention.
  - Principle 6: a cw-rails serving from a deleted root acknowledges writes it cannot keep.
  - Boundary gate: 51, unchanged. There is no code in this commit.

<!-- appendix -->

## five-programs-66 · 2026-09-25 — ensure_rails passes the port it probes; cw-rails exits on root loss; binary-boot tests pinned; solo write window to phase-c

<details><summary>reasoning, evidence, package</summary>

**The orphan.** pid 1703064 was `target/debug/cw-rails run` with `HOME=/tmp/.tmpuUcSAi/founder/home`. It started at 19:54:58Z, inside fp-solo-clients' TESTALL (sc-testall.log 12:57 local). Its parent was the toolbox's conmon, because the test process that spawned it had exited. It listened on 127.0.0.1:9747 and :44673, and its only open files were `rails.log` and `rails.lock`, both "(deleted)". The deployed daemon (pid 1882, started 2026-09-23, before fp-88) had no connection to it. The seat saved its log and stopped it, which freed 9747. Two tests build `founder/home` and boot the binary with no `rails_base`: sovereign-daemon tests/main/admin_join_serves_venues_e2e.rs:94 and sovereign-cli-daemon src/setup_cmd/terminal/join_child/tests.rs:88.

**The mismatch, reproduced.** The seat ran a local-only daemon (real Qwen3.5-0.8B and Qwen3-Embedding-0.6B) with `rails_base` pinned to 127.0.0.1:43383 and no rails.toml. `ensure_rails` logged "brought up pid 1787059 but http://127.0.0.1:43383 did not answer within 10.1s", while that cw-rails' own log read `serving api=127.0.0.1:9747`. `ensure_rails` (rails_client.rs:118-119) passes only `run`. cw-rails' `run` reads `config.listen` (cli.rs:190), and only `media` parses `--listen` today (cli.rs:59,92).

**The local-only journey, measured.** With cw-rails beside the daemon: `/v1/models` 200, one lock holder, `ensure_rails` BroughtUp in 257 ms. With the daemon copied away from cw-rails: still 200, 0 holders, and the named absence at warn. `list_models` (routes_inference.rs:749) reads the node's manifest whenever it has a local engine and falls back to the store only on an engine-less node. So five-programs-63's 503 was the engine-less case, and fp-solo-clients' store round-trip was the right discriminator.

**The write window.** From fp-solo-clients' e2e (30667d2d7): a write cw-rails had acknowledged was gone after a kill milliseconds later. The row was durable only after the pump's next 2 s tick (kv.rs PUMP_INTERVAL). On a solo node there is no peer copy.

</details>
