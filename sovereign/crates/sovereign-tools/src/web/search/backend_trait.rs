// SPDX-License-Identifier: AGPL-3.0-or-later
//! Trait-based dispatch for web search backends.
//!
//! Phase 0 of the production-search integration
//! (`sovereign/docs/PRODUCTION_SEARCH_INTEGRATION.md`). Adds the
//! `WebSearchBackend` trait + `WebSearchRegistry` alongside the
//! existing `SearchBackend` enum + `search()` free function. The
//! legacy surface remains in place so the eight current call sites
//! keep working (ARCH §10.1 behavior-preserving); new code (the
//! Phase 2 orchestrator, future backends) consumes the trait.
//!
//! Why a trait + registry over the enum: per `ARCH_PRINCIPLES.md`
//! §4 ("when new implementations of a trait are expected to appear
//! over time — domains, middleware, tools, exporters — dispatch
//! through a registry, not a match on an id string"). The set of
//! web-search backends is open — Kagi, Google CSE, an internal
//! corpus, BYOM enterprise search will all want in over time, and
//! shipping any of those should be a `register` call, not an enum
//! variant + match-arm edit across the codebase.
//!
//! The wrapper structs in this file (`MockBackendImpl`,
//! `TavilyBackendImpl`, …) implement the trait by delegating to the
//! existing free functions (`search_mock`, `search_tavily`, etc.).
//! Once Phase 2's orchestrator ships and all call sites migrate to
//! the trait, the legacy enum + dispatcher can be retired.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::Error;

use super::{search as legacy_dispatch, SearchBackend as LegacyBackend, SearchResult};

/// Privacy posture for a search backend. Drives orchestrator-side
/// filtering: a request with OICP `LocalOnly` privacy must only see
/// `Local` backends. Per ARCH §7.1, this is encoded on the backend
/// itself rather than passed as a parameter — a caller cannot flip
/// it via config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPrivacy {
    /// Query never leaves this node. Mock fixtures, an in-process
    /// internal-corpus search, anything that doesn't make a network
    /// call to a third party.
    Local,
    /// Query may be sent to mesh peers (federated knowledge search).
    /// Acceptable when the request's OICP privacy is `MeshAllowed`
    /// or `External`. Not used in Phase 0 — placeholder for the
    /// federated search workstream.
    Mesh,
    /// Query goes to an external provider (Tavily, Brave, …). The
    /// `provider` field is the stable id used in tracing + budget
    /// accounting; it must match the backend's `id()` for
    /// audit-log correlation.
    External { provider: &'static str },
}

impl SearchPrivacy {
    /// Total ordering for the orchestrator's "max privacy" filter:
    /// `Local <= Mesh <= External`. A request with `max_privacy =
    /// Local` may only use `Local` backends; with `max_privacy =
    /// External` any backend is allowed.
    pub fn rank(&self) -> u8 {
        match self {
            Self::Local => 0,
            Self::Mesh => 1,
            Self::External { .. } => 2,
        }
    }
}

/// Cost estimate for one backend call. Denominations are
/// backend-specific (Tavily credits, Brave queries, …) — the
/// orchestrator doesn't try to convert between them; the operator
/// configures per-backend budgets in the matching denomination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchCost {
    pub units_per_call: u32,
    pub denomination: &'static str,
}

/// The contract every backend implements. Minimal per ARCH §5.1:
/// caching, retry, budget accounting, and the URL-allowlist
/// accumulator are orchestrator concerns, not backend concerns —
/// they don't belong on the trait.
#[async_trait]
pub trait WebSearchBackend: Send + Sync {
    /// Issue a query against this backend. Implementations MUST
    /// respect `max_results` (no over-return). On error, return
    /// the underlying provider's diagnostic; the orchestrator will
    /// decide whether to fall through to the next backend.
    async fn search(
        &self,
        client: &reqwest::Client,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, Error>;

    /// Stable backend identifier — used in tracing, operator
    /// preference lists, budget accounting, audit logs. Must match
    /// the `provider` field in `SearchPrivacy::External` when the
    /// backend is external. Per ARCH §2.2 this is the wire-contract
    /// alias for the type — operators see it in config, logs name
    /// it, dashboards aggregate on it.
    fn id(&self) -> &'static str;

    /// Privacy posture — see `SearchPrivacy` for semantics. Must
    /// be a constant for a given backend type (the orchestrator
    /// can cache the value across requests).
    fn privacy(&self) -> SearchPrivacy;

    /// Per-call cost estimate, or `None` for free/local backends
    /// the budget gate doesn't track.
    fn cost_estimate(&self) -> Option<SearchCost>;
}

/// Open-set registry per ARCH §4.1. Backends register at daemon
/// startup; the orchestrator looks them up by id or iterates the
/// full set during candidate filtering.
///
/// Single-threaded by construction (built once, read many) — the
/// registry itself is `Send + Sync` via `Arc<dyn WebSearchBackend>`
/// values, but mutations happen at construction only.
pub struct WebSearchRegistry {
    by_id: HashMap<&'static str, Arc<dyn WebSearchBackend>>,
}

impl WebSearchRegistry {
    pub fn new() -> Self {
        Self {
            by_id: HashMap::new(),
        }
    }

