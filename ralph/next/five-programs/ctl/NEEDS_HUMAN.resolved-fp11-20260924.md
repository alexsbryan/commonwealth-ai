# NEEDS_HUMAN — fp-11 (sovereign-daemon → sovereign-code)

## (a) Unit

`- [ ] fp-11 — depends [fp-10] — DIAL code tools via a project-scoped /mcp root (§12 decision 2 + §11 probe): project serve needs the daemon to mount a project-scoped /mcp root; the daemon's sovereign-code/tool_registry mount becomes the client. TSV pair sovereign-daemon→sovereign-code (32 refs in one mount file). — read: TSV row, §11 project-serve probe, DA tool_registry.rs — check: scoped lint 0; gate delta recorded`

fp-10 is `[x]`; the row is dep-ready. The premise check fails, so I made no
edit. STATE.md is untouched.

## (b) What I ran, at f03daf7ff

`scripts/ralph-check.sh boundary` → `boundary-gate FAILED (56 violation(s))`,
edge red at boundary.log:73:

    ✗ [svrn] sovereign-daemon → sovereign-code

`git grep -n sovereign_code -- sovereign/crates/sovereign-daemon`: 34 refs in
`src/tool_registry.rs` (the row says 32; TSV:15 says 34), plus 14 in
`tests/main/e2e_code_intel*`. The test refs are dev-only, which the gate does
not enforce (arch-layers lib.rs:172), so the src file carries the edge by itself.

The 34 refs construct 27 tools. The list goes well past "code intel":
`SymbolLookup/CodeSearch/RecentChanges/FindCallers/FindCallees/BlastRadius`,
`CapabilityMap/Findings/Posture`, `ArchReport/ArchPosture`,
`DriftFindings/DriftPosture`, `Briefing`, `Facts`, `Build`,
`LintStatus/GetLintOutput/TestStatus/RunTests/GetRunOutput`, **and
`ReadNotes/WriteNote/RetireNote/DeleteNote/SessionState/SessionReflection`**.
That last group is the `notes` / `note` / `session_state` MCP surface. Every
agent session's hooks and protocol call it. The registry also serves the
daemon's in-process Runtime (tool_registry.rs:42-46 comment), not only `/mcp`.

A "DIAL" needs something to dial, and nothing in the tree can be dialled:

1. **No code-program MCP server exists that the daemon could be a client of.**
   The only other server is `svrn project serve`
   (`sovereign-cli-dev/src/project_cmd/serve.rs`). It is user-launched, it is
   not discoverable by the daemon, and it builds its MCP app from
   `sovereign_daemon::mcp_router::{FeatureRoot, McpNotifier, mcp_router}`
   (serve.rs:666,675,696). That is the cli-dev → sovereign-daemon edge that
   §11 REFUSED on 2026-09-21. The row joins two opposite directions: §11 asks
   the daemon's `/mcp` to take a project root, so that project serve dials the
   daemon. Closing daemon → code needs the reverse: the code process serves and
   the daemon dials. Doing both would leave each process dialling the other.
2. **The SCIP graph is shared with the daemon's reindexer.** boot.rs:640-651
   builds ONE `ScipGraphHandle` (a `corpus_engine_watchers::reindexer` type) and
   hands it to both the registry and `start_freshness_pipeline`. The comment
   there records that doing it any other way caused the "always stale" bug. If
   the tools move to another process, the reindexer (and so the watchers
   package) has to move with them. Otherwise the frozen-snapshot bug returns.
3. **Behaviour.** TSV:15's consequence cell accepts that the code tools stop
   when the code process is not running. On the default path, with only the
   daemon up, that would take `symbols/callers/blast/notes/session_state` off
   every agent's MCP surface. They would report absence, not answer. §12
   decision 2 covers the SERVING cluster (inference, mesh, model server). It
   says nothing that changes the default MCP surface. PROMPT §7 says a row that
   changes a default without its decision saying so goes to §6.

## (c) What the operator must decide

1. Where the code-program MCP server lives and who starts it. The options:
   (a) a standalone `[code]` server (project serve with `mcp_router` moved out
   of the daemon into a home both can name), started by the operator or the
   harness and never supervised by the daemon (principle 12); or
   (b) grandfather `sovereign-daemon → sovereign-code` with an `[[exception]]`
   (`package = "svrn"`). The reason would be that the daemon is today's one
   MCP host for code tools and notes. fp-10 was resolved this way for inference
   and compute (NEEDS_HUMAN.resolved-fp10-20260924.md, 1bb11d2ee).
2. If (a): does the daemon's reindexer and SCIP-graph ownership move to the code
   process too? Needed so that tool_registry.rs:31-38 and boot.rs:640-651 keep
   a single live graph.
3. If (a): do the notes / session tools (`ReadNotes`…`SessionReflection`) go
   with the code program? Or do they stay reachable on the daemon's `/mcp`
   when the code process is down? This decides whether every harness session
   depends on a second process.
4. Whether the row should split into ordered rows: move mcp_router out, stand
   up the server, add the daemon's proxy tool, then drop the dep. It touches
   well over ten files and cannot land in one commit.

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
