<!-- ledger -->

**browser-dial-1 · 2026-09-22 · REVIEW-build-browser-dial-inventory · seat** — this commit
- Needed: `the-link-3` (b) recorded a build failure — "iroh 1.0.2 does NOT build for wasm32-unknown-unknown" — from the Mac, and the browser-dial order's Premise 1 says that does not reproduce on the Halo. A ledger that carries only one host's outcome is a host fact recorded as a pin fact (ARCH 7). Bar `bd-wasm-build-reproduced` clause (b) requires the Mac outcome to stand BESIDE the Halo one, with each host's instrument named, never replaced.
- Chose: reproduced the probe under `target/ralph/browser-dial/iroh-probe/` at `iroh = "=1.0.2"` (manifest kept, own `[workspace]`), on the Halo's `sovereign-vulkan` toolbox: `cargo check --target wasm32-unknown-unknown` → `Compiling ring v0.17.14` … `Finished `dev` profile [unoptimized + debuginfo] target(s) in 15.90s`, exit 0. Recorded it beside the Mac's verbatim refusal, named the conditions for both, and named the falsifier. No product code moved — `git diff` over sovereign/crates and commonwealth/crates for this row is empty.
- Because: the Mac's `No available targets are compatible with triple "wasm32-unknown-unknown"` is cc-rs reporting that its host C compiler cannot target wasm32; on the Halo, clang 21.1.8 lists `wasm32` in `--print-targets` and ring's build script compiles. The failing input for the claim is a host whose clang cannot target wasm32, which reproduces the Mac refusal verbatim. A pin that is a host fact is worth recording as exactly that — and the pin itself (iroh 1.0.2, ring 0.17.14) is unchanged on both.

<!-- appendix -->

## browser-dial-1 · 2026-09-22 — the tl-3 (b) build outcome was a host fact, not a pin fact; both records now stand

<details><summary>reasoning, evidence, package</summary>

**The fork.** `the-link-3` (b) closed the dial work on "iroh 1.0.2 does not
build for wasm32" (Mac). The handoff session then probed the same pins on
the Halo and got a build. One of the two is not durable: either the pin
moved (it did not — the lock still pins iroh 1.0.2 and ring 0.17.14) or the
outcome was never about the pin. The row's job is to reproduce the Halo
outcome under `target/` with its instrument named, and correct the ledger by
ADDING it, never by replacing the Mac record.

**The Mac outcome, verbatim (as recorded by `the-link-3`, 2026-09-22, host =
the operator's Mac; instrument = `cargo check --target wasm32-unknown-unknown`
in an isolated copy pinning iroh 1.0.2 / ring 0.17.14), exit 101:**

```
error: failed to run custom build command for 'ring v0.17.14'
warning: ring@0.17.14: error: unable to create target: 'No available targets are compatible with triple "wasm32-unknown-unknown"'
```

**The Halo outcome, verbatim (this row, 2026-09-22), exit 0:**

```
   Compiling ring v0.17.14
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 15.90s
```

**Conditions named — Halo (the instrument with its parts):**

- Host: the `sovereign-vulkan` toolbox (podman image
  `docker.io/kyuz0/amd-strix-halo-toolboxes:vulkan-radv`, container
  `c0142767d629`), Linux.
- Rust: `rustc 1.95.0 (59807616e 2026-04-14)`, `cargo 1.95.0`.
- Target std: `wasm32-unknown-unknown` present in `rustup target list
  --installed`.
- C compiler: `/usr/bin/clang 21.1.8 (Fedora 21.1.8-4.fc43)`, whose
  `--print-targets` lists `wasm32 - WebAssembly 32-bit`; `wasm-ld` present.
  THIS is the part the Mac lacked.
- Probe: `target/ralph/browser-dial/iroh-probe/Cargo.toml` (never committed),
  `iroh = "=1.0.2"` with an empty `[workspace]`; its own lock resolved iroh
  1.0.2, ring 0.17.14, iroh-base 1.2.0. (The workspace lock pins iroh-base
  1.0.2; the probe's fresh lock takes 1.2.0 through `iroh-dns`'s `^1.2.0`.
  The difference does not touch ring, which is the failing layer, and the pin
  under test — iroh 1.0.2 — is exact.)
- Command: `cargo check --manifest-path
  target/ralph/browser-dial/iroh-probe/Cargo.toml --target
  wasm32-unknown-unknown`, through `scripts/with-cargo-lock.sh`.

**Conditions named — Mac:** host C compiler (Xcode clang) with no wasm32
backend, so cc-rs could not map `wasm32-unknown-unknown` to a target at all;
iroh 1.0.2 and ring 0.17.14 identical to the Halo's.

**What would falsify this entry:** a host whose clang cannot target
wasm32 reproducing the Mac refusal, verbatim — which is the Mac's own run.
The claim is not "iroh builds everywhere"; it is "the tl-3 (b) refusal was
the Halo's C-compiler condition, not the pin, and the pin is unchanged".

**Clause (d) evidence:** `git diff --stat -- sovereign/crates
commonwealth/crates` for this row is empty; the only committed changes are
this ledger entry (plus its render), the row's `BASE` stamp in
`ralph/next/browser-dial/order.md`, and the queue mark.

**Checks:** CLEAN exit=0; DOCS exit=0; `ralph-decisions.py --check` current.

</details>
