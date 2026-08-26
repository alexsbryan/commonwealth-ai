// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ONE construction of the operator's web-search registry.
//!
//! Before this existed there were four of them — the deep-research
//! port, two in the desktop's state, and one in the conversation tool
//! builder — each with its own spelling of "DuckDuckGo always, the
//! keyed provider when configured". They read different config, too:
//! the loop read `SVRNMESH_TAVILY_API_KEY` while the desktop read its
//! own `[search_backend]` block, so a provider configured in the app
//! was invisible to a run. One decider, one name (§10.6).

use std::sync::Arc;

use sovereign_contracts::setup_config::SearchSection;

use super::backend_trait::{
    BraveBackendImpl, DuckDuckGoBackendImpl, TavilyBackendImpl, WebSearchRegistry,
};

/// The zero-config backend. Always registered, never needs a key, and
/// the answer whenever the operator's preference is absent or unkeyed.
pub const FALLBACK_BACKEND: &str = "duckduckgo";

/// A built registry plus the backend id the orchestrator should prefer.
pub struct ConfiguredSearch {
    pub registry: WebSearchRegistry,
    /// The operator's provider when it is both named and keyed;
    /// [`FALLBACK_BACKEND`] otherwise. This is the ONE source of that
    /// choice — no caller re-derives it from key presence.
    pub preferred: String,
}

/// Build the registry from the operator's `[search]` section.
///
/// `env_api_key` is the older `SVRNMESH_TAVILY_API_KEY` path, passed in
/// rather than read here so the env read stays declared at its call site
/// (`quality/env-flags.toml`) and this function stays testable. The
/// config section wins when both are present.
///
/// A named-but-unkeyed provider is NOT silently downgraded in secret —
/// `preferred` comes back as the fallback, which is what the caller
/// reports (§18.3: absence is reported, never defaulted).
pub fn configured_search(cfg: &SearchSection, env_api_key: Option<&str>) -> ConfiguredSearch {
    let mut registry = WebSearchRegistry::new();
    registry.register(Arc::new(DuckDuckGoBackendImpl::new()));

    let key = cfg
        .api_key
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(env_api_key.filter(|s| !s.is_empty()));

    // An empty provider with a key present still means Tavily: that is
    // exactly the shape of an operator who set only the env var, and
    // the env var names Tavily in its own key.
    let named = match (cfg.provider.as_str(), key) {
        ("tavily", Some(k)) => {
            registry.register(Arc::new(TavilyBackendImpl::new(k.to_string())));
            Some("tavily")
        }
        ("brave", Some(k)) => {
            registry.register(Arc::new(BraveBackendImpl::new(k.to_string())));
            Some("brave")
        }
        ("", Some(k)) => {
            registry.register(Arc::new(TavilyBackendImpl::new(k.to_string())));
            Some("tavily")
        }
        _ => None,
    };

    ConfiguredSearch {
        registry,
        preferred: named.unwrap_or(FALLBACK_BACKEND).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(provider: &str, api_key: Option<&str>) -> SearchSection {
        SearchSection {
            provider: provider.to_string(),
            api_key: api_key.map(str::to_string),
        }
    }

    #[test]
    fn no_config_and_no_env_is_the_zero_config_fallback() {
        let c = configured_search(&section("", None), None);
        assert_eq!(c.preferred, FALLBACK_BACKEND);
    }

    /// The defect this whole section exists to fix: a provider the
    /// operator configured in ONE surface must be the provider a run
    /// uses. Watched red before `[search]` existed — the loop could not
    /// see the desktop's block at all.
    #[test]
    fn the_configured_provider_is_the_preferred_one() {
        assert_eq!(
            configured_search(&section("brave", Some("k")), None).preferred,
            "brave"
        );
        assert_eq!(
            configured_search(&section("tavily", Some("k")), None).preferred,
            "tavily"
        );
    }

    /// A named provider with no key cannot serve. It reports the
    /// fallback rather than pretending the preference took effect.
    #[test]
    fn a_named_but_unkeyed_provider_reports_the_fallback() {
        assert_eq!(
            configured_search(&section("brave", None), None).preferred,
            FALLBACK_BACKEND
        );
        assert_eq!(
            configured_search(&section("tavily", Some("")), None).preferred,
            FALLBACK_BACKEND
        );
    }

    /// The older env-var path keeps working with no config at all.
    #[test]
    fn the_env_key_alone_still_selects_tavily() {
        assert_eq!(
            configured_search(&section("", None), Some("env-key")).preferred,
            "tavily"
        );
    }

    /// Config wins over env — one surface is authoritative, and which
    /// one is not left to whichever call site happens to run.
    #[test]
    fn config_wins_over_the_env_key() {
        let c = configured_search(&section("brave", Some("cfg-key")), Some("env-key"));
        assert_eq!(c.preferred, "brave");
    }
}
