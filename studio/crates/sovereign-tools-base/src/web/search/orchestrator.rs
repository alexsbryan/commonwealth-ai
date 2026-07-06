// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search orchestrator — picks a backend per call, respects privacy
//! and budget, emits tracing, handles fallback.
//!
//! The orchestrator is where the policy lives. The trait
//! (`WebSearchBackend`) says nothing about caching, retry, budget,
//! or selection — those are orchestrator concerns per ARCH §5.1
//! (interface segregation). The registry (`WebSearchRegistry`)
//! holds the open set of backends; the orchestrator filters and
//! orders that set for each request.
//!
//! Selection algorithm (per the integration plan §Phase 2):
//!   1. Start with every backend in the registry.
//!   2. Filter: drop backends whose `privacy().rank()` exceeds the
//!      request's `max_privacy.rank()`. This is the structural
//!      privacy gate per ARCH §7.1 — a `LocalOnly` request *cannot
//!      reach* an External backend because it's never in the
//!      candidate set.
//!   3. Filter: drop External backends whose remaining budget is 0.
//!      Local backends are exempt (they have no cost).
//!   4. Order by the operator's preference list. Backends not
//!      explicitly preferred sort to the end in registry order.
//!   5. Try each in turn. On `Err`, log + fall through. On `Ok`,
//!      return immediately.
//!   6. If every candidate failed (or there were none), return a
//!      synthetic 0-results response with `warn!` per ARCH §9.2.
//!      This is recoverable degradation — the model gets to say
//!      "no results found" rather than the request hard-failing.
//!
//! Every selection emits a tracing event per ARCH §9.1 so the
//! operator can answer "why did this query go to backend X instead
//! of Y?" from `tracing=debug` alone.

use std::collections::HashMap;
use std::sync::Arc;

use super::backend_trait::{SearchPrivacy, WebSearchBackend, WebSearchRegistry};
use super::SearchResult;

/// Per-backend remaining budget. Keys are backend ids
/// (`"tavily"`, `"brave"`, …). Local backends don't appear
/// (they're not budget-gated).
///
/// The orchestrator only reads this; the budget store
/// (a separate concern — SQLite-backed counter that resets daily)
/// owns the writes. Phase 2 ships the read interface; the budget
/// store lives in its own follow-up so the orchestrator's seams
/// stay testable without the storage layer.
#[derive(Debug, Clone, Default)]
pub struct BudgetView {
    remaining: HashMap<String, u32>,
}

impl BudgetView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test/fixture helper: every external backend at zero.
    /// Drives the "all-external-drop" invariant test.
    pub fn all_zero() -> Self {
        let mut v = Self::new();
        v.remaining.insert("tavily".into(), 0);
        v.remaining.insert("brave".into(), 0);
        v.remaining.insert("duckduckgo".into(), 0);
        v
    }

    pub fn set(&mut self, backend_id: &str, units: u32) {
        self.remaining.insert(backend_id.to_string(), units);
    }

    /// Remaining units for a backend. Returns `None` when the
    /// backend isn't tracked (local backends, or external backends
    /// the operator hasn't configured a budget for — in which case
    /// the orchestrator treats them as unlimited).
    pub fn remaining(&self, backend_id: &str) -> Option<u32> {
        self.remaining.get(backend_id).copied()
    }
}

/// Inputs to one selection call. Built per-request from:
/// - the agent loop's tool-call args (query, max_results)
/// - the request's OICP privacy posture (max_privacy)
/// - the daemon-wide budget view (budget)
/// - the operator config (`default_backends.toml` — Phase 4)
#[derive(Debug)]
pub struct SelectInputs<'a> {
    pub query: &'a str,
    pub max_results: usize,
    pub max_privacy: SearchPrivacy,
    pub budget: &'a BudgetView,
    /// Operator preference order. Backends listed earlier are tried
    /// first. Backends not listed sort to the end (registry-arbitrary
    /// order among them). Empty slice = no preference, use registry
    /// iteration order.
    pub prefer: &'a [&'a str],
}

