<!-- ledger -->

**five-programs-62 · 2026-09-25 · fp-cond2-b2, fp-cond2-c · director** — this commit
- Needed: fp-cond2-b's join child (a `MeshAdmin` assembly) answers 404 on `/v1/mesh/venues`, because `start_daemon` mounts `mesh_router` only when `serves_host_surface()` (daemon.rs:3495), which is false for `MeshAdmin` (daemon_services.rs:399-401), and `Launch::AdminJoin` assembles `mesh_admin()` (daemon_services.rs:633-634). fp-cond2-c's `find_holders` had nothing to poll.
- Chose: the package's option (i), as a new row fp-cond2-b2 ahead of fp-cond2-c: a venues-only router in mesh_http.rs that `mesh_router` merges (one route declaration), mounted in `start_daemon`'s `else` arm for `MeshAdmin`. fp-cond2-c's proof locates `<target>/debug/sovereign-daemon` through daemon_bin.rs's existing resolution and fails naming the build command when absent; its checks run TEST(sovereign-daemon) first so the bin is fresh. The founder fixture is the package's measured one (terminal-class `run`, join link plus `&relay=127.0.0.1:<internal_port>`).
- Because: it is the one read the wizard needs and exposes no mutating route on the child (option ii would expose create/join/leave/rotate/forget for the child's lifetime); option iii is a second wire for a fact the daemon already serves (principle 8). It is the falsifier five-programs-61 named ("the child cannot keep serving `/v1/mesh/venues`"), met by the smallest code change.

<!-- appendix -->

## five-programs-62 · 2026-09-25 — the admin-join child mounts `/v1/mesh/venues` alone; fp-cond2-c's proof uses the prebuilt sibling bin

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fpcond2c-20260925.md. Reproduced at f8f19af1d by reading the tree: daemon.rs:3495 `if self.services.serves_host_surface()` guards `mesh_router` (and every other host router); daemon_services.rs:399-401 `!matches!(self, Self::MeshAdmin(_))`; daemon_services.rs:632-634 `Launch::Verb { .. } | Launch::AdminJoin { .. } => … LaunchParts::Admin => Ok(DaemonServices::mesh_admin())`; mesh_http.rs:49 is the venues route inside `mesh_router`. The package's live probe (404 on the joiner's client and internal ports, 200 `{"venues":[]}` on the founder) was not re-run; the code path above yields it deterministically. Boundary gate at f8f19af1d: 51 violations, EXIT=1.

The `Launch::Verb` one-shots (`svrn mesh create|join` with no daemon) also assemble `MeshAdmin` and will gain the same read route while they run. It is loopback-guarded, read-only, and they exit after the verb; that is not an end-user-visible change.

Decision 2 of the package: a `--package sovereign-cli-daemon` run cannot build another package's `[[bin]]`, so the test resolves the bin the way `daemon_bin::locate` (sovereign-cli-daemon/src/daemon_bin.rs:17, honours `SOVEREIGN_DAEMON_BIN`) does, and the row orders TEST(sovereign-daemon) before TEST(sovereign-cli-daemon), which builds the bin into target/debug. An absent bin is a named failure, never a skip (principle 5, 6).

What would falsify this: the venues-only mount makes the child answer a route other than `/v1/mesh/venues` beyond the base routers (check `mount_names` at `tracing=debug`); the joiner's venues list stays empty with the founder online, so `find_holders` still cannot see holders (then the gap is `peer_inference_endpoints` on an admin assembly, and a new package is owed); or TEST(sovereign-daemon) does not refresh target/debug/sovereign-daemon under the test script's engine.

</details>
