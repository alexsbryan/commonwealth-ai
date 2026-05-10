//! URL / body interpolation for the [`HttpApi`](super::HttpApiAcquirer) acquirer.
//!
//! Two responsibilities:
//!
//! 1. Substitute `{name}` placeholders in a string against a binding
//!    map (`{base_url}` is reserved for the acquirer's base URL).
//! 2. Expand a `for_each` declaration on a [`RequestTemplate`] into
//!    one binding map per cartesian-product point, given the
//!    user-supplied [`ResolvedParameters`].
//!
//! Both are pure — no I/O, no async — so they're trivially testable
//! without standing up a fake HTTP server.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;

use crate::error::{Error, Result};
use crate::recipe::ResolvedParameters;

/// Lazily-compiled placeholder regex. `{name}` where `name` is a
/// standard identifier; nested or escaped braces are out of scope.
fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{([a-zA-Z_][a-zA-Z0-9_]*)\}").unwrap())
}

/// Render a string template by substituting every `{name}` against
/// `bindings`, with the special `{base_url}` resolving to the
/// acquirer's `base_url`. Missing placeholders surface as a single
/// recipe-level error listing all unresolved names — far easier to
/// debug than failing on the first miss only to hit another on
/// retry.
pub fn render_template(
    template: &str,
    base_url: &str,
    bindings: &BTreeMap<String, String>,
) -> Result<String> {
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
        return Err(Error::Recipe(format!(
            "URL template `{}` references undeclared placeholder(s): {}",
            template,
            missing
                .iter()
                .map(|n| format!("{{{n}}}"))
                .collect::<Vec<_>>()
                .join(", "),
        )));
    }
    Ok(result.to_string())
}

/// Expand a `for_each` declaration into one binding map per
/// cartesian-product point.
///
/// Semantics:
///
/// - When `for_each` is empty, every parameter renders to its
///   `as_interpolation()` form (lists join with commas) and the
///   function returns a single binding.
/// - When `for_each` is non-empty, every listed parameter contributes
///   one axis of iteration; the binding for that name on each
///   iteration is the single token (a list element). Non-listed
///   parameters keep their full as-interpolation form.
///
/// This lets a recipe author write:
///
/// ```toml
/// [parameters.entity]
/// type = "list"
/// [parameters.start_date]
/// type = "date"
///
/// url = "?q={entity}&from={start_date}"
/// for_each = ["entity"]
/// ```
///
/// and get one paginated request per entity, with `{start_date}`
/// the same string in each.
pub fn for_each_bindings(
    for_each: &[String],
    parameters: &ResolvedParameters,
) -> Result<Vec<BTreeMap<String, String>>> {
    // Base bindings: every parameter rendered as-interpolation (lists
    // become comma-joined). Iteration axes overwrite their own entry
    // per-binding below.
    let mut base: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in &parameters.values {
        base.insert(name.clone(), value.as_interpolation());
    }

    if for_each.is_empty() {
        return Ok(vec![base]);
    }

    // Resolve each `for_each` parameter to its tokens (one element
    // per iteration). Bail loudly if a referenced name is unknown
    // or empty.
    let mut axes: Vec<(String, Vec<String>)> = Vec::with_capacity(for_each.len());
    for name in for_each {
        let value = parameters.values.get(name).ok_or_else(|| {
            Error::Recipe(format!(
                "request `for_each` references unknown parameter `{name}` \
                 (declared parameters: [{}])",
                parameters
                    .values
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        let tokens = value.iter_tokens();
        if tokens.is_empty() {
            return Err(Error::Recipe(format!(
                "request `for_each` parameter `{name}` resolved to zero values; \
                 nothing to iterate (provide at least one value at install time)"
            )));
        }
        axes.push((name.clone(), tokens));
    }

    // Cartesian product. Start with one binding (the base) and
    // multiply by each axis in turn — this keeps the semantics
    // correct when len(axes) == 1.
    let mut bindings: Vec<BTreeMap<String, String>> = vec![base];
    for (name, tokens) in axes {
        let mut next = Vec::with_capacity(bindings.len() * tokens.len());
        for b in &bindings {
            for token in &tokens {
                let mut clone = b.clone();
                clone.insert(name.clone(), token.clone());
                next.push(clone);
            }
        }
        bindings = next;
    }
    Ok(bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{ParameterValue, ResolvedParameters};

    fn params(items: Vec<(&str, ParameterValue)>) -> ResolvedParameters {
        let mut values = BTreeMap::new();
        for (k, v) in items {
            values.insert(k.into(), v);
        }
        ResolvedParameters { values }
    }

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
        let msg = format!("{err}");
        assert!(msg.contains("{a}"), "{msg}");
        assert!(msg.contains("{b}"), "{msg}");
        assert!(msg.contains("{c}"), "{msg}");
    }

    #[test]
    fn for_each_empty_returns_single_binding_with_lists_joined() {
        let p = params(vec![
            (
                "entities",
                ParameterValue::List(vec!["NVDA".into(), "MSFT".into()]),
            ),
            ("start_date", ParameterValue::Date("2022-01-01".into())),
        ]);
        let bindings = for_each_bindings(&[], &p).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0]["entities"], "NVDA,MSFT");
        assert_eq!(bindings[0]["start_date"], "2022-01-01");
    }

    #[test]
    fn for_each_cartesian_product_two_axes() {
        let p = params(vec![
            (
                "entity",
                ParameterValue::List(vec!["NVDA".into(), "MSFT".into()]),
            ),
            (
                "form_type",
                ParameterValue::List(vec!["10-K".into(), "10-Q".into()]),
            ),
            ("start_date", ParameterValue::Date("2022-01-01".into())),
        ]);
        let bindings = for_each_bindings(
            &["entity".into(), "form_type".into()],
            &p,
        )
        .unwrap();
        assert_eq!(bindings.len(), 4); // 2 × 2

        // Every binding carries start_date intact.
        for b in &bindings {
            assert_eq!(b["start_date"], "2022-01-01");
        }

        // Verify the cross-product covers all combinations.
        let combos: std::collections::BTreeSet<(String, String)> = bindings
            .iter()
            .map(|b| (b["entity"].clone(), b["form_type"].clone()))
            .collect();
        assert!(combos.contains(&("NVDA".into(), "10-K".into())));
        assert!(combos.contains(&("NVDA".into(), "10-Q".into())));
        assert!(combos.contains(&("MSFT".into(), "10-K".into())));
        assert!(combos.contains(&("MSFT".into(), "10-Q".into())));
    }

    #[test]
    fn for_each_unknown_parameter_errors() {
        let p = params(vec![("entity", ParameterValue::List(vec!["X".into()]))]);
        let err = for_each_bindings(&["nope".into()], &p).unwrap_err();
        assert!(format!("{err}").contains("nope"));
    }

    #[test]
    fn for_each_empty_list_errors() {
        let p = params(vec![("entity", ParameterValue::List(vec![]))]);
        let err = for_each_bindings(&["entity".into()], &p).unwrap_err();
        assert!(format!("{err}").contains("zero values"));
    }
}
