// SPDX-License-Identifier: AGPL-3.0-or-later
//! `{name}` placeholder substitution for URL / body templates.
//!
//! The pure substitution half of what used to be
//! `corpus_engine::acquirers::http_api::template`: given a template string and a
//! `{name} -> value` binding map (with `{base_url}` reserved for the acquirer's
//! base URL), render the string, reporting every unresolved placeholder at once.
//!
//! Housed here so both the `http_api` acquirer and the recipe-author `probe_url`
//! tool render templates through one implementation — the recipe-author stack
//! reaches it without a `corpus-engine` dependency. corpus-engine wraps this at
//! `corpus_engine::acquirers::http_api::template::render_template`, mapping the
//! `String` error into its own `Error::Recipe` so its callers are unchanged; the
//! `for_each` cartesian-product expansion (which needs corpus-engine's
//! `ResolvedParameters`) stays there.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;

/// Lazily-compiled placeholder regex. `{name}` where `name` is a
/// standard identifier; nested or escaped braces are out of scope.
fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{([a-zA-Z_][a-zA-Z0-9_]*)\}").unwrap())
}

/// Render a string template by substituting every `{name}` against
/// `bindings`, with the special `{base_url}` resolving to the
/// acquirer's `base_url`. Missing placeholders surface as a single
/// error message listing all unresolved names — far easier to debug
/// than failing on the first miss only to hit another on retry.
///
/// Returns `Err(message)` (a plain string) on unresolved placeholders; callers
/// categorize it into their own error type (corpus-engine → `Error::Recipe`,
/// the `probe_url` tool → `Error::InvalidInput`).
pub fn render_template(
    template: &str,
    base_url: &str,
    bindings: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut missing: Vec<String> = Vec::new();
    let result = placeholder_re().replace_all(template, |caps: &regex::Captures| {
        let name = &caps[1];
        if name == "base_url" {
            return base_url.to_string();
        }
        match bindings.get(name) {
            Some(v) => v.clone(),
            None => {
                missing.push(name.to_string());
                String::new()
            }
        }
    });
    if !missing.is_empty() {
        // Dedup so the same placeholder named twice doesn't appear
        // twice in the error.
        missing.sort();
        missing.dedup();
        return Err(format!(
            "URL template `{}` references undeclared placeholder(s): {}",
            template,
            missing
                .iter()
                .map(|n| format!("{{{n}}}"))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    Ok(result.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_base_url_and_named_bindings() {
        let mut bindings = BTreeMap::new();
        bindings.insert("entity".into(), "NVDA".into());
        bindings.insert("start_date".into(), "2022-01-01".into());

        let url = render_template(
            "{base_url}?q={entity}&from={start_date}",
            "https://example.com",
            &bindings,
        )
        .unwrap();
        assert_eq!(url, "https://example.com?q=NVDA&from=2022-01-01");
    }

    #[test]
    fn render_reports_all_missing_placeholders() {
        let bindings = BTreeMap::new();
        let err = render_template("{a}/{b}/{c}", "", &bindings).unwrap_err();
        assert!(err.contains("{a}"), "{err}");
        assert!(err.contains("{b}"), "{err}");
        assert!(err.contains("{c}"), "{err}");
    }
}
