// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared MCP-server surface contract.
//!
//! Sovereign exposes its tool registry via two HTTP entry points:
//!
//! - `sovereign-mesh::mcp_router` — the embedded daemon's mount,
//!   active when `sovereign daemon` owns `:9741`.
//! - `sovereign-server::routes_mcp` — the standalone MCP server,
//!   active when `sovereign serve` (or the legacy `sovereign
//!   project serve`) is running.
//!
//! Both surfaces must agree on:
//!
//! - which canonical tool ids are exposed,
//! - which legacy ids alias to which new ones,
//! - whether a given request name should be rewritten before
//!   registry lookup.
//!
//! Letting each module define its own allowlist drifted previously
//! and is the hazard `.claude/plans/...` (the CLI refactor) wants
//! to eliminate. This module is the single source of truth — both
//! HTTP modules import from here.
//!
//! ## Phase 2 vs Phase 5 layering
//!
//! Phase 2 lays down the structural split (`MCP_TOOLS_ALWAYS` +
//! `MCP_TOOLS_SPEC_GATED`) but keeps the union policy unconditional
//! — every spec-gated tool is exposed alongside the always tools.
//! Phase 5 adds the file-presence gate that conditionally unions
//! the spec-gated portion based on `.sovereign/features/*/spec.md`
//! presence.
//!
//! Test coverage for the surface lives in this crate's `tests/`.