/// Result of one orchestrated search. Carries the chosen backend's
/// id (for tracing + auditing) alongside the actual results. When
/// every backend failed and we synthesized the 0-results
/// degradation response, `backend_id` is `"none"` and `results` is
/// empty — callers should still treat this as `Ok`, not a hard
/// failure (per the algorithm step 6).
#[derive(Debug)]
pub struct OrchestratedSearch {
    pub backend_id: String,
    pub results: Vec<SearchResult>,
}

/// The orchestrator. Holds the registry by `Arc` so it can be
/// cloned cheaply into the per-request agent loop state.
#[derive(Clone)]
pub struct SearchOrchestrator {
    registry: Arc<WebSearchRegistry>,
}

impl SearchOrchestrator {
    pub fn new(registry: Arc<WebSearchRegistry>) -> Self {
        Self { registry }
    }

    /// Run one search call. Per the algorithm: filter → order →
    /// try in fallback chain → synthetic 0-results on total
    /// failure. Always returns `Ok` — backend failure does not
    /// propagate to the caller, it manifests as `results: vec![]`.
    pub async fn search(
        &self,
        client: &reqwest::Client,
        inputs: SelectInputs<'_>,
    ) -> OrchestratedSearch {
        let candidates = self.candidates(&inputs);
        let candidate_ids: Vec<&'static str> = candidates.iter().map(|b| b.id()).collect();

        if candidates.is_empty() {
            tracing::warn!(
                query_len = inputs.query.len(),
                max_privacy = ?inputs.max_privacy,
                "search: no candidate backends survived privacy + budget filter \
                 — returning synthetic 0-results"
            );
            return OrchestratedSearch {
                backend_id: "none".into(),
                results: Vec::new(),
            };
        }

        for backend in &candidates {
            let chosen_id = backend.id();
            tracing::debug!(
                backend = %chosen_id,
                candidates = ?candidate_ids,
                max_privacy = ?inputs.max_privacy,
                "search: backend selected"
            );
            match backend
                .search(client, inputs.query, inputs.max_results)
                .await
            {
                Ok(results) => {
                    return OrchestratedSearch {
                        backend_id: chosen_id.to_string(),
                        results,
                    };
                }
                Err(e) => {
                    tracing::warn!(
                        backend = %chosen_id,
                        error = %e,
                        "search: backend failed — falling through"
                    );
                    continue;
                }
            }
        }

        tracing::warn!(
            candidates = ?candidate_ids,
            "search: every candidate backend failed — returning synthetic 0-results"
        );
        OrchestratedSearch {
            backend_id: "none".into(),
            results: Vec::new(),
        }
    }

    /// Filter + order the registry's backends for the given inputs.
    /// Exposed for testing the selection-policy invariants without
    /// running the actual `search()` HTTP path.
    pub fn candidates(&self, inputs: &SelectInputs<'_>) -> Vec<Arc<dyn WebSearchBackend>> {
        let max_rank = inputs.max_privacy.rank();
        let mut surviving: Vec<Arc<dyn WebSearchBackend>> = self
            .registry
            .iter()
            .filter(|b| b.privacy().rank() <= max_rank)
            .filter(|b| budget_allows(b.as_ref(), inputs.budget))
            .cloned()
            .collect();

        // Order by operator preference. Backends in `prefer` come
        // first in the order listed; backends not in `prefer` sort
        // after them (registry-arbitrary, but stable within a run
        // because we're sorting by an Option<usize> position).
        surviving.sort_by_key(|b| {
            inputs
                .prefer
                .iter()
                .position(|&p| p == b.id())
                .map(|p| (0, p))
                .unwrap_or((1, 0))
        });
        surviving
    }
}

