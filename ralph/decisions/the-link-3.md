<!-- ledger -->

**the-link-3 · 2026-09-22 · tl-3-dial-measured · seat** — this commit
- Needed: bar `tl-dial-measured` wants the dial's one unknown as measured numbers on the record — rail-core's wasm size, iroh's wasm outcome at the locked pin, and a dial attempt only if it built — with zero product code moving (clause (d)).
- Chose: recorded all three. (a) `probe.wasm` **415,140 bytes, sha256 9a07541e…9831d7**, reproduced BYTE-IDENTICAL from a clean rebuild (cargo/rustc 1.95.0, release profile defaults opt-level=3, no LTO, no wasm-opt) — the inventory's number stands, unmoved. (b) iroh **1.0.2 does NOT build** for wasm32-unknown-unknown — verbatim: `error: failed to run custom build command for 'ring v0.17.14'` / `ring@0.17.14: error: unable to create target: 'No available targets are compatible with triple "wasm32-unknown-unknown"'` (exit 101). (c) the dial attempt is DEAD — (b) did not build. No product code; the row's diff over sovereign/crates and commonwealth/crates is empty.
- Because: a number reproduced byte-for-byte from clean is the strongest form of "did not move", and a pin that cannot reach the browser is a measurement, not a premise — RING_APP_LIBRARY.md:546-553's browser claim does not hold at our pin, exactly as the inventory recorded.

<!-- appendix -->

## the-link-3 · 2026-09-22 — the dial's three numbers: wasm size reproduced, iroh refused at the pin, dial attempt dead

<details><summary>reasoning, evidence, package</summary>

**The fork.** The row's clause (c) is conditional on clause (b): ONE throwaway page
dialling a live node's guest channel through a relay, IF iroh builds for
wasm32. The measurement forked at (b) and took the dead branch: ring 0.17.14's
build script refuses the target before any iroh code compiles, so there is no
iroh-in-browser runtime to dial WITH. No page was built, no grant was minted,
no live node was touched.

**The three numbers (re-measured 2026-09-22):**

- **(a) rail-core's wasm size: 415,140 bytes — unchanged, reproduced
  byte-identical.** Clean rebuild (`cargo clean`, then `cargo build -p probe
  --target wasm32-unknown-unknown --release`, RUSTFLAGS
  `--cfg getrandom_backend="wasm_js"`) in the inventory's isolated copy
  `target/ralph/wasm-probe/` (rail-core+oplog+kernel-types, workspace-hack
  dropped, getrandom =0.3.4 wasm_js, ed25519-dalek 2.2.0, blake3 1.8.5;
  rail-core is READ-ONLY, invariant `c3fed9c3`, so its sources cannot have
  moved). sha256
  `9a07541e0c764a68a5a5c437931ffd48b7ac1b429871754e00be57150b9831d7` —
  identical to the inventory's 2026-09-21 hash. Tool: cargo/rustc 1.95.0,
  release profile DEFAULTS (opt-level=3, no LTO), no wasm-opt. Build 8.55 s.
  The artifact exports `the_link_four_steps` running admit+digest over a
  signed 3-actor 6-op journal — the checkpoint paths, not a stand-in.
- **(b) iroh at the LOCKED version (Cargo.lock: iroh 1.0.2, ring 0.17.14):
  NO.** Re-ran `cargo check --target wasm32-unknown-unknown` in the
  inventory's `target/ralph/iroh-probe/` (its own lock pins iroh =1.0.2 and
  resolves ring 0.17.14, same as the workspace). Outcome verbatim, exit 101:
  `error: failed to run custom build command for 'ring v0.17.14'` —
  `warning: ring@0.17.14: error: unable to create target: 'No available
  targets are compatible with triple "wasm32-unknown-unknown"'`. The failing
  layer is ring, iroh 1.0.2's crypto dependency, inside its build script
  (cc-rs → host clang has no wasm32 backend), before any iroh code compiles.
- **(c) the dial attempt: DEAD** — not run, by the row's own conditional.

**What would falsify each:**

- (a) any toolchain move (rustc ≠ 1.95.0) or a rail-core/kernel-types/oplog
  source change moves the hash — the falsifier is one clean rebuild + sha256;
  rail-core's read-only invariant keeps the likeliest mover at the toolchain.
- (b) a pin move — iroh > 1.0.2, or ring at a version with a wasm32-clean
  build script or a pure-Rust fallback — reopens the question and REVIVES
  clause (c); `git grep 'name = "ring"' Cargo.lock` is the tripwire.
- (c) cannot be falsified while (b) stands; its verdict would have been the
  layer that answered or failed against a live node's guest channel.

**Method notes for the next re-measurer:** rustc 1.95 rejects the unquoted
cfg form — RUSTFLAGS must carry the embedded quotes
(`--cfg getrandom_backend="wasm_js"`); sccache as RUSTC_WRAPPER was cleared
for both probe builds. Both probe directories are the inventory's own
instruments, reused unchanged (principle 11).

**Clause (d) evidence:** `git diff --stat -- sovereign/crates
commonwealth/crates` for this row is EMPTY (checked at commit time). The
summary-verify commit `33d554a87` atop this session's start was the seat
session's carried work, committed on the operator's instruction before this
row opened — it is not this row's diff.

**Checks:** CLEAN exit=0 (41G under the 256G ceiling, cache kept); DOCS
exit=0; decisions ledger re-rendered fresh.

</details>