/// MCP tools exposed unconditionally once the workspace is set up.
///
/// The flat-namespace CLI refactor renamed the original ids to
/// short canonical forms (e.g. `find_callers` → `callers`). Old
/// ids stay reachable via [`MCP_TOOL_ALIASES`] — `tools/list` emits
/// a deprecated mirror entry for every alias and `tools/call`
/// rewrites old names before the registry lookup.
pub const MCP_TOOLS_ALWAYS: &[&str] = &[
    // Code intelligence (compiler-resolved, fast).
    "symbols",
    "callers",
    "callees",
    "blast",
    // Capability map — derived "what the codebase does" from the SCIP
    // call graph (clusters of entry points + their spines). Read-only,
    // deterministic; agent-callable to get a capability-level overview
    // instead of reading files one by one.
    "capability_map",
    // Architecture observability (quality program): the full report (god
    // crates, coupling carriers, declared↔observed deltas, layer-map
    // violations, temporal coupling) + the cheap persisted-posture reader.
    "arch_report",
    "arch_posture",
    // Build/lint status. `build` is the canonical single-call
    // tool; `lint_status` + `get_lint_output` remain registered
    // for backward-compat during the alias window.
    "build",
    "lint_status",
    "get_lint_output",
    // Working notes (the audit's primary input).
    "note",
    "notes",
    // Non-destructive retirement (safe counterpart to the unexposed
    // `delete_note`): hides a stale note from `read_notes` but keeps
    // the row + supersedes chain. `note` with `supersedes` retires
    // automatically; this is for stale-with-no-replacement.
    "retire_note",
    // Catalog-driven on-demand article ingest. Surfaced so an MCP
    // client (or `mcp call wikipedia_fetch`) can drive the
    // chat-with-wikipedia loop directly when the agent's autonomous
    // tool-selection doesn't pick the catalog-hit follow-up.
    "wikipedia_fetch",
    // Recipe-author surface. The five existing tools drive the
    // author → validate → test loop; web_search / web_fetch supply
    // domain research; checkpoint / decision_log / capability_request
    // are the recipe-author-only escalation + audit surface. Together
    // they're the live tool set for the `recipe-author` skill — the
    // skill's `[tools] required` list is descriptive; MCP exposure is
    // the gate that actually lets the live agent loop reach them.
    "recipe_read",
    "recipe_write",
    "recipe_write_structured",
    "recipe_validate",
    "recipe_test",
    "registry_browse",
    "web_search",
    "web_fetch",
    "checkpoint",
    "decision_log",
    "capability_request",
    // API-shape probing + durable web findings — closed the loop
    // where the agent guessed at API contracts and never persisted
    // what it learned. probe_url returns one HTTP GET's structured
    // response (status, top-level JSON keys, pagination hint, body
    // excerpt). research_finding is the ResearchFinding writer the
    // v7 NoteStore migration left without a tool wrapping it.
    "probe_url",
    "research_finding",
    // ATOS step verification — runs verify command + hollow/untouched gates.
    "atos_verify",
    // Drift report query — point-of-edit narrative-side lookup.
    // Sibling to `symbols`/`callers`/`blast` (code-side) and
    // `drift_posture` (freshness gate). Reads the canonical drift
    // JSON sidecar; never re-runs the LLM pipeline.
    "drift_findings",
    // Session-orientation brief — the same renderer the SessionStart
    // hook injects (working set + work-in-flight + drift posture +
    // notes + recent activity), callable by any MCP client so peer
    // agents and mid-session re-orientation don't depend on the
    // `svrn code brief` CLI sibling being on PATH.
    "briefing",
    // Code facts — the embed-free deterministic fact base (tree-sitter
    // fn defs / config construction-fields / string literals), cited to
    // file:line and freshness-stamped. The structural companion to
    // `symbols`/`callers` (call graph) and `code_search` (semantic);
    // reading it never touches the inference slots. See
    // docs/CHECK_CODE_AGAINST_SPEC.md.
    "facts",
    // `drift_posture` is wired in the in-process registry today
    // but stayed out of MCP_TOOLS_ALWAYS because the freshness-
    // gate use case was operator-facing. Adding it here lets an
    // agent check freshness before relying on `drift_findings`
    // results — the natural pair.
    "drift_posture",
    // Work atlas — coordination layer for agents sharing a mesh repo.
    // `declare_scope` / `release_scope` are write-effectful; the audit
    // gate in `mcp_router::handle_tool_call` logs them at WARN.
    // `work_in_flight` is read-only, used to check overlapping work
    // before starting. See sovereign/docs/WORK_ATLAS.md.
    "declare_scope",
    "release_scope",
    "work_in_flight",
    // Corpus / atlas plane (B:P9d). These operate on the HOST's corpora and
    // structural atlas — `corpus_search`/`corpus_store` read/write the local
    // LanceDB corpus, `atlas_gaps`/`atlas_tensions` query the structural atlas,
    // and `extract` pulls text out of a document. `standard_registry` dropped
    // them when the studio bundle carved out corpus-engine (B:P5); the daemon
    // still links it and registers them (see the daemon's `build_tool_registry`),
    // so a corpus-engine-free studio client reaches them here over MCP — which is
    // what lets it run the shipped `notebook` / `summarize` workflows.
    //
    // Caveat for `extract`: its `path` argument resolves on THIS host's
    // filesystem, so it is correct for a loopback / same-box client (the studio
    // bin's primary mode) and returns a clean "file not found" for a remote
    // client whose local paths the daemon can't see — a clear error, never a
    // silent mis-read.
    "corpus_store",
    "corpus_search",
    "atlas_gaps",
    "atlas_tensions",
    "extract",
    // SOLVE — the daemon-hosted TDD solver (docs/specs/SOLVE_UX.md).
    // Unconditional on purpose: the `solve` description is the
    // discoverability mechanism telling agents this is the standard
    // engine for coding goals, so it must be visible without any
    // spec-presence gate.
    "solve",
    "solve_status",
    "solve_cancel",
    // Semantic code search. Restored to the MCP surface (2026-07-22): agents
    // reaching the server had no way to search by concept and fell back to raw
    // `grep`/file reads — the exact behaviour the code-intelligence path exists
    // to replace. Now health-aware (reports a DEGRADED index instead of a
    // silent empty), so exposing it can't masquerade "index stale" as "absent".
    "code_search",
];

/// MCP tools that should only appear when a spec exists in the
/// workspace. Phase 2 populates this slot; Phase 5 wires the
/// file-presence gate that conditionally unions them into
/// [`MCP_TOOLS_ALWAYS`] at request time. Until then the union is
/// unconditional — a fresh repo with no `.sovereign/features/`
/// will see `spec`/`drift` advertise empty content.
pub const MCP_TOOLS_SPEC_GATED: &[&str] = &["spec", "drift"];

