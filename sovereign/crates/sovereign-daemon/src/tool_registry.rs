// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon MCP tool-registry construction — extracted from `daemon_cmd`
//! (§3.2). Builds svrn's own `/mcp` `ToolRegistry`: the corpus, parcel,
//! SEC, Wikipedia and solve tools. Code's tools (code intelligence, notes,
//! the work atlas, lint/test) are the code program's since
//! pb-code-daemon-exit; they mount beside these when a distribution
//! composes code into this process (`crate::hosted_code`).

use std::sync::Arc;

use corpus_engine_atlas_reader::ports::AtlasPort;
use corpus_index::ingest_port::daemon::IngestPort;
use sovereign_core::ToolRegistry;

/// `ingest` is ingest's port and atlas port when a distribution composed
/// ingest into this process (pb-ingest-dial-daemon); `None` withholds the
/// corpus, parcel, SEC, Wikipedia and workflow-corpus tools and says so.
pub async fn build_tool_registry(
    ingest: Option<(Arc<dyn IngestPort>, Arc<dyn AtlasPort>)>,
    solve_jobs: Arc<super::solve_http::SolveJobs>,
) -> ToolRegistry {
    // Tier 4 — shared tool-result cache. This registry serves `/mcp` only;
    // the turn Runtime's tools are `baseline_bundles` plus `[[mcp_servers]]`
    // (daemon_cmd/boot.rs). Per-conversation scoping in `CacheKey` keeps the
    // slices isolated even when two clients hit different conversations
    // simultaneously.
    let tool_cache = std::sync::Arc::new(sovereign_core::tool_result_cache::ToolResultCache::new());
    let mut tools = ToolRegistry::new().with_cache(std::sync::Arc::clone(&tool_cache));

    match ingest {
        Some((engine, atlas)) => register_ingest_tools(&mut tools, engine, atlas),
        None => tracing::info!(
            "tool_registry: no ingest program in this process; the corpus, parcel, SEC, \
             Wikipedia and workflow-corpus tools are withheld from /mcp"
        ),
    }

    // SOLVE — the daemon-hosted TDD solver (docs/specs/SOLVE_UX.md).
    // Same job table as the /v1/solve/jobs HTTP surface, so MCP
    // agents and curl sessions see the same jobs.
    tools.register(Box::new(
        super::solve_tools::SolveTool(Arc::clone(&solve_jobs)).declared(),
    ));
    tools.register(Box::new(
        super::solve_tools::SolveStatusTool(Arc::clone(&solve_jobs)).declared(),
    ));
    tools.register(Box::new(
        super::solve_tools::SolveCancelTool(solve_jobs).declared(),
    ));

    // Tools that are pure data. Every manifest under
    // `sovereign-contracts/tool-manifests/` declaring `delegate = "<id>"` is
    // bound here to the tool it names — so adding one of those is a TOML edit
    // and nothing else. LAST, because it can only bind targets already
    // registered above. Installs nothing until a manifest declares a delegate;
    // that empty case is the same shape as a recipe registry with no rows, not
    // a dark switch.
    tools.install_declared();

    tools
}

/// The tools that act through ingest's ports.
fn register_ingest_tools(
    tools: &mut ToolRegistry,
    engine: Arc<dyn IngestPort>,
    atlas: Arc<dyn AtlasPort>,
) {
    // Deterministic land-value-tax analytics over parcel corpora
    // (e.g. sf-assessor-roll) — pre-cited figures for the "no
    // confabulated numbers" demo. Read-only; safe on the MCP surface.
    tools.register(Box::new(
        sovereign_tools::parcel_analytics::ParcelAnalyticsTool::new(Arc::clone(&engine) as _)
            .declared(),
    ));
    // Typed SEC-filing figures with basis + accession, or first-class
    // refusals; declares the opt-in bare-numeral audit (FINANCIAL_CORPORA §6).
    tools.register(Box::new(
        sovereign_tools::sec_facts::SecFactsTool::new(Arc::clone(&engine) as _).declared(),
    ));

    // Doc-path checker — no state dependency.

    // Wikipedia on-demand fetch — operates against the catalog corpus
    // installed on this daemon. Wired here so `svrn tools call
    // wikipedia_fetch --title=…` and the MCP /mcp surface can drive
    // catalog-hit → fetch end-to-end without a live chat session.
    tools.register(Box::new(
        sovereign_tools::WikipediaFetchTool::new(engine as _, Arc::clone(&atlas)).declared(),
    ));

    // B:P9d — the corpus/atlas plane (corpus_store, corpus_search, atlas_gaps,
    // atlas_tensions, + the document ExtractTool). `standard_registry` dropped
    // these when the studio bundle carved out corpus-engine (B:P5); the daemon
    // registers them HERE, over ingest's atlas port, to serve them over
    // `/mcp`, letting a corpus-engine-free studio client (B:P9e) run the shipped
    // notebook / summarize workflows remotely. Each is a bare unit struct that
    // opens its corpus/atlas (or document) from disk at call time, so no engine
    // handle is threaded in. `extract` resolves its `path` on THIS host's
    // filesystem — correct for a loopback client, a clean "file not found" for a
    // remote one (see `sovereign_tools::mcp_surface::MCP_TOOLS_ALWAYS`).
    for tool in sovereign_tools::workflow_corpus_tools(atlas) {
        tools.register(tool);
    }
}
