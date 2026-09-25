<!-- ledger -->

**five-programs-61 · 2026-09-25 · fp-cond2-b, fp-cond2-c · director** — this commit
- Needed: fp-cond2-b's admin launch, as written, spawns a process with no listener. `EmbeddedDaemon` binds its client API only inside `start_daemon` (daemon.rs:2896), which only try_resume, create_mesh_with and join_mesh reach; `expose_client_api` (daemon.rs:1205) only persists a marker. So fp-cond2-c's "spawn, wait for `/v1/models`, join over HTTP" cannot work on a fresh host.
- Chose: the package's option 1. The admin launch performs the join itself (`expose_client_api` then `join_mesh`, as terminal.rs:334-335 do today). Two corrections to the package: the join signal is a `joined "<mesh>"` stdout line from the child, not `/v1/models`, and the invite link goes over the child's stdin, never argv. Both rows are rewritten in place.
- Because: it is the only option that preserves behaviour (one membership, the same refusal text) with the smallest lift. Option 2 leaves a parked solo mesh on every terminal node, which is end-user-visible and no §12 decision names it (charter: operator's call). Option 3 adds a daemon lifecycle capability, against five-programs-54's minimal lift.

<!-- appendix -->

## five-programs-61 · 2026-09-25 — fp-cond2-b's admin launch carries the join; the child reports the join on stdout and takes the link on stdin

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fpcond2b-20260925.md. Reproduced at 236b74224: server.rs:687 is the only `TcpListener::bind(client_addr)`; `start_daemon` call sites are daemon.rs:1031, :1324, :1596; `expose_client_api` at :1205 calls only `persist::set_client_exposed`; the wizard joins in-process at terminal.rs:335.

The package's option 1 said `/v1/models` answering would signal a successful join. It would not: `join_mesh` calls `start_daemon(placeholder_mesh, …)` at daemon.rs:1596 and only then runs the handshake, so the listener answers while the join can still fail, and the placeholder carries the real mesh name, so `/v1/mesh/status` cannot tell them apart either. The child's own result is the only decider, so it prints the line the wizard prints today, with the prefix held in one const beside `Launch::parse`. The invite link embeds the join key, and argv is readable by every local user through `/proc/<pid>/cmdline`, so it travels on stdin.

Rejected: (2) solo-bootstrap then HTTP join, which parks a second "<host>'s Mesh" membership visible in `svrn mesh list` and the desktop MeshList; (3) a no-mesh listener in `EmbeddedDaemon`, which is new lifecycle capability.

What would falsify this: a failed `join_mesh` in the child persists state (a mesh file or a client-exposed marker) that the in-process wizard's failure path does not; the child cannot keep serving `/v1/mesh/venues` after printing its line; or fp-cond2-c's test cannot observe the stdout line before the child's first venues poll.

</details>