/// Tools registered in the in-process [`sovereign_core::ToolRegistry`]
/// but no longer exposed via MCP. The flat-namespace plan retires
/// these from the agent-facing surface — their value is folded into
/// `notes` / `spec` / `audit`. They stay registered so the CLI's
/// `sovereign tools call <name>` debugging surface still works.
///
/// Documentation only — exposure is decided by [`is_mcp_exposed`].
#[allow(dead_code)]
pub const MCP_TOOLS_RETIRED: &[&str] = &[
    // `code_search` was here but is back on the MCP surface (see
    // MCP_TOOLS_ALWAYS) — semantic search is the agent-facing alternative to
    // raw grep, so retiring it worked against the code-intelligence goal.
    "recent_changes",
    "test_status",
    "run_tests",
    "get_run_output",
    "delete_note",
    "read_note_by_id",
    "read_note_digest",
    "promote_note",
    "suggest_note",
    "session_reflection",
    "check_doc_paths",
    "design_signals_extract",
    "provision_feature",
    "archive_feature",
    "record_atos_event",
    "write_redteam_finding",
    "project_context",
];

/// Backward-compat aliases mapping old MCP tool names → canonical
/// new names. `tools/list` emits a deprecated mirror for each alias
/// so cached clients keep working; `tools/call` rewrites the alias
/// before looking up the registry.
pub const MCP_TOOL_ALIASES: &[(&str, &str)] = &[
    ("find_callers", "callers"),
    ("find_callees", "callees"),
    ("blast_radius", "blast"),
    ("symbol_lookup", "symbols"),
    ("write_note", "note"),
    ("read_notes", "notes"),
];

/// Returns the canonical tool name for an incoming request. If
/// `name` is in [`MCP_TOOL_ALIASES`], returns the new name;
/// otherwise returns `name` unchanged. Borrowing the input avoids
/// allocation on the common (no-alias) path.
pub fn resolve_alias(name: &str) -> &str {
    for (old, new) in MCP_TOOL_ALIASES {
        if name == *old {
            return new;
        }
    }
    name
}

/// Returns true iff `canonical_name` (already alias-resolved) is
/// exposed via the MCP surface. Phase 2 unconditionally unions
/// `ALWAYS` and `SPEC_GATED`; Phase 5 will replace the union with
/// a file-presence-gated variant.
pub fn is_mcp_exposed(canonical_name: &str) -> bool {
    MCP_TOOLS_ALWAYS.contains(&canonical_name) || MCP_TOOLS_SPEC_GATED.contains(&canonical_name)
}

/// Render the MCP `tools/list` payload for a registry's descriptors.
///
/// Emits one entry per canonical exposed tool, plus one mirror
/// entry per [`MCP_TOOL_ALIASES`] whose target is exposed. The
/// mirror's description is prefixed with a deprecation marker so a
/// fresh client doesn't pick the alias by mistake; the input
/// schema is shared with the canonical entry so both call paths
/// validate identically.
///
/// Phase 5: callers that want spec-presence gating (the standalone
/// server, and any future per-request gate in the daemon) call
/// [`render_tools_list_gated`] instead. This unconditional variant
/// stays for backward compat — it includes every spec-gated tool
/// regardless of `.sovereign/features/*/spec.md` presence, which
/// preserves Phase 2 behaviour for callers that haven't migrated.
///
/// Centralised here so the daemon and the standalone server agree
/// on the listing without subtle divergence.
pub fn render_tools_list(
    descriptors: &[sovereign_core::types::ToolDescriptor],
) -> Vec<serde_json::Value> {
    render_tools_list_gated(descriptors, None)
}

