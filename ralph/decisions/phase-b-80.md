<!-- ledger -->

**phase-b-80 · 2026-09-30 · pb-mesh-exit-transport (the flip) · director** — this commit
- Needed: the flip deletes founding, joining and admission, but the daemon starts only through `start_daemon(mesh, node_id)` (daemon.rs:2481), reached from resume/found/join (daemon_cmd/mesh_resume.rs:10). The row named no replacement boot, no fate for the `[discovery] join_key` + `seed_addrs` fleet join, and no way to land ~4,400 changed lines atomically across sessions.
- Chose:
  - Boot (fork 1 (a)): `start_daemon` takes no `Mesh` and runs at every boot. The daemon's membership and peer transport are what the composition root injects through the existing seams `FabricSeed.membership` and `FabricSeed.peer_transport` (sovereign-mesh fabric.rs:206, :216): in sovereign-stock, cw-rails' roster and `RailsTransport`, the swap `mesh_ports` (main.rs:181) already makes for serve. The desktop's in-process daemon (daemon_services.rs, `Launch::Desktop`) gets the same injection. sovereign-mesh's in-process `FabricPart.mesh` readers (16 non-test lines: fabric.rs 6, gossip.rs 6, membership.rs 3, ring_sync.rs 1) read the port or are deleted with gossip and admission, in the flip. No in-process `Mesh` mirror (fork 1 (b), a second roster copy).
  - Config join (fork 2 (a)): a config naming `[discovery] join_key` is refused at boot by name, with a pointer to `svrn mesh join <invite>`. ENTERPRISE_FLEET_DEPLOY.md moves to create-then-invite in the same commit. The tailscale entrypoint moves to `MESH_INVITE` + `svrn mesh join` in the same commit; it never used the config join (it writes `[mesh] seed_addrs`, a section `SetupConfig` does not parse, and no `join_key`), so its node founds a solo mesh today.
  - Landing (fork 3 (a)): the flip is built in a detached worktree across sessions (phase-b-75/-79 mechanics), WIP committed there every session, gated there, and landed on `cut` as one commit when the main tree has no uncommitted code. The row stays `[~]` and each progress note names the worktree's HEAD.
- Because:
  - Fork 1: principle 8 and FIVE_PROGRAMS §4 rule 8. cw-rails' roster is the node's one roster after the flip; the seams exist and serve already takes them; a mirrored `Mesh` is the copy pb-mesh-exit-mesh would have to delete.
  - Fork 2: the config join is `perform_join`'s plaintext `relay=` path (mesh_resume.rs:52-60 builds `DeepLink::Join { encrypted: false, relay_hint: Some(seed) }`), which the row already retires under the operator's phase-b-36/37 decision to drop plaintext meshes, refused by name. This names that delta; fork 2 (b) adds an unpriced config key, and (c) contradicts phase-b-36.
  - Fork 3: this host runs the deployed daemon from `target/debug` of this tree, so a half-flipped `cut` is a live hazard, and a default-off dual path (3 (b)) is a principle-8 copy with a ledger row.
- REVIEW-AFTER: the flip's landing. Fork 2 changes what a `join_key` config does at boot (it used to join and now it exits with a pointer). The row's plaintext drop covers that path, but the row never named it. If the operator treats the config-driven fleet join as a separate end-user promise, this is falsified, and the fix is fork 2 (b) in a row of its own. Fork 1 is falsified if a `FabricPart.mesh` reader needs founding-time state that cw-rails' roster does not carry. Fork 3 is falsified if the worktree's rebase onto `cut` costs more than a session.

<!-- appendix -->

## phase-b-80 · 2026-09-30 — the flip boots on injected rails ports, refuses the config join by name, and lands from a worktree

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md (fourth flip session, after 146d8fd03).

Reproduced:
- `start_daemon(&self, mesh: Mesh, node_id: NodeId)` at daemon.rs:2481; `try_resume` :977, `create_mesh` :1236, `join_mesh` :1438; `resume_or_bootstrap_mesh` called at daemon_cmd/boot.rs:1056.
- mesh_resume.rs:47-60: the configured joiner builds `DeepLink::Join { join_key, relay_hint: Some(seed), encrypted: false, iroh_dial: None }` for each seed and calls `join_mesh`.
- `FabricSeed.peer_transport: TransportReader` (fabric.rs:206) and `FabricSeed.membership: Option<Arc<dyn MembershipReader<Dial = PeerContact>>>` (:216); `FabricPart.mesh: Arc<RwLock<Mesh>>` (:231). sovereign-serve rails_mesh.rs:245 `mesh_ports(roster: RailsRoster, …)`, sovereign-stock main.rs:181 `mesh_ports`.
- `SetupConfig` has `discovery: DiscoverySection` (setup_config.rs:90) and no `mesh` field; `seed_addrs` and `join_key` are declared only at setup_config.rs:1348, :1357. entrypoint-tailscale.sh:159-160 writes `[mesh] seed_addrs` and names no join key. The package's claim that the entrypoint uses the config join is false; the fork still applies to ENTERPRISE_FLEET_DEPLOY.md:48-62.
- BOUNDARY 7 (`cargo xtask boundary-gate` from corpus-engine/, FAILED, 7 violations) at ecf15ae68.

Rejected: 1 (b) is an in-process roster mirrored from cw-rails, a second decider for membership. 2 (b) is a new `svrn mesh up` config key that no row prices. 2 (c) keeps the plaintext path the operator dropped. 3 (b) is a default-off dual path. 3 (c) risks losing ~4,400 uncommitted lines at a session boundary.

</details>
