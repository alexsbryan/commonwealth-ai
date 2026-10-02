<!-- ledger -->

**phase-b-13 · 2026-09-26 · pb-mcp · director** — this commit
- Needed: pb-mcp said to lift `dispatch` and `handle_tool_call` verbatim into the host kit, with a re-export left behind. `handle_tool_call` names `ToolRegistry`, `NoteStore`, `ToolPatternMatcher`, `ToolContext`, `Effect` and `StepOutput`, and the kit is `allow = ["workspace-hack"]` and names no program's vocabulary (§2c). So the move cannot compile. corpus-mcp, the first adopter, has no registry.
- Chose:
  - The kit holds the protocol half, generic over a tool-host port and a call-log port. The ToolRegistry half stays in the daemon, and pb-code-server lifts it into the registry-backed port impl.
  - The method enum and the version negotiation go to oicp-types (ladder rung 2), and sovereign-tools re-exports them.
  - The kit's allow list grows by `oicp-types`, behind an optional `mcp` feature that cw-rails does not enable.
  - HTTP+SSE framing and bundles go to pb-code-server. Exposure-as-data and the NoteStore rewire go to pb-daemon-adopts.
  - LIFT goes from ~1,000 to ~600 lines.
- Because:
  - Principle 11: a mechanism lands with its adopter. corpus-mcp is stdio-only and exposes every tool, so HTTP, exposure lists and bundles would be inventory in this row.
  - Principle 8: a second JSON-RPC envelope in the kit would be a twin. sovereign-tools and corpus-mcp already depend on oicp-types, so the move adds no edge. The boundary gate already lists oicp-types (with kernel-types) among the shared leaves of every package's closure, so widening the kit by it changes no package's closure.
  - Boundary gate: 51, unchanged. No code is in this commit.
- REVIEW-AFTER: the charter does not name growing the host kit's allow list. It reserves only exceptions, admitting other leaves and the size cap for the operator. I read an optional wire-leaf edge as within the ladder. The operator may disagree.

<!-- appendix -->

## phase-b-13 · 2026-09-26 — pb-mcp's kit dispatcher is the protocol half over a tool-host port; wire vocabulary goes to oicp-types

<details><summary>reasoning, evidence, package</summary>

The evidence is the worker's NEEDS_HUMAN package, reproduced at fb5e692be:
- `handle_tool_call` is at mcp_router.rs:485, taking `Arc<ToolRegistry>`, `Arc<NoteStore>` and `ToolPatternMatcher`.
- `ToolPatternMatcher::new(Arc::clone(&logger))` is at :183.
- `match req.method.as_str()` is at :426.
- The daemon's envelope comes through `sovereign_core::oicp::jsonrpc` (:44), and corpus-mcp's from `oicp_types::jsonrpc` (mcp.rs:11).
- `MCP_SUPPORTED_PROTOCOL_VERSIONS` is at mcp_surface.rs:470, and `negotiate_mcp_protocol_version` at :477.
- `ToolBundle::register_into(&mut ToolRegistry)` is at tool_bundle.rs:78.
- corpus-mcp `Server::call` is at tools.rs:257, `tool_list` at :194 and `instructions` at :181.
- The kit's leaf row is at ARCH_LAYERS.toml:1096 with `allow = ["workspace-hack"]`, and host-kit/Cargo.toml has no workspace deps.
- sovereign-tools/Cargo.toml:30 and corpus-mcp/Cargo.toml:44 already depend on oicp-types.

On the package's four questions:
1. The port shape is accepted.
2. The allow-list edit is approved, narrowed. The version negotiation lives in oicp-types, not in the kit, so sovereign-tools' re-export adds no sovereign-tools → host-kit edge.
3. Option (c), with (a) recorded on pb-daemon-adopts as the recommended shape.
4. The port lives in the kit only. The daemon-side rewire is on pb-daemon-adopts.

What would falsify this:
- The arch gate counts an optional dependency as an edge in cw-rails' lifted closure, so LIFT(cmnwlth) grows. Then the kit's `mcp` half needs its own leaf, which is the operator's call.
- pb-code-server's census finds the registry-backed port impl cannot live beside `ToolRegistry` without naming `NoteStore`. Then the call-log port has not removed the coupling it was meant to remove.

</details>