    /// Register a backend. Per ARCH §4.3 unknown-id handling is
    /// loud — registering a second backend with the same id is a
    /// configuration bug; replacing one would mask the
    /// double-registration silently. The current shape *replaces*
    /// for caller ergonomics (tests often re-register a stub on
    /// top of the default Mock); revisit if production setup gains
    /// the same hazard.
    pub fn register(&mut self, backend: Arc<dyn WebSearchBackend>) {
        let id = backend.id();
        if self.by_id.insert(id, backend).is_some() {
            tracing::warn!(
                backend_id = %id,
                "WebSearchRegistry: replaced existing backend registration"
            );
        }
    }

    /// Look up a backend by id. Returns `None` if no backend with
    /// that id is registered. Callers that treat a missing id as a
    /// hard error should map this to their own error type — the
    /// registry doesn't know what "missing" means in the caller's
    /// context (operator typo vs. capability filter).
    pub fn get(&self, id: &str) -> Option<Arc<dyn WebSearchBackend>> {
        self.by_id.get(id).cloned()
    }

    /// Iterate all registered backends. Order is HashMap-arbitrary;
    /// the orchestrator imposes its own ordering via the operator
    /// preference list.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn WebSearchBackend>> {
        self.by_id.values()
    }

    /// Count of registered backends — used by health checks and the
    /// "is search even configured?" gate.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

impl Default for WebSearchRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Wrapper impls — delegate to the legacy enum dispatcher ────
//
// Each wrapper holds the same construction parameters as the
// corresponding `SearchBackend` enum variant. The trait impl
// constructs a transient `LegacyBackend` value on each call and
// hands it to the existing `search()` free function — no logic is
// duplicated. Once all call sites migrate to the trait, the legacy
// enum can be retired and these wrappers become the real
// implementations.

/// Mock backend (gym fixtures). `corpus_path` is the directory
/// containing `aliases.toml` + per-fixture JSON files.
pub struct MockBackendImpl {
    corpus_path: PathBuf,
}

impl MockBackendImpl {
    pub fn new(corpus_path: PathBuf) -> Self {
        Self { corpus_path }
    }
}

#[async_trait]
impl WebSearchBackend for MockBackendImpl {
    async fn search(
        &self,
        client: &reqwest::Client,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, Error> {
        let backend = LegacyBackend::Mock {
            corpus_path: self.corpus_path.clone(),
        };
        legacy_dispatch(client, &backend, query, max_results).await
    }

    fn id(&self) -> &'static str {
        "mock"
    }
    fn privacy(&self) -> SearchPrivacy {
        SearchPrivacy::Local
    }
    fn cost_estimate(&self) -> Option<SearchCost> {
        None
    }
}

/// DuckDuckGo backend. Zero-config but HTML-scrape-based; fragile
/// against DDG's anti-bot measures.
pub struct DuckDuckGoBackendImpl;

impl DuckDuckGoBackendImpl {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DuckDuckGoBackendImpl {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebSearchBackend for DuckDuckGoBackendImpl {
    async fn search(
        &self,
        client: &reqwest::Client,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, Error> {
        legacy_dispatch(client, &LegacyBackend::DuckDuckGo, query, max_results).await
    }

    fn id(&self) -> &'static str {
        "duckduckgo"
    }
    fn privacy(&self) -> SearchPrivacy {
        SearchPrivacy::External {
            provider: "duckduckgo",
        }
    }
    fn cost_estimate(&self) -> Option<SearchCost> {
        // DDG is free at the scraping endpoint we use — no budget
        // gate. The cost is rate-limit + bot-detection risk, not
        // dollars; tracked separately if it becomes load-bearing.
        None
    }
}

/// Brave Search API. Requires `api_key`. ~$5/1k queries.
pub struct BraveBackendImpl {
    api_key: String,
}

impl BraveBackendImpl {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

// Redact the key from Debug to avoid logging it. ARCH-aligned with the
// secret-handling note in the production-search plan's risk register.
impl std::fmt::Debug for BraveBackendImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BraveBackendImpl")
            .field("api_key", &"<redacted>")
            .finish()
    }
}

#[async_trait]
impl WebSearchBackend for BraveBackendImpl {
    async fn search(
        &self,
        client: &reqwest::Client,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, Error> {
        let backend = LegacyBackend::Brave {
            api_key: self.api_key.clone(),
        };
        legacy_dispatch(client, &backend, query, max_results).await
    }

    fn id(&self) -> &'static str {
        "brave"
    }
    fn privacy(&self) -> SearchPrivacy {
        SearchPrivacy::External { provider: "brave" }
    }
    fn cost_estimate(&self) -> Option<SearchCost> {
        Some(SearchCost {
            units_per_call: 1,
            denomination: "brave-queries",
        })
    }
}

/// Tavily backend. AI-native search with pre-extracted content;
/// 1000 free queries per month. Requires `api_key`.
pub struct TavilyBackendImpl {
    api_key: String,
}

impl TavilyBackendImpl {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

impl std::fmt::Debug for TavilyBackendImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TavilyBackendImpl")
            .field("api_key", &"<redacted>")
            .finish()
    }
}

#[async_trait]
impl WebSearchBackend for TavilyBackendImpl {
    async fn search(
        &self,
        client: &reqwest::Client,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, Error> {
        let backend = LegacyBackend::Tavily {
            api_key: self.api_key.clone(),
        };
        legacy_dispatch(client, &backend, query, max_results).await
    }

    fn id(&self) -> &'static str {
        "tavily"
    }
    fn privacy(&self) -> SearchPrivacy {
        SearchPrivacy::External { provider: "tavily" }
    }
    fn cost_estimate(&self) -> Option<SearchCost> {
        Some(SearchCost {
            units_per_call: 1,
            denomination: "tavily-credits",
        })
    }
}

// ─── Tests — pin the abstraction shape per ARCH §7.2 ─────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_rank_is_total_order() {
        assert!(SearchPrivacy::Local.rank() < SearchPrivacy::Mesh.rank());
        assert!(SearchPrivacy::Mesh.rank() < SearchPrivacy::External { provider: "x" }.rank());
    }

    #[test]
    fn external_backends_declare_correct_provider_id() {
        let t = TavilyBackendImpl::new("dummy".into());
        assert_eq!(t.id(), "tavily");
        assert!(matches!(
            t.privacy(),
            SearchPrivacy::External { provider: "tavily" }
        ));

        let b = BraveBackendImpl::new("dummy".into());
        assert_eq!(b.id(), "brave");
        assert!(matches!(
            b.privacy(),
            SearchPrivacy::External { provider: "brave" }
        ));

        let d = DuckDuckGoBackendImpl::new();
        assert_eq!(d.id(), "duckduckgo");
        assert!(matches!(
            d.privacy(),
            SearchPrivacy::External {
                provider: "duckduckgo"
            }
        ));
    }

    #[test]
    fn local_backends_declare_local_privacy() {
        let m = MockBackendImpl::new(PathBuf::new());
        assert_eq!(m.id(), "mock");
        assert!(matches!(m.privacy(), SearchPrivacy::Local));
        assert!(m.cost_estimate().is_none());
    }

    #[test]
    fn external_backends_report_cost() {
        // Cost denomination must match the provider id so the
        // operator's per-backend budget table is unambiguously
        // addressed. Pin the convention here so a future refactor
        // doesn't accidentally homogenise the denominations.
        let t = TavilyBackendImpl::new("dummy".into());
        let c = t.cost_estimate().expect("tavily has a cost");
        assert_eq!(c.denomination, "tavily-credits");
        assert_eq!(c.units_per_call, 1);

        let b = BraveBackendImpl::new("dummy".into());
        let c = b.cost_estimate().expect("brave has a cost");
        assert_eq!(c.denomination, "brave-queries");
    }

    #[test]
    fn api_key_redacted_in_debug() {
        // ARCH risk-register invariant: API keys must not leak via
        // Debug. Pin it with a test so a careless future refactor
        // doesn't reintroduce the leak.
        let t = TavilyBackendImpl::new("secret-key-12345".into());
        let dbg = format!("{:?}", t);
        assert!(
            !dbg.contains("secret-key-12345"),
            "api key leaked in Debug: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "Debug should mark redaction: {dbg}"
        );

        let b = BraveBackendImpl::new("secret-key-12345".into());
        let dbg = format!("{:?}", b);
        assert!(!dbg.contains("secret-key-12345"));
    }

    #[test]
    fn registry_holds_and_retrieves_by_id() {
        let mut r = WebSearchRegistry::new();
        assert!(r.is_empty());
        r.register(Arc::new(TavilyBackendImpl::new("dummy".into())));
        r.register(Arc::new(DuckDuckGoBackendImpl::new()));
        assert_eq!(r.len(), 2);
        assert!(r.get("tavily").is_some());
        assert!(r.get("duckduckgo").is_some());
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn registry_replaces_on_duplicate_id() {
        // Trait + Arc make object equality awkward to test directly;
        // assert via cost_estimate observable difference. The first
        // registration has units_per_call=1; we'd register a "fake"
        // Tavily with different cost, but since we delegate the
        // cost-estimate values to a const, we exercise the "len
        // stays 1" invariant instead — the registry stores per id,
        // not per registration call.
        let mut r = WebSearchRegistry::new();
        r.register(Arc::new(TavilyBackendImpl::new("first".into())));
        r.register(Arc::new(TavilyBackendImpl::new("second".into())));
        assert_eq!(r.len(), 1);
    }
}
