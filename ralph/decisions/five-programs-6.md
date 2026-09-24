<!-- ledger -->

**five-programs-6 · 2026-09-23 · fp-44 · director** — this commit
- Needed: fp-44 halted on a false premise — its text read the TSV's missing-capability cell ("cw-rails serving /v1/rail/{log,append,live}") as already true; the tree says cw-rails serves /v1/mesh/* only and links no rail crate. Decide the row's shape and the three decisions the package buried in it.
- Chose: split fp-44 into SERVE + FLIP on the queue's own fp-46/fp-47 precedent (fp-44 rescoped = cw-rails builds the /v1/rail surface; new fp-54 = the daemon's sites dial, journals migrate in the same commit). Signer identity does not change (TSV behaviour_delta = none); the ring round stays daemon-side (cw-rails is package-closure clean — a rails-side round would mint cmnwlth→svrn); the unowned participants fold into fp-54; work_donor stays fp-8's.
- Because: §12 D2 names cw-rails the owner ("extend it, do not build a new binary") and the TSV's decision_needed cell is `none`, so the fork is the charter's, not the operator's; the wire-preserving key answer is forced by the row's own behaviour_delta cell plus the ONE loader both processes already share (commonwealth_transport::identity::load_or_generate_node_key; cw-rails lib.rs:128,137).

<!-- appendix -->

## five-programs-6 · 2026-09-23 — fp-44 (daemon→commonwealth-rail, 51 refs) split SERVE/FLIP; premise-3 failure confirmed

<details><summary>reasoning, evidence, package</summary>

The worker's census (ctl/NEEDS_HUMAN.md) was reproduced line by line before deciding:
premises 1-2 hold (routes_rail.rs:35 import, admit :505; rail-core leaf exists and is
in every package's shared-leaf list per `cargo xtask boundary-gate`), premise 3 fails
(cw-rails api.rs:43-64 serves /v1/mesh/* only; no rail dep in its Cargo.toml; fp-6's
commit body 86b03b9b7 says outright "fp-44 extends them, not this row").

Evidence for each sub-decision:
- **Shape (SERVE+FLIP, not one row):** the row's subject is a lifecycle (construction
  with the node key at daemon.rs:3101, MeshRosterSource at :3268, journals under the
  daemon's data dir), not the two stateless verbs fp-6 folded into one row. The
  queue's atomic-row grammar and the fp-20 → fp-46/fp-47 precedent name the split.
- **Key custody:** TSV behaviour_delta = none forbids a re-key (acts are signed at the
  door, routes_rail.rs:371-374/:435, and admit refuses keys the roster does not name,
  :346 — a new signer identity is a wire change). Both processes already load node_key
  through the same loader and cw-rails' identity.rs documents the shared file
  names/formats as deliberate, so rails signing with the project's node identity
  preserves the wire. The deployment detail of pointing rails' rail at the project
  node_key is REVIEW-AFTER: the loader convention is verified, the config wiring is
  fp-44's worker to land and the check to pin.
- **Ring round home:** stays daemon-side over rail-core types, dialing /v1/rail/* —
  forced by package closure (cw-rails: "no sovereign runtime"; sovereign-mesh is
  svrn's), not a judgement call. The replication census + ring-live tests follow the
  proxy.
- **Participants** (mesh_http.rs:655,742,757; bootstrap.rs:1325;
  routes_guest_session.rs:78,137; work_atlas_broadcaster tests — test-only, cfg(test)
  :99): folded into fp-54, one row per pair; work_donor.rs is fp-8's.
- **Appendix line** for the pair said `fp-21`, an id a later queue generation reused
  for the transport split (0fe0f31be's repair diff) — corrected to fp-44 + fp-54.

Falsified if: the SERVE row cannot mount the doors without opening the daemon's data
dir (§4 rule 1 breach — would force co-locating storage earlier than fp-54); or the
FLIP's proxy round cannot keep the replication census green (would reopen the round's
home as an operator question).

REVIEW-AFTER: the node_key config wiring for rails' rail signer (one campaign cycle —
if fp-44's worker finds the shared-identity reading wrong, STOP and repackage; do not
re-key in flight).

</details>
