<!-- ledger -->

**phase-b-35 · 2026-09-27 · pb-rails-reach → the guest dialer splits to pb-reach-guest and holds the iroh guest machinery in `mesh-reach`; the IP overlay moves to the flip · director**
- Needed: pb-rails-reach's NEEDS_HUMAN named two premises the tree contradicts. The row priced the guest dialer as "≤134 moved from guest_tunnel", but guest_tunnel.rs:38-41 stands on commonwealth-transport's `build_relayed_endpoint`, `parse_dial_string`, `HttpBridge`, `RelayConfig` and `GUEST_ALPN`, and the leaf cannot name commonwealth-transport. The row's proof asked for "with iroh disabled on both, the same request answers over the IP overlay" on a cw-rails-founded mesh, but cw-rails founds every mesh `require_encryption` (found.rs:7-9, :108) and gossips and joins with no overlay address on purpose (join.rs:168-172, gossip.rs:471, acceptor.rs:113).
- Chose: (1) the guest dialer's machinery moves into `mesh-reach` behind a `guest` feature, with commonwealth-transport re-exporting it at its old paths (the package's option A), in a new row pb-reach-guest split from pb-rails-reach because its proof differs (a non-member's dial, not a member's reach). pb-serve-ranks and pb-mesh-dissolve depend on it. (2) The IP overlay leaves pb-rails-reach for the flip (pb-mesh-exit-transport), where the daemon's plaintext posture and its internal listener move as one piece (the package's option A). pb-rails-reach's proof is iroh only, which is what a cw-rails-founded mesh permits.
- Because: FIVE_PROGRAMS §4 rule 8 already places the guest dial in `mesh-reach` (phase-b-30), so only the size premise was false, and a second bridge beside `HttpBridge` is what principle 8 forbids. The trial below shows the machinery needs no workspace crate, so it passes the leaf's falsifier as phase-b-33 restated it. The overlay cannot be proven where the row put it, since the mesh's own policy forbids plaintext there. Boundary gate: 41 red, EXIT=1, unchanged by the trial.

<!-- appendix -->

## phase-b-35 · 2026-09-27 — the guest dialer is mesh-reach's `guest` feature in its own row; the IP overlay goes to the flip

<details><summary>reasoning, evidence, package</summary>

The package was `ralph/next/phase-b/ctl/NEEDS_HUMAN.md` (untracked, removed by this resolution). Its facts, reproduced:

- `found.rs:7-9` "The mesh is founded encrypted (`require_encryption`)"; `found.rs:125` asserts it.
- `join.rs:168-172`: `joining_node_addresses: Vec::new()`, commented "Deliberately empty ... this daemon is reachable by key or not at all". `gossip.rs:471` and `acceptor.rs:113` likewise send `addresses: Vec::new()`.
- `sovereign-mesh/src/guest_tunnel.rs:38-41` imports `build_relayed_endpoint, parse_dial_string, Endpoint, HttpBridge, RelayConfig, SecretKey, GUEST_ALPN` from `commonwealth_transport::iroh`.
- The daemon composes `RoutedTransport::with_required` at daemon.rs:445.

**Fork 1, the guest dialer.** Options were (A) move the machinery into the leaf, (B) a cw-rails guest door with an ephemeral key per dial, which re-opens phase-b-30 Group 3 (b)'s rejection, and (C) defer it to its consumers, which only moves this same question onto pb-serve-ranks. FIVE_PROGRAMS §4 rule 8 already says the guest dial lives in `mesh-reach`; the row's "≤134 moved" counted guest_tunnel's own lines and not what it stands on. So the design stands and the price is corrected.

Trial (at bdacb4346, compile, reverted): moved iroh.rs:54-71 (`relay_pin_active`), :85-101 (`GUEST_ALPN`), :133-646 (`ring_crypto_provider` through `parse_dial_string`, including `RelayConfig`, `build_relayed_endpoint`, `build_relay_only_endpoint`, `configured_proxy_redacted`, `HttpBridge`) and :875-961 (`copy_count`, `PumpSide`, `pump`) into `mesh-reach/src/guest.rs` (646 lines with a 7-line header) behind `guest = ["dep:iroh", "dep:tokio", "dep:rustls", "dep:hex", "iroh/unstable-custom-transports", "iroh/test-utils"]` and `iroh-relay-only = ["guest"]`; commonwealth-transport's `iroh` feature enables `mesh-reach/guest` and re-exports every moved pub item, `pump`/`copy_count`/`PumpSide` as `pub(crate) use`. Results: `cargo check -p commonwealth-transport --features iroh,iroh-relay-only -p sovereign-mesh` EXIT=0; `--all-targets` fails only on iroh.rs unit tests of the moved private helpers `parse_relay_mode` and `redact_userinfo` (7 sites), which move with them; `cargo xtask layer-gate` pass; `cargo xtask boundary-gate` 41, EXIT=1, unchanged. The moved region names no `crate::` item and no workspace crate, only iroh, tokio, rustls, hex and tracing. Raw: target/ralph/phase-b/trials/t-reach-guest.txt and t-reach-guest.guest.rs. Iroh's feature set in the leaf must match commonwealth-transport's exactly, or cargo-hakari lists iroh in workspace-hack (commonwealth-transport Cargo.toml's `[features]` comment).

The cost, named: the leaf then carries iroh endpoint mechanism behind a feature, not only vocabulary. FIVE_PROGRAMS §12's falsifier sentence named "kernel-types, iroh and workspace-hack", which phase-b-33 had already restated as the workspace budget, so this commit brings that sentence in line.

**Fork 2, the IP overlay.** Options were (A) move it to the flip, (B) build a `--overlay` listener on unencrypted meshes here, with admission by mesh proof and no identity stamping, and (C) (B) with `Admit::Any` registrations only. (B) and (C) each need an admission rule for plaintext callers on registered `Members` prefixes that no document or code states (commonwealth-media origins.rs `forward_for` admits by verified key; the daemon admits plaintext internal calls as `Principal::Anonymous` or by mesh proof, internal_principal.rs:43-55). The flip is the row that moves the daemon's plaintext posture, so it owns that rule. Nothing user-visible changes before the flip: the daemon keeps serving its overlay until then.

Falsified if: the pb-reach-guest move needs any workspace crate in `mesh-reach` beyond kernel-types and workspace-hack, or turns LAYER red (then the leaf is a mechanism and the fork goes back to the operator); or a caller of `RailsTransport` before the flip needs a class answered over IP on a cw-rails-founded mesh.

</details>
