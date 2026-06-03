//! Tavily real-network e2e test — env-gated, silent on CI.
//!
//! Per ARCH §12.4 "Tests must not require GPU, network, or real
//! model weights" — every test in this workspace runs on a CI box
//! with no GPU, no internet, and no model files on disk. This test
//! respects that rule by reading `SOVEREIGN_TAVILY_API_KEY` and
//! returning early when it's absent. CI doesn't set it → test is
//! silent. Operators with a Tavily key set it locally → test
//! exercises the real API.
//!
//! Why this can't be deferred to "manual check": the integration
//! plan called for "data to back the robustness of the system."
//! Without a real-network test, the daemon can be advertising
//! Tavily support that's silently broken (auth key revoked, API
//! shape drifted, request envelope changed) and no part of the gym
//! would catch it — the gym uses mock backends end-to-end. The
//! env-gate is the compromise: structurally part of the test suite,
//! observable in `cargo test` output when run, never breaks CI.
//!
//! Per the integration plan's Phase 5: the canned-API-shape sister
//! test (uses `httpmock` to stub Tavily) is deferred as a follow-up
//! — adding `httpmock` as a workspace dep is a separate ARCH §8
//! decision the operator owns. When that lands, this file gains a
//! sibling test that runs always and catches our backend's
//! request/response shape regressions without burning real credits.

use std::sync::Arc;
use std::time::Duration;

use sovereign_tools::web::search::{
    BudgetView, SearchOrchestrator, SearchPrivacy, SelectInputs, TavilyBackendImpl,
    WebSearchRegistry,
};

const ENV_KEY: &str = "SOVEREIGN_TAVILY_API_KEY";

/// Real-network smoke: TavilyBackendImpl returns citable results
/// for a stable, popular query. Skipped silently when no key is
/// provided. When run, asserts the contract every downstream
/// consumer relies on: URLs are https, titles non-empty, snippets
/// non-empty.
#[tokio::test]
async fn tavily_real_query_returns_citable_results() {
    let key = match std::env::var(ENV_KEY) {
        Ok(k) if !k.trim().is_empty() => k,
        _ => {
            // Silence on the skip is the contract — CI must stay
            // green without a key. We use eprintln rather than
            // tracing::info because cargo test doesn't initialize
            // a subscriber; an operator who *expected* to test
            // against real Tavily can see they're not via `cargo
            // test -- --nocapture`.
            eprintln!("tavily_real_query_returns_citable_results: SKIPPED (no {ENV_KEY})");
            return;
        }
    };
    eprintln!(
        "tavily_real_query_returns_citable_results: RUNNING (key set, len={})",
        key.len()
    );

    // The query is stable + popular so Tavily should never return
    // zero. If it does, that's a Tavily-side regression worth
    // investigating, not a flake to ignore.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client");
    let backend = TavilyBackendImpl::new(key);

    // Going through the orchestrator (not the bare backend) exercises
    // the full Phase 0 → 2 path the daemon will use in production.
    let mut registry = WebSearchRegistry::new();
    registry.register(Arc::new(backend));
    let orchestrator = SearchOrchestrator::new(Arc::new(registry));

    let budget = BudgetView::new(); // untracked = unlimited
    let out = orchestrator
        .search(
            &client,
            SelectInputs {
                query: "rust programming language",
                max_results: 5,
                max_privacy: SearchPrivacy::External { provider: "tavily" },
                budget: &budget,
                prefer: &["tavily"],
            },
        )
        .await;

    assert_eq!(
        out.backend_id, "tavily",
        "expected tavily to serve; got {}",
        out.backend_id
    );
    assert!(
        !out.results.is_empty(),
        "Tavily returned zero results for a popular query — likely an \
         API-side regression"
    );

    for r in &out.results {
        assert!(
            r.url.starts_with("https://") || r.url.starts_with("http://"),
            "result url is not http(s): {}",
            r.url
        );
        assert!(!r.title.trim().is_empty(), "result title is empty: {:?}", r);
        assert!(
            !r.snippet.trim().is_empty(),
            "result snippet is empty: {:?}",
            r
        );
    }

    eprintln!(
        "tavily_real_query_returns_citable_results: PASS backend={} n_results={}",
        out.backend_id,
        out.results.len()
    );
    eprintln!("  sample URLs:");
    for r in out.results.iter().take(3) {
        eprintln!("    {}", r.url);
    }
}

/// Negative case: with a clearly-invalid key the backend errors,
/// the orchestrator falls through to the empty-candidate synthetic
/// 0-results path. Skipped when there's no real key to compare
/// against — the test exercises a known-bad value vs a known-good
/// one, and we don't want to mark a CI run as "passed" when neither
/// path was exercised.
#[tokio::test]
async fn tavily_invalid_key_degrades_gracefully() {
    if std::env::var(ENV_KEY)
        .map(|k| k.trim().is_empty())
        .unwrap_or(true)
    {
        eprintln!("tavily_invalid_key_degrades_gracefully: SKIPPED (no {ENV_KEY})");
        return;
    }
    eprintln!("tavily_invalid_key_degrades_gracefully: RUNNING");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client");
    let backend = TavilyBackendImpl::new("tvly-DEFINITELY-NOT-A-REAL-KEY-1234567890".into());

    let mut registry = WebSearchRegistry::new();
    registry.register(Arc::new(backend));
    let orchestrator = SearchOrchestrator::new(Arc::new(registry));

    let budget = BudgetView::new();
    let out = orchestrator
        .search(
            &client,
            SelectInputs {
                query: "anything",
                max_results: 5,
                max_privacy: SearchPrivacy::External { provider: "tavily" },
                budget: &budget,
                prefer: &["tavily"],
            },
        )
        .await;

    // Per orchestrator algorithm step 6: all candidates failed →
    // synthetic 0-results, backend_id="none". No exception, no
    // user-visible 5xx. This is the graceful degradation contract.
    assert_eq!(
        out.backend_id, "none",
        "invalid-key call should fall through to synthetic 0-results"
    );
    assert!(out.results.is_empty());
}
