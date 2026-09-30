<!-- ledger -->

**phase-b-76 · 2026-09-30 · pb-mesh-exit-transport (THE FLIP), NEEDS_HUMAN before its atomic commit · director** — this commit
- Needed: three forks the flip's census found unpriced. (1) After the flip cw-rails advertises this node only from registration claims, fixed at register (commonwealth-media origins.rs:258-265; renew moves the deadline only, :290-298), and no row declares svrn's live `build_local_capabilities` output (hosted_corpora, embed_model, storage), so peers would stop choosing this node for knowledge fan-out and ingest. (2) serve registers `cwth/client/0` as `Admit::Members`, closing the non-member path the daemon's acceptor routes to guest (iroh_access.rs:415-438). (3) `commonwealth_core::partition` has no home the daemon can keep after the edge closes.
- Chose:
  - (1) option (a): a new additive row pb-rails-renew-claims ahead of the flip (renew carries optional claims; the one register/renew loop reads a claims source), and the flip declares svrn's capabilities through it, renewing every 10 s to match today's gossip cadence.
  - (2) the flip changes serve's registration to `Admit::MembersElse(GUEST_ALPN)`, which cw-rails already implements (origins.rs:454-469); today's behaviour kept.
  - (3) landed here: `partition` moved whole to `kernel_types::partition`. Callers were svrn's daemon and sovereign-compute, zero in cmnwlth (git grep), so §12 3a's rung 3 (ids and atoms) places it beside `NodeId`. sovereign-compute drops its commonwealth-core dependency.
- Because:
  - Extend, never re-own (§2c): (1) extends the origin registry's existing declaration rather than adding a second advertiser; §4 rule 8 already says cw-rails advertises what origins declare. Option (b) re-registers and mints a new tie mid-flight; (c) is an end-user-visible loss the charter leaves to the operator.
  - Principle 6: the flip must not silently drop an advertisement or close a door; (2) keeps both.
  - Evidence for (3): cargo check of kernel-types, commonwealth-core, sovereign-compute, sovereign-daemon, sovereign-mesh with tests green; TEST(kernel-types, sovereign-compute, commonwealth-core) 384/0; LAYER pass; `cargo xtask boundary-gate` EXIT=1, 11 violations (11 → 11: compute sits inside the cmnwlth closure).
  - Found and sent to phase-c: `rendezvous_owner` hashes with `DefaultHasher`, not stable across toolchains (pc-rendezvous-stable-hash).

<!-- appendix -->

## phase-b-76 · 2026-09-30 — the flip's three unpriced forks: renew carries claims, guest fallback on cwth/client/0, partition to kernel-types

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md at e480bd363, archived at target/ralph/phase-b/pb-mesh-exit-transport-needs-human.phase-b-76.md.

Fork 1 evidence, reproduced: `OriginRegistry::register` sets `e.claims = req.claims.clone()` on the first slot (commonwealth-media origins.rs:258-265); `renew(claim_id, ttl)` only calls `s.claimed.renew` (:290-298); cw-rails' gossip merges `declared_claims()` into its self record every round (commonwealth-rails gossip.rs:179, `merge_declared` origins.rs:183). svrn's peer origin registers with `claims: None` (sovereign-daemon peer_origin.rs:62) and renews every 20 s (`ORIGIN_RENEW_EVERY`, :34). The daemon's own advertisement is rebuilt per 10 s gossip round (sovereign-mesh gossip.rs:125). Cadence chosen: svrn's peer-origin renew at 10 s, so the advertisement is no staler than today; a loopback renew per 10 s costs nothing measurable. Why a separate row: its proof is on cw-rails alone and it is additive, where the flip's proof is the mesh-of-two; declaring svrn's claims before the flip would advertise its corpora under cw-rails' solo key, so the svrn half stays in the flip.

Fork 2 evidence: iroh_access.rs:415-438 routes a non-member CLIENT_ALPN dial to `self.guest`; serve's `registrations_for` (sovereign-serve rails_mesh.rs:398-447) registers `cwth/client/0` with `Admit::Members(Vec::new())`; `Admit::MembersElse(other)` forwards a non-member to the registered `other` ALPN's origin and closes if none (commonwealth-media origins.rs:454-469). Serve names svrn's guest ALPN as data from mesh-reach (mesh_reach::guest::GUEST_ALPN), the shared vocabulary leaf, not svrn's crate.

Fork 3 evidence: `git grep -n "partition::"` outside commonwealth-core: sovereign-daemon newsworthy_host.rs:27/194/199, routes_internal/newsworthy_status.rs:27/248, sovereign-compute distributed_discovery.rs:519, sovereign-mesh tests dst.rs:42/651-652; no site under commonwealth/. partition.rs depends only on `NodeId` (already kernel-types) and std. The package's claim that it "is real cmnwlth code" was false in the sense that matters for placement: cmnwlth owns the file but calls none of it.

What would falsify these: (1) a peer-side consumer that reads svrn's capabilities faster than 10 s, or a renew body size that cw-rails refuses; (2) a guest listener that must not see member-less client traffic under cw-rails (none found: today's daemon does exactly this); (3) a cmnwlth caller of partition appearing in a row not yet landed (none in STATE.md).

</details>
