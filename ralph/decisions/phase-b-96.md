<!-- ledger -->

**phase-b-96 · 2026-10-01 · pb-distribution-ship-gate · seat, adding pre-gate fixes F4-F7 from the release-note census and filing the rest to phase-c** — this commit
- Needed: a read-only census of 18f783f44..30aa81286, drafting the ship gate's release note, listed 24 findings. The seat verified the ones that bear on the gate in the tree at 30aa81286 before acting (principle 4).
- Verified and made pre-gate rows, each a gap Phase B's own split left, the class of F1-F3 (phase-b-85):
  - F4: `svrn atos` and `svrn design` fall through to `print_usage(); exit(1)` (sovereign-cli main.rs:1224); `project design|plan` and `drift accept` print misleading errors; `amend design` and `audit <feature-id>` silently run other verbs. Fails Tier 1's verbs bar as written.
  - F5: .github/workflows/cli-release.yml builds and packages only the three pre-split binaries (:283-285, :295); F1's census reads two lists, not this third.
  - F6: the guest door's TCP bind serves `door_router` unsealed (guest_door.rs:429) while its ALPN twin is sealed (daemon.rs:1650).
  - F7: the on-prem kit renamed main's firm-rag-daemon.service and retires only firm-rag-server.service (install.sh:283-287).
- Refuted, dropped: `svrn ring checkpoint` 404s (it dials `rails_base()`, ring_cmd/mod.rs:339, and cw-rails mounts the route, ring_routes.rs:60); `install.sh --force-config` deleting keys (documented, install.sh:80).
- Filed to phase-c: pc-upgrade-off-mesh-named (flagged as a pre-merge candidate for the operator's O1), pc-removed-env-warn, pc-migration-backups, pc-onprem-absence-messages, pc-bare-404s, pc-docs-after-cut.
- Because: the gate's own bars (Tier 1 verbs, F1's intent, P8's on-prem hardening) fail on F4-F7 as written, so fixing them before the gate is cheaper than recording four misses; the rest change no bar the gate reads.
- REVIEW-AFTER: the ship gate's Tier 1 reading at C.
