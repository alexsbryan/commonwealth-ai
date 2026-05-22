//! DuckDuckGo real-network probe — runs always (no key required).
//! Surfaces whether the zero-config fallback the desktop ships with
//! actually returns results in practice.
//!
//! DDG is famously aggressive about bot detection. The existing
//! backend at `sovereign-tools/src/web/search/mod.rs` has fallback
//! cascades (HTML endpoint → Lite endpoint → API path) and explicit
//! `eprintln!` lines noting when DDG returns a blocking page. None
//! of that survives gym scoring, so until this test landed nobody
//! actually knew whether the fallback was load-bearing or
//! decorative.
//!
//! Per ARCH §12.4 normally tests must not require network. This
//! test is permitted because (a) DDG is free and unauthenticated,
//! so running it on CI is acceptable; (b) the failure mode is
//! "skipped with a loud message" not "test failure" — DDG bot-
//! blocking is a real-world condition, not a regression in our code.
//! When DDG IS blocking, the test surfaces that fact via stderr
//! rather than making CI red.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use sovereign_tools::web::search::{
    BudgetView, DuckDuckGoBackendImpl, SearchOrchestrator, SearchPrivacy,
    SelectInputs, WebSearchRegistry,
};

#[tokio::test]
async fn duckduckgo_returns_results_or_surfaces_block() {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client");
    let backend = DuckDuckGoBackendImpl::new();

    let mut registry = WebSearchRegistry::new();
    registry.register(Arc::new(backend));
    let orchestrator = SearchOrchestrator::new(Arc::new(registry));

    let budget = BudgetView::new();
    let out = orchestrator
        .search(
            &client,
            SelectInputs {
                query: "rust programming language",
                max_results: 5,
                max_privacy: SearchPrivacy::External { provider: "duckduckgo" },
                budget: &budget,
                prefer: &["duckduckgo"],
            },
        )
        .await;

    eprintln!(
        "duckduckgo_returns_results_or_surfaces_block: backend={} n_results={}",
        out.backend_id,
        out.results.len()
    );

    if out.results.is_empty() {
        // DDG either blocked us or returned legitimately empty. The
        // distinction matters operationally: blocking means the
        // "free fallback" the desktop ships with is broken; empty
        // for a popular query means something deeper is wrong.
        // Either way, the test does NOT fail — surfacing this as a
        // CI failure would be a false positive for the system; we
        // want operators to know the state, not for the harness to
        // misattribute it.
        eprintln!(
            "  DDG returned zero results for 'rust programming language' \
             — likely bot-blocked. The desktop's zero-config fallback is \
             NOT load-bearing in this environment; configure Tavily or \
             Brave for actual search capability."
        );
        return;
    }

    // DDG did return — pin the same invariants the Tavily test
    // pins. If DDG ever DOES work in CI, regressions in its parser
    // get caught here.
    for r in &out.results {
        assert!(
            r.url.starts_with("https://") || r.url.starts_with("http://"),
            "ddg result url not http(s): {}",
            r.url
        );
        assert!(!r.title.trim().is_empty(), "ddg result title empty: {r:?}");
    }

    // DDG's HTML routinely lists the same canonical URL twice
    // (zero-click info box + organic row); parse_ddg_results dedups
    // at the parser. Regression-pin that here so a future refactor
    // that drops the seen-set doesn't silently leak duplicates into
    // the URL allowlist.
    let unique: HashSet<&str> = out.results.iter().map(|r| r.url.as_str()).collect();
    assert_eq!(
        unique.len(),
        out.results.len(),
        "ddg results contain duplicate URLs: {:?}",
        out.results.iter().map(|r| &r.url).collect::<Vec<_>>()
    );

    eprintln!("  DDG works — sample URLs:");
    for r in out.results.iter().take(3) {
        eprintln!("    {}", r.url);
    }
}
