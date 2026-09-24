<!-- ledger -->

**five-programs-21 · 2026-09-24 · fp-9 (membership bootstrap has no serving owner) · director** — this commit
- Needed: fp-9's worker halted before editing. The row's named moves (mDNS and disk-free become cw-rails reads, the join-key half becomes a leaf) leave founding, joiner bootstrap, invite minting and admission in the daemon, so the daemon→commonwealth-discovery edge would not close.
- Chose: park fp-9 behind a new operator row, HUMAN-fp9-membership-owner, placed last with three options and a recommendation. This follows the fp-7 and fp-58 precedent. The row's dead "mint a leaf" arm is struck: mesh-join-vocab already holds both pure functions, so that half is now a repoint that rides with the operator's answer. No code changed. Boundary gate FAILED at 62 violations (`RALPH_QUEUE=five-programs scripts/ralph-check.sh boundary`, at ce4e2aeac).
- Because: both ways of closing the edge are the operator's under the charter. One grows a surface that commonwealth-rails disclaims by name (five-programs-20 already sent disclaimed surfaces to the operator, principle 11) and changes what a lone svrn daemon can do. The other needs an `[[exception]]` row. Parking keeps the loop moving, and `current()` now serves fp-10.

<!-- appendix -->

## five-programs-21 · 2026-09-24 — fp-9 parked on the owner of mesh founding and admission

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp9-20260924.md. I reproduced it at ce4e2aeac:

- `grep -rn commonwealth_discovery sovereign/crates/sovereign-daemon/src` gives daemon.rs:17 (mdns), :18, :2164-2165; mesh_admin.rs:23, :577 (disk-free); mesh_http.rs:1589, :1607; and a tracing filter string at bin/sovereign-daemon.rs:142. The `membership::` calls are daemon.rs:1294 `init_mesh_with_identity`, :1565 `validate_join_key_format`, :1585 `init_mesh_with_node_id`, and mesh_admin.rs:723 `accept_join_with_identity`. These match the package.
- Across the repo, only commonwealth-discovery/membership.rs, sovereign-mesh/join.rs, and the two daemon files call `accept_join_with_identity`, `init_mesh_with_identity` or `MdnsDiscovery::new`. Rails uses `init_mesh` only in its tests. So the svrn daemon is the "full daemon" that commonwealth-rails lib.rs:26 says founds and grows a mesh. There is no third-home option (c).
- commonwealth-discovery/src/mdns.rs:84 registers the service. MdnsDiscovery advertises as well as browses, so turning it into a read would drop the advertisement.
- New fact: mesh-join-vocab/src/join_key.rs:17 and :22 already define `hash_join_key` and `validate_join_key_format` (3a). The row's leaf extraction is done, so what remains of it is a repoint worth no gate delta alone.
- commonwealth-rails/Cargo.toml:20 already depends on commonwealth-discovery. Option (a)'s closure cost is therefore about zero. Its real cost is behaviour: the security decider moves to another process.

Why I did not decide the fork: (a) reverses two recorded disclaimers in rails and means a lone daemon can no longer found a mesh, which the charter says is end-user-observable and the operator's call. (b) is an `[[exception]]`, also the operator's call. (c) removes mDNS, a behaviour removal. I recommend (b) as the interim until finish condition 2 takes `Mesh` out of the daemon (fpcore package item 10). The recommendation is written on the HUMAN row.

Residual risk, not acted on because it is out of scope: fp-42 and fp-47 are also dep-ready, carry scout findings that are still unresolved, and are not behind a HUMAN row. When the loop reaches either one it will halt again the same way.

Falsified if the operator's answer or a later census shows a process other than the svrn daemon already founding or admitting (then option (c), dialing that process, existed and parking was unnecessary), or if the mesh-join-vocab repoint alone drops the edge (it would not while daemon.rs:1294 remains).

</details>