/// "Is this backend's budget non-zero (or untracked)?" — the
/// orchestrator's gate. Local backends always pass; external
/// backends pass if either (a) no budget entry exists (untracked
/// = unlimited per `BudgetView::remaining` contract) or (b) the
/// entry is > 0.
fn budget_allows(backend: &dyn WebSearchBackend, budget: &BudgetView) -> bool {
    if !matches!(backend.privacy(), SearchPrivacy::External { .. }) {
        return true;
    }
    match budget.remaining(backend.id()) {
        Some(0) => false,
        Some(_) | None => true,
    }
}

// ─── Tests — pin the orchestrator's invariants per ARCH §7.2 ───

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::search::backend_trait::{MockBackendImpl, SearchCost};
    use async_trait::async_trait;
    use sovereign_contracts::error::Error;

    /// Test stub: a configurable backend. Returns a fixed result
    /// set on success, or a tagged error string. Doesn't hit the
    /// network. Used to drive the orchestrator-policy invariants
    /// without depending on the real backend impls' HTTP behavior.
    struct StubBackend {
        id: &'static str,
        privacy: SearchPrivacy,
        cost: Option<SearchCost>,
        outcome: StubOutcome,
    }

    #[derive(Clone)]
    enum StubOutcome {
        Ok(Vec<SearchResult>),
        Err(String),
    }

    #[async_trait]
    impl WebSearchBackend for StubBackend {
        async fn search(
            &self,
            _client: &reqwest::Client,
            _query: &str,
            _max_results: usize,
        ) -> Result<Vec<SearchResult>, Error> {
            match &self.outcome {
                StubOutcome::Ok(r) => Ok(r.clone()),
                StubOutcome::Err(e) => Err(Error::Execution(e.clone())),
            }
        }
        fn id(&self) -> &'static str {
            self.id
        }
        fn privacy(&self) -> SearchPrivacy {
            self.privacy
        }
        fn cost_estimate(&self) -> Option<SearchCost> {
            self.cost
        }
    }

    fn one_result(url: &str) -> SearchResult {
        SearchResult {
            title: format!("title for {url}"),
            url: url.into(),
            snippet: "snippet".into(),
        }
    }

    fn registry_with(backends: Vec<Arc<dyn WebSearchBackend>>) -> Arc<WebSearchRegistry> {
        let mut r = WebSearchRegistry::new();
        for b in backends {
            r.register(b);
        }
        Arc::new(r)
    }

    #[tokio::test]
    async fn local_only_request_never_reaches_external_backend() {
        // ARCH §7 structural privacy invariant. A LocalOnly request
        // must not even have an External backend in its candidate
        // set — let alone dispatch to one.
        let tavily = StubBackend {
            id: "tavily",
            privacy: SearchPrivacy::External { provider: "tavily" },
            cost: Some(SearchCost {
                units_per_call: 1,
                denomination: "tavily-credits",
            }),
            outcome: StubOutcome::Ok(vec![one_result("https://tavily/x")]),
        };
        let mock = StubBackend {
            id: "mock",
            privacy: SearchPrivacy::Local,
            cost: None,
            outcome: StubOutcome::Ok(vec![one_result("https://local/x")]),
        };
        let registry = registry_with(vec![Arc::new(tavily), Arc::new(mock)]);
        let orch = SearchOrchestrator::new(registry);

        let budget = BudgetView::new();
        let cands = orch.candidates(&SelectInputs {
            query: "anything",
            max_results: 5,
            max_privacy: SearchPrivacy::Local,
            budget: &budget,
            prefer: &[],
        });
        let ids: Vec<&str> = cands.iter().map(|b| b.id()).collect();
        assert!(
            !ids.contains(&"tavily"),
            "tavily must be filtered out when max_privacy=Local: {ids:?}"
        );
        assert!(ids.contains(&"mock"));
    }

    #[tokio::test]
    async fn external_request_includes_external_backends() {
        let tavily = StubBackend {
            id: "tavily",
            privacy: SearchPrivacy::External { provider: "tavily" },
            cost: Some(SearchCost {
                units_per_call: 1,
                denomination: "tavily-credits",
            }),
            outcome: StubOutcome::Ok(vec![one_result("https://tavily/x")]),
        };
        let registry = registry_with(vec![Arc::new(tavily)]);
        let orch = SearchOrchestrator::new(registry);

        let budget = BudgetView::new(); // untracked = unlimited
        let cands = orch.candidates(&SelectInputs {
            query: "anything",
            max_results: 5,
            max_privacy: SearchPrivacy::External { provider: "any" },
            budget: &budget,
            prefer: &[],
        });
        let ids: Vec<&str> = cands.iter().map(|b| b.id()).collect();
        assert_eq!(ids, vec!["tavily"]);
    }

    #[tokio::test]
    async fn external_drop_when_budget_zero() {
        let tavily = StubBackend {
            id: "tavily",
            privacy: SearchPrivacy::External { provider: "tavily" },
            cost: Some(SearchCost {
                units_per_call: 1,
                denomination: "tavily-credits",
            }),
            outcome: StubOutcome::Ok(vec![one_result("https://tavily/x")]),
        };
        let registry = registry_with(vec![Arc::new(tavily)]);
        let orch = SearchOrchestrator::new(registry);

        let budget = BudgetView::all_zero();
        let cands = orch.candidates(&SelectInputs {
            query: "anything",
            max_results: 5,
            max_privacy: SearchPrivacy::External { provider: "any" },
            budget: &budget,
            prefer: &[],
        });
        assert!(
            cands.is_empty(),
            "all-zero budget must drop every external backend"
        );
    }

    #[tokio::test]
    async fn local_backend_unaffected_by_budget() {
        // Local backends have no cost — they should never be
        // filtered out by the budget gate, even when "all zero".
        let mock = StubBackend {
            id: "mock",
            privacy: SearchPrivacy::Local,
            cost: None,
            outcome: StubOutcome::Ok(vec![one_result("https://local/x")]),
        };
        let registry = registry_with(vec![Arc::new(mock)]);
        let orch = SearchOrchestrator::new(registry);

        let budget = BudgetView::all_zero();
        let cands = orch.candidates(&SelectInputs {
            query: "anything",
            max_results: 5,
            max_privacy: SearchPrivacy::External { provider: "any" },
            budget: &budget,
            prefer: &[],
        });
        let ids: Vec<&str> = cands.iter().map(|b| b.id()).collect();
        assert_eq!(ids, vec!["mock"]);
    }

    #[tokio::test]
    async fn preference_order_honored() {
        let brave = StubBackend {
            id: "brave",
            privacy: SearchPrivacy::External { provider: "brave" },
            cost: Some(SearchCost {
                units_per_call: 1,
                denomination: "brave-queries",
            }),
            outcome: StubOutcome::Ok(vec![]),
        };
        let tavily = StubBackend {
            id: "tavily",
            privacy: SearchPrivacy::External { provider: "tavily" },
            cost: Some(SearchCost {
                units_per_call: 1,
                denomination: "tavily-credits",
            }),
            outcome: StubOutcome::Ok(vec![]),
        };
        let registry = registry_with(vec![Arc::new(brave), Arc::new(tavily)]);
        let orch = SearchOrchestrator::new(registry);

        let budget = BudgetView::new();
        let cands = orch.candidates(&SelectInputs {
            query: "anything",
            max_results: 5,
            max_privacy: SearchPrivacy::External { provider: "any" },
            budget: &budget,
            // Operator prefers tavily first, then brave.
            prefer: &["tavily", "brave"],
        });
        let ids: Vec<&str> = cands.iter().map(|b| b.id()).collect();
        assert_eq!(ids, vec!["tavily", "brave"]);
    }

    #[tokio::test]
    async fn fallback_chain_skips_failing_backend() {
        let failing = StubBackend {
            id: "tavily",
            privacy: SearchPrivacy::External { provider: "tavily" },
            cost: Some(SearchCost {
                units_per_call: 1,
                denomination: "tavily-credits",
            }),
            outcome: StubOutcome::Err("quota exceeded".into()),
        };
        let ok = StubBackend {
            id: "brave",
            privacy: SearchPrivacy::External { provider: "brave" },
            cost: Some(SearchCost {
                units_per_call: 1,
                denomination: "brave-queries",
            }),
            outcome: StubOutcome::Ok(vec![one_result("https://brave/result")]),
        };
        let registry = registry_with(vec![Arc::new(failing), Arc::new(ok)]);
        let orch = SearchOrchestrator::new(registry);

        let client = reqwest::Client::new();
        let budget = BudgetView::new();
        let out = orch
            .search(
                &client,
                SelectInputs {
                    query: "x",
                    max_results: 5,
                    max_privacy: SearchPrivacy::External { provider: "any" },
                    budget: &budget,
                    prefer: &["tavily", "brave"],
                },
            )
            .await;
        assert_eq!(out.backend_id, "brave", "should fall through to brave");
        assert_eq!(out.results.len(), 1);
    }

    #[tokio::test]
    async fn all_failing_returns_synthetic_zero_results() {
        // Recoverable degradation per algorithm step 6 — no caller-
        // visible failure when every backend errors. The model gets
        // to render "no results found" cleanly.
        let failing = StubBackend {
            id: "tavily",
            privacy: SearchPrivacy::External { provider: "tavily" },
            cost: Some(SearchCost {
                units_per_call: 1,
                denomination: "tavily-credits",
            }),
            outcome: StubOutcome::Err("backend down".into()),
        };
        let registry = registry_with(vec![Arc::new(failing)]);
        let orch = SearchOrchestrator::new(registry);

        let client = reqwest::Client::new();
        let budget = BudgetView::new();
        let out = orch
            .search(
                &client,
                SelectInputs {
                    query: "x",
                    max_results: 5,
                    max_privacy: SearchPrivacy::External { provider: "any" },
                    budget: &budget,
                    prefer: &[],
                },
            )
            .await;
        assert_eq!(out.backend_id, "none");
        assert!(out.results.is_empty());
    }

    #[tokio::test]
    async fn empty_registry_returns_synthetic_zero_results() {
        let registry = Arc::new(WebSearchRegistry::new());
        let orch = SearchOrchestrator::new(registry);
        let client = reqwest::Client::new();
        let budget = BudgetView::new();
        let out = orch
            .search(
                &client,
                SelectInputs {
                    query: "x",
                    max_results: 5,
                    max_privacy: SearchPrivacy::External { provider: "any" },
                    budget: &budget,
                    prefer: &[],
                },
            )
            .await;
        assert_eq!(out.backend_id, "none");
        assert!(out.results.is_empty());
    }

    // Spot-check: the real MockBackendImpl resolves via the
    // orchestrator path the same way it does via legacy dispatch.
    // This is the gym-parity proof — once the gym migrates (later
    // phase), this path is what it'll use.
    #[tokio::test]
    async fn real_mock_backend_works_through_orchestrator() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Write an aliases.toml + one fixture so the mock resolves.
        std::fs::write(
            tmp.path().join("aliases.toml"),
            r#"
[[entry]]
file = "x.json"
aliases = ["test query"]
"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("x.json"),
            r#"{"query":"test query","results":[{"title":"T","url":"https://e.test/","snippet":"s"}]}"#,
        )
        .unwrap();

        let mock = MockBackendImpl::new(tmp.path().to_path_buf());
        let mut r = WebSearchRegistry::new();
        r.register(Arc::new(mock));
        let orch = SearchOrchestrator::new(Arc::new(r));
        let client = reqwest::Client::new();
        let budget = BudgetView::new();
        let out = orch
            .search(
                &client,
                SelectInputs {
                    query: "test query",
                    max_results: 5,
                    max_privacy: SearchPrivacy::Local,
                    budget: &budget,
                    prefer: &[],
                },
            )
            .await;
        assert_eq!(out.backend_id, "mock");
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].url, "https://e.test/");
    }
}