/// Phase 5 spec-gated variant of [`render_tools_list`].
///
/// When `feature_root` is `Some(dir)`, the function consults
/// [`spec_present_in_dir`] on `dir` and excludes the entire
/// `MCP_TOOLS_SPEC_GATED` set (and their aliases) from the
/// rendered list when no spec is present. When `feature_root`
/// is `None`, behaviour matches the unconditional
/// [`render_tools_list`] — every exposed tool is emitted.
///
/// The "drop the alias too" behaviour is load-bearing: a stale
/// client that cached a `tools/list` containing the alias would
/// keep dispatching to the gated handler forever. The MCP
/// `notifications/tools/list_changed` notification (Phase 5b)
/// will tell well-behaved clients to re-fetch on disk changes;
/// this gate is what makes the re-fetch return the right answer.
pub fn render_tools_list_gated(
    descriptors: &[sovereign_core::types::ToolDescriptor],
    feature_root: Option<&std::path::Path>,
) -> Vec<serde_json::Value> {
    let spec_visible = match feature_root {
        Some(dir) => spec_present_in_dir_cached(dir),
        // Legacy unconditional: every tool is "visible" regardless
        // of disk state.
        None => true,
    };
    let mut out = Vec::new();
    for desc in descriptors {
        if !is_mcp_exposed(&desc.id) {
            continue;
        }
        if !spec_visible && MCP_TOOLS_SPEC_GATED.contains(&desc.id.as_str()) {
            continue;
        }
        out.push(serde_json::json!({
            "name": desc.id,
            "description": desc.description,
            "inputSchema": desc.parameters,
        }));
    }
    for (old, new) in MCP_TOOL_ALIASES {
        if !is_mcp_exposed(new) {
            continue;
        }
        if !spec_visible && MCP_TOOLS_SPEC_GATED.contains(new) {
            // Alias of a gated tool is gated identically. Without
            // this, an old client that cached the alias would call
            // it and bypass the gate.
            continue;
        }
        let Some(canonical) = descriptors.iter().find(|d| d.id == *new) else {
            // Canonical handler not registered in this build (e.g.
            // SCIP-disabled). Skip — emitting an alias for an
            // unreachable handler would just produce confusing 503s.
            continue;
        };
        out.push(serde_json::json!({
            "name": old,
            "description": format!(
                "(deprecated alias for `{new}`) {}",
                canonical.description
            ),
            "inputSchema": canonical.parameters,
        }));
    }
    out
}

/// Returns true iff `dir` contains either:
///
/// - A glob match `.sovereign/features/*/spec.md`, or
/// - An `ARCHITECTURE.md` at the top level.
///
/// Either signal counts as "the workspace has a spec the agent
/// can interrogate," which is what gates the `spec`/`drift` tools
/// + the spec-driven `note`/`notes` recording surface.
///
/// Walk semantics: read `<dir>/.sovereign/features/` and accept any
/// immediate subdirectory that contains a `spec.md` regular file.
/// A symlinked `spec.md` is fine — `Path::is_file` follows symlinks
/// — but a broken symlink resolves to false. The `ARCHITECTURE.md`
/// check is `is_file` at the top level.
///
/// Cheap by design: at most one `read_dir` + per-feature `is_file`
/// stat. Phase 5's cache wraps this so heavy MCP traffic doesn't
/// stat once per `tools/list` call.
pub fn spec_present_in_dir(dir: &std::path::Path) -> bool {
    if dir.join("ARCHITECTURE.md").is_file() {
        return true;
    }
    let features_dir = dir.join(".sovereign").join("features");
    let Ok(entries) = std::fs::read_dir(&features_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if entry.path().join("spec.md").is_file() {
            return true;
        }
    }
    false
}

/// Process-global cache of `(stamped_at, value)` per absolute path.
/// The map and TTL constant live up here at module scope so the
/// FS-watcher path ([`invalidate_spec_cache`] / [`crate::spec_watcher`])
/// can drop entries eagerly without round-tripping through the
/// 1-second TTL.
///
/// Mutex-poisoning recovery is the caller's responsibility — every
/// access goes through `lock_cache()` which returns the inner map
/// after a panic. A poisoned cache is a perf hit, never a
/// correctness hazard.
type SpecCacheKey = std::path::PathBuf;
type SpecCacheValue = (std::time::Instant, bool);
type SpecCacheMap = std::collections::HashMap<SpecCacheKey, SpecCacheValue>;
static SPEC_CACHE: std::sync::OnceLock<std::sync::Mutex<SpecCacheMap>> = std::sync::OnceLock::new();
const SPEC_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(1);

fn lock_cache() -> std::sync::MutexGuard<'static, SpecCacheMap> {
    let lock = SPEC_CACHE.get_or_init(|| std::sync::Mutex::new(SpecCacheMap::new()));
    lock.lock().unwrap_or_else(|p| p.into_inner())
}

