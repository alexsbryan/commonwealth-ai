//! Search-related text + config assets, loaded at compile time per
//! ARCH §6.2 (data vs. program). The strings + tables here are
//! operator-tunable inputs that change without code changes —
//! moving them out of `.rs` files keeps the grep distance from
//! "where does this string come from?" to "here it is" at one hop.
//!
//! Assets:
//!   - `SYSTEM_PROMPT`        — re-exports
//!                              `crate::search::SEARCH_SYSTEM_PROMPT`.
//!                              The canonical asset lives at
//!                              `sovereign-tools/assets/search_system_prompt.md`
//!                              and predates this module; we re-export
//!                              under the orchestrator's namespace so
//!                              new code reaches for it through
//!                              `web::search::SYSTEM_PROMPT` without
//!                              caring about the legacy `search` module
//!                              that originally owned the asset.
//!   - `TOOL_DESCRIPTION`     — same shape as `SYSTEM_PROMPT`, points at
//!                              `crate::search::SEARCH_TOOL_DESCRIPTION`.
//!   - `default_backends.toml`— NEW in this module. Orchestrator
//!                              selection rules (preference order,
//!                              budget, privacy). Loaded via
//!                              `BackendsConfig::from_default_toml`;
//!                              operator overrides parse the same shape
//!                              from a user-level TOML file.

/// The search-tool system message. Re-exported alias for
/// `crate::search::SEARCH_SYSTEM_PROMPT` so new code rooted in the
/// orchestrator's module has one canonical path. The file itself
/// lives at `sovereign-tools/assets/search_system_prompt.md` per
/// ARCH §6.2.
pub const SYSTEM_PROMPT: &str = crate::search::SEARCH_SYSTEM_PROMPT;

/// The `description` field on the `search` tool definition the
/// daemon advertises to clients. Re-exported alias for
/// `crate::search::SEARCH_TOOL_DESCRIPTION`.
pub const TOOL_DESCRIPTION: &str = crate::search::SEARCH_TOOL_DESCRIPTION;

/// Default orchestrator config — backend preference order, per-
/// backend budgets, privacy floor. Loaded via
/// `BackendsConfig::from_default_toml`; operator overrides parse
/// the same shape from a user-level TOML file.
pub const DEFAULT_BACKENDS_TOML: &str =
    include_str!("assets/default_backends.toml");

/// Operator-tunable orchestrator config. Mirrors the
/// `default_backends.toml` shape one-to-one.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BackendsConfig {
    #[serde(default)]
    pub selection: SelectionConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub privacy: PrivacyConfig,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SelectionConfig {
    /// Backend ids tried in order. Backends not listed sort to the
    /// end in registry-arbitrary order.
    #[serde(default)]
    pub prefer: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct BudgetConfig {
    /// Per-backend daily call budget, keyed by backend id. Absent
    /// entries mean "untracked = unlimited" per the orchestrator's
    /// rule. Backends with `daily_calls = 0` get filtered out of
    /// the candidate set.
    #[serde(flatten)]
    pub per_backend: std::collections::HashMap<String, BudgetEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BudgetEntry {
    pub daily_calls: u32,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct PrivacyConfig {
    /// Default privacy floor; per-request OICP can be tighter but
    /// not looser. Accepted: `"local"`, `"mesh"`, `"external"`.
    #[serde(default = "default_privacy_max")]
    pub default_max: String,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            default_max: default_privacy_max(),
        }
    }
}

fn default_privacy_max() -> String {
    "external".into()
}

impl BackendsConfig {
    /// Parse the default TOML compiled into the binary. Failure is
    /// a build-time bug because the input is `include_str!`-ed —
    /// the panic message names the file so operators can find it.
    pub fn from_default_toml() -> Self {
        toml::from_str(DEFAULT_BACKENDS_TOML)
            .expect("default_backends.toml must parse — this is checked-in data")
    }

    /// Parse from an operator-supplied TOML string. Returns the
    /// parser error rather than panicking — runtime override.
    pub fn parse_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_is_non_empty_and_mentions_citations() {
        assert!(!SYSTEM_PROMPT.is_empty());
        // Pin a load-bearing instruction from the canonical prompt.
        // Changing this is a behavior change that should be a
        // deliberate edit to the asset, not silent drift.
        assert!(SYSTEM_PROMPT.contains("character-for-character"),
                "system prompt must instruct verbatim URL citation");
        assert!(SYSTEM_PROMPT.contains("zero results"),
                "system prompt must instruct honest-empty behavior");
    }

    #[test]
    fn tool_description_is_non_empty() {
        assert!(!TOOL_DESCRIPTION.is_empty());
        assert!(TOOL_DESCRIPTION.contains("Cite the URL"),
                "tool description must include the citation directive");
    }

    #[test]
    fn default_backends_toml_parses() {
        let cfg = BackendsConfig::from_default_toml();
        assert!(!cfg.selection.prefer.is_empty(),
                "default config must declare a preference order");
        assert!(cfg.selection.prefer.contains(&"tavily".into()),
                "tavily must be in default preference list");
        assert!(cfg.budget.per_backend.contains_key("tavily"),
                "tavily must have a default budget entry");
    }

    #[test]
    fn operator_override_parses_independently() {
        // ARCH §6.1: operators can tune without recompile.
        let user = r#"
[selection]
prefer = ["internal-only"]

[budget.tavily]
daily_calls = 0

[privacy]
default_max = "local"
"#;
        let cfg = BackendsConfig::parse_toml(user).expect("user toml parses");
        assert_eq!(cfg.selection.prefer, vec!["internal-only"]);
        assert_eq!(cfg.budget.per_backend["tavily"].daily_calls, 0);
        assert_eq!(cfg.privacy.default_max, "local");
    }

    #[test]
    fn budget_entries_with_zero_are_preserved_as_zero() {
        // The orchestrator's "drop External with budget=0" rule
        // depends on a 0 entry actually surviving parse — sanity
        // check it does (vs. silently coercing to absent).
        let user = r#"
[budget.tavily]
daily_calls = 0
"#;
        let cfg = BackendsConfig::parse_toml(user).unwrap();
        assert_eq!(cfg.budget.per_backend["tavily"].daily_calls, 0);
    }
}