/// Cached variant of [`spec_present_in_dir`] with a 1-second TTL
/// per directory. The cache is process-global; callers don't need
/// to manage one. A 1s window is short enough that interactive
/// flows ("create spec.md, run agent in opencode") feel
/// near-instant, while heavy MCP workloads (a chatty agent making
/// dozens of `tools/list` calls per second) still amortise the
/// stat over many requests.
///
/// Phase 5b: the [`crate::spec_watcher::SpecWatcher`] eagerly
/// invalidates entries via [`invalidate_spec_cache`] on
/// `.sovereign/features/*/spec.md` and `ARCHITECTURE.md` writes,
/// so cache freshness no longer depends on the TTL window.
fn spec_present_in_dir_cached(dir: &std::path::Path) -> bool {
    let mut map = lock_cache();
    let now = std::time::Instant::now();
    if let Some((stamped_at, value)) = map.get(dir) {
        if now.duration_since(*stamped_at) < SPEC_CACHE_TTL {
            return *value;
        }
    }
    let fresh = spec_present_in_dir(dir);
    map.insert(dir.to_path_buf(), (now, fresh));
    fresh
}

/// Eagerly drop the cached `spec_present_in_dir` answer for `dir`.
///
/// Called by [`crate::spec_watcher::SpecWatcher`] whenever a watched
/// path under `dir` (an `ARCHITECTURE.md` or
/// `.sovereign/features/*/spec.md`) is created, modified, or
/// removed. The next [`render_tools_list_gated`] call will re-stat
/// rather than serve a stale answer.
///
/// A no-op if `dir` was never cached.
pub fn invalidate_spec_cache(dir: &std::path::Path) {
    let mut map = lock_cache();
    map.remove(dir);
}

/// Drop every entry in the spec-presence cache. Used in tests where
/// many tempdirs accumulate, and as the watcher's "I don't know
/// which root the event belongs to" fallback.
pub fn invalidate_all_spec_caches() {
    let mut map = lock_cache();
    map.clear();
}

/// MCP protocol revisions both server mounts can speak, newest first.
///
/// What each revision demands of a server beyond 2024-11-05, and why we
/// can claim it: 2025-03-26 adds the Streamable-HTTP transport (both
/// mounts serve `POST` + SSE on one endpoint) and requires accepting
/// JSON-RPC batch bodies (both mounts do); 2025-06-18 removes batching
/// again and everything else it adds (structured output, elicitation,
/// OAuth for non-local servers) is optional — our servers are
/// loopback-only.
pub const MCP_SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] =
    ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Spec-conformant `initialize` version negotiation: echo the client's
/// requested revision when we support it; otherwise answer with our
/// newest and let the client decide whether to proceed or disconnect.
///
/// Takes the raw `initialize` params so all call sites stay one-liners.
pub fn negotiate_mcp_protocol_version(params: Option<&serde_json::Value>) -> &'static str {
    params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .and_then(|requested| {
            MCP_SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .find(|v| **v == requested)
                .copied()
        })
        .unwrap_or(MCP_SUPPORTED_PROTOCOL_VERSIONS[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Version negotiation echoes a supported requested revision,
    /// counters an unknown one with our newest, and defaults to the
    /// newest when the client sends no version at all.
    #[test]
    fn protocol_version_negotiation() {
        let req = |v: &str| serde_json::json!({ "protocolVersion": v });
        assert_eq!(
            negotiate_mcp_protocol_version(Some(&req("2024-11-05"))),
            "2024-11-05"
        );
        assert_eq!(
            negotiate_mcp_protocol_version(Some(&req("2025-03-26"))),
            "2025-03-26"
        );
        assert_eq!(
            negotiate_mcp_protocol_version(Some(&req("2025-06-18"))),
            "2025-06-18"
        );
        // Unknown revision → counter-offer our newest.
        assert_eq!(
            negotiate_mcp_protocol_version(Some(&req("2099-01-01"))),
            "2025-06-18"
        );
        // No params / no protocolVersion → newest.
        assert_eq!(negotiate_mcp_protocol_version(None), "2025-06-18");
        assert_eq!(
            negotiate_mcp_protocol_version(Some(&serde_json::json!({}))),
            "2025-06-18"
        );
    }

    /// `resolve_alias` round-trips legacy ids to their new canonical
    /// form and is a no-op for already-canonical / unknown ids.
    #[test]
    fn resolve_alias_rewrites_legacy_ids() {
        assert_eq!(resolve_alias("find_callers"), "callers");
        assert_eq!(resolve_alias("find_callees"), "callees");
        assert_eq!(resolve_alias("blast_radius"), "blast");
        assert_eq!(resolve_alias("symbol_lookup"), "symbols");
        assert_eq!(resolve_alias("write_note"), "note");
        assert_eq!(resolve_alias("read_notes"), "notes");
        // Already canonical — returned verbatim.
        assert_eq!(resolve_alias("callers"), "callers");
        // Unknown — returned verbatim (the caller decides whether
        // to reject).
        assert_eq!(resolve_alias("not_a_tool"), "not_a_tool");
    }

    #[test]
    fn is_mcp_exposed_admits_renamed_canonical_ids() {
        for canonical in MCP_TOOLS_ALWAYS {
            assert!(
                is_mcp_exposed(canonical),
                "ALWAYS entry {canonical} should be exposed"
            );
        }
    }

    #[test]
    fn retired_ids_are_not_exposed() {
        for retired in MCP_TOOLS_RETIRED {
            assert!(
                !is_mcp_exposed(retired),
                "retired tool {retired} should not be MCP-exposed"
            );
        }
    }

    /// Every legacy alias must point to a canonical id that is in
    /// `MCP_TOOLS_ALWAYS` or `MCP_TOOLS_SPEC_GATED`. An alias that
    /// targets a non-exposed id is a configuration error — the
    /// `tools/list` mirror would advertise a name `tools/call`
    /// rejects.
    #[test]
    fn every_alias_target_is_exposed() {
        for (old, new) in MCP_TOOL_ALIASES {
            assert!(
                is_mcp_exposed(new),
                "alias {old} → {new} but {new} is not in ALWAYS or SPEC_GATED"
            );
        }
    }

    // ─── Phase 5: spec-presence gate ─────────────────────────────

    /// `spec_present_in_dir` finds an `ARCHITECTURE.md` at the top
    /// level of the supplied dir. This is the simpler of the two
    /// signals — no glob walk, just a single `is_file` check.
    #[test]
    fn spec_present_finds_architecture_md_at_top_level() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ARCHITECTURE.md"), b"# arch\n").unwrap();
        assert!(spec_present_in_dir(dir.path()));
    }

    /// `spec_present_in_dir` finds a feature spec under
    /// `.sovereign/features/<id>/spec.md`. Multiple feature
    /// directories are supported — the first hit short-circuits.
    #[test]
    fn spec_present_finds_feature_spec_md() {
        let dir = tempfile::tempdir().unwrap();
        let foo = dir.path().join(".sovereign").join("features").join("foo");
        std::fs::create_dir_all(&foo).unwrap();
        std::fs::write(foo.join("spec.md"), b"# foo spec\n").unwrap();
        assert!(spec_present_in_dir(dir.path()));
    }

    /// A `.sovereign/features/<id>/` directory with NO `spec.md`
    /// inside doesn't count. The gate is strictly about the spec
    /// file's presence, not the surrounding scaffolding.
    #[test]
    fn spec_present_ignores_feature_dir_without_spec_md() {
        let dir = tempfile::tempdir().unwrap();
        let foo = dir.path().join(".sovereign").join("features").join("foo");
        std::fs::create_dir_all(&foo).unwrap();
        // Sibling files that aren't spec.md must not trigger the gate.
        std::fs::write(foo.join("brief.md"), b"# brief\n").unwrap();
        assert!(!spec_present_in_dir(dir.path()));
    }

    /// A fresh repo with no `.sovereign/` and no `ARCHITECTURE.md`
    /// — the agent's view should be the spec-gateless minimum until
    /// the user creates one of those signals.
    #[test]
    fn spec_present_returns_false_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!spec_present_in_dir(dir.path()));
    }

    /// A non-existent input dir is a clean false rather than a
    /// panic. `read_dir` on a missing path returns Err; the gate
    /// must treat that as "no spec."
    #[test]
    fn spec_present_returns_false_for_missing_dir() {
        let path = std::path::PathBuf::from("/this/path/does/not/exist/xyz123");
        assert!(!spec_present_in_dir(&path));
    }

    // ─── render_tools_list_gated ─────────────────────────────────

    /// Build a minimal descriptor list covering the surfaces we
    /// care about for the gate test: two always-on canonical tools
    /// (`callers`, `note`) and two spec-gated canonical tools
    /// (`spec`, `drift`). The current alias map has no aliases that
    /// target spec-gated tools, so we cover those branches via the
    /// canonical-id assertions.
    fn fake_descriptors() -> Vec<sovereign_core::types::ToolDescriptor> {
        use sovereign_core::types::{Effect, Idempotency, Latency, Scope, ToolDescriptor};
        let make = |id: &str, desc: &str, effect: Effect| ToolDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            description: desc.to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            examples: vec![],
            effect,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: None,
        };
        vec![
            make("callers", "find callers", Effect::Read),
            make("note", "write a note", Effect::Write),
            make("spec", "read the spec", Effect::Read),
            make("drift", "show drift", Effect::Read),
        ]
    }

    /// When `feature_root` is `None`, the gated renderer behaves
    /// identically to the unconditional one — every exposed tool
    /// (canonical + alias) ships. This is the daemon's default.
    #[test]
    fn render_gated_with_no_feature_root_includes_spec_gated() {
        let descs = fake_descriptors();
        let out = render_tools_list_gated(&descs, None);
        let names: Vec<String> = out
            .iter()
            .filter_map(|v| v["name"].as_str().map(String::from))
            .collect();
        assert!(
            names.contains(&"callers".to_string()),
            "always-on missing: {names:?}"
        );
        assert!(
            names.contains(&"spec".to_string()),
            "spec-gated missing: {names:?}"
        );
        assert!(
            names.contains(&"drift".to_string()),
            "spec-gated missing: {names:?}"
        );
    }

    /// When `feature_root` points at a directory with no spec, the
    /// gated renderer drops `spec` and `drift` (and any aliases that
    /// target them — none in the current alias map, but the code
    /// path is exercised). Always-on tools remain.
    #[test]
    fn render_gated_drops_spec_gated_when_no_spec_present() {
        let dir = tempfile::tempdir().unwrap();
        let descs = fake_descriptors();
        let out = render_tools_list_gated(&descs, Some(dir.path()));
        let names: Vec<String> = out
            .iter()
            .filter_map(|v| v["name"].as_str().map(String::from))
            .collect();
        assert!(
            names.contains(&"callers".to_string()),
            "always-on tool dropped: {names:?}"
        );
        assert!(
            names.contains(&"note".to_string()),
            "always-on tool dropped: {names:?}"
        );
        assert!(
            !names.contains(&"spec".to_string()),
            "spec-gated tool leaked despite no spec on disk: {names:?}"
        );
        assert!(
            !names.contains(&"drift".to_string()),
            "spec-gated tool leaked despite no spec on disk: {names:?}"
        );
    }

    /// When the feature_root contains a spec, the gated renderer
    /// includes everything — equivalent to the always-on case. This
    /// is the "user dropped a spec.md, agent now sees the new
    /// tools" path.
    #[test]
    fn render_gated_includes_spec_gated_when_spec_present() {
        let dir = tempfile::tempdir().unwrap();
        let foo = dir.path().join(".sovereign").join("features").join("foo");
        std::fs::create_dir_all(&foo).unwrap();
        std::fs::write(foo.join("spec.md"), b"# foo\n").unwrap();
        // Make sure a previous test's cache entry for an unrelated
        // tempdir doesn't leak into this one — we use a fresh path
        // each test, so the cache key is unique.
        let descs = fake_descriptors();
        let out = render_tools_list_gated(&descs, Some(dir.path()));
        let names: Vec<String> = out
            .iter()
            .filter_map(|v| v["name"].as_str().map(String::from))
            .collect();
        assert!(
            names.contains(&"spec".to_string()),
            "spec-gated tool missing despite spec on disk: {names:?}"
        );
        assert!(
            names.contains(&"drift".to_string()),
            "spec-gated tool missing despite spec on disk: {names:?}"
        );
    }

    /// Tests that mutate the process-global spec-cache via
    /// `invalidate_all_spec_caches()` race each other under
    /// `cargo test`'s parallel runner — one test can wipe another's
    /// primed entry and break a "should be cached" assertion.
    /// We serialise them with a per-test-suite `Mutex` rather than
    /// pulling in `serial_test`.
    fn cache_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// `invalidate_spec_cache(dir)` drops the cached answer for
    /// that path. After the drop, the next render re-stats the disk
    /// and reflects the new state — a critical step for the
    /// FS watcher in `spec_watcher` to deliver eager visibility
    /// on spec writes.
    #[test]
    fn invalidate_spec_cache_drops_entry_so_next_call_restats() {
        let _g = cache_test_lock();
        let dir = tempfile::tempdir().unwrap();
        // Prime the cache with the empty state.
        assert!(!spec_present_in_dir_cached(dir.path()));

        // Create a spec but DON'T invalidate yet — the cache should
        // still report false during the TTL window.
        let foo = dir.path().join(".sovereign").join("features").join("foo");
        std::fs::create_dir_all(&foo).unwrap();
        std::fs::write(foo.join("spec.md"), b"# foo\n").unwrap();
        assert!(
            !spec_present_in_dir_cached(dir.path()),
            "cache should still return the pre-write answer until invalidation"
        );

        // Invalidate; next call must re-stat and see the new spec.
        invalidate_spec_cache(dir.path());
        assert!(
            spec_present_in_dir_cached(dir.path()),
            "post-invalidation call must reflect on-disk state"
        );
    }

    /// `invalidate_all_spec_caches()` clears every entry — useful as
    /// a watcher-level "fall through to re-stat" hammer when the
    /// event's path doesn't map cleanly to a cached root.
    #[test]
    fn invalidate_all_spec_caches_clears_every_entry() {
        let _g = cache_test_lock();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // Prime both with their (empty) state.
        assert!(!spec_present_in_dir_cached(a.path()));
        assert!(!spec_present_in_dir_cached(b.path()));

        // Add a spec to A only.
        let foo = a.path().join(".sovereign").join("features").join("foo");
        std::fs::create_dir_all(&foo).unwrap();
        std::fs::write(foo.join("spec.md"), b"# foo\n").unwrap();

        // Without invalidation, A still reports stale.
        assert!(!spec_present_in_dir_cached(a.path()));

        // Drop both entries; next A read sees fresh state, B still
        // returns false because it has no spec.
        invalidate_all_spec_caches();
        assert!(spec_present_in_dir_cached(a.path()));
        assert!(!spec_present_in_dir_cached(b.path()));
    }

    /// The 1-second TTL means the cache returns a stable answer for
    /// rapid back-to-back calls on the same path. We can't reliably
    /// test the post-TTL invalidation in unit tests (it would
    /// require a real 1-second wait), but we can verify the
    /// cache-hit path doesn't lose data by toggling the underlying
    /// file mid-run and confirming both snapshots are visible from
    /// the gated rendering.
    #[test]
    fn render_gated_cache_keys_per_distinct_path() {
        // Two separate tempdirs: one with a spec, one without. The
        // cache is keyed on the absolute path, so they should not
        // contaminate each other's answers.
        let with_spec = tempfile::tempdir().unwrap();
        let foo = with_spec
            .path()
            .join(".sovereign")
            .join("features")
            .join("foo");
        std::fs::create_dir_all(&foo).unwrap();
        std::fs::write(foo.join("spec.md"), b"# foo\n").unwrap();

        let without_spec = tempfile::tempdir().unwrap();

        let descs = fake_descriptors();
        let with = render_tools_list_gated(&descs, Some(with_spec.path()));
        let without = render_tools_list_gated(&descs, Some(without_spec.path()));

        let names_with: Vec<String> = with
            .iter()
            .filter_map(|v| v["name"].as_str().map(String::from))
            .collect();
        let names_without: Vec<String> = without
            .iter()
            .filter_map(|v| v["name"].as_str().map(String::from))
            .collect();

        assert!(names_with.contains(&"spec".to_string()));
        assert!(!names_without.contains(&"spec".to_string()));
    }
}
