// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pagination drivers for the HTTP API acquirer.
//!
//! The four [`PaginationStrategy`] variants share a uniform shape:
//! given the response body of the previous page (and the URL we
//! issued for it), produce the URL of the next page — or `None` if
//! the sequence is exhausted. The acquirer's loop is strategy-blind.
//!
//! Each driver is a small, pure function so the strategies are
//! testable in isolation. Live HTTP integration lives in the
//! orchestrator (`mod.rs`) so the same pure logic can be exercised
//! against recorded fixtures or live wiremock.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::recipe::PaginationStrategy;

/// Outcome of consulting a [`PaginationStrategy`] after fetching a
/// page. The orchestrator threads the previous URL through so
/// strategies that need to mutate one query parameter (Offset /
/// PageNumber / Cursor) can do so without re-rendering the template.
pub enum NextPage {
    /// Next page URL.
    Url(String),
    /// No more pages.
    Done,
}

/// Drive one step of pagination. `current_url` is the absolute URL
/// the caller just fetched; `body` is the parsed JSON response.
///
/// `state` carries strategy-specific counters (the page index for
/// PageNumber, the running offset for Offset). Strategies update it
/// in place.
pub fn next_page(
    strategy: &PaginationStrategy,
    current_url: &str,
    body: &Value,
    state: &mut PaginationState,
) -> Result<NextPage> {
    match strategy {
        PaginationStrategy::Offset {
            param,
            page_size,
            items_path,
        } => offset_next(current_url, body, param, *page_size, items_path, state),
        PaginationStrategy::Cursor {
            param,
            response_path,
        } => cursor_next(current_url, body, param, response_path),
        PaginationStrategy::NextUrl { response_path } => next_url_next(body, response_path),
        PaginationStrategy::PageNumber {
            param,
            start: _,
            end,
        } => page_number_next(current_url, *end, param, state),
    }
}

/// Per-strategy mutable state. The orchestrator constructs one and
/// passes it into [`next_page`] each iteration.
#[derive(Debug, Default)]
pub struct PaginationState {
    /// Total items observed on Offset pagination — used to decide
    /// when to stop (sub-`page_size` page = exhausted).
    pub items_seen_this_request: usize,
    /// Current page index, 1-based, for PageNumber strategy.
    pub current_page: usize,
}

fn offset_next(
    current_url: &str,
    body: &Value,
    param: &str,
    page_size: usize,
    items_path: &str,
    state: &mut PaginationState,
) -> Result<NextPage> {
    let count = count_items_at_path(body, items_path)?;
    state.items_seen_this_request = count;

    if count < page_size {
        return Ok(NextPage::Done);
    }

    let prior_offset = parse_query_param_usize(current_url, param).unwrap_or(0);
    let next_offset = prior_offset + page_size;
    Ok(NextPage::Url(set_query_param(
        current_url,
        param,
        &next_offset.to_string(),
    )))
}

/// Count items at a JSONPath. Tolerates two common recipe-author
/// conventions:
///
/// - `$.items` — points at the items *array* itself; the
///   single-match result is `[[item1, item2, ...]]`. We unwrap.
/// - `$.items[*]` — yields every element as its own match; the
///   result is `[item1, item2, ...]`. We use the length directly.
///
/// Both forms naturally read as "the items" in TOML and we don't
/// want to surprise the recipe author with a forced choice.
fn count_items_at_path(body: &Value, items_path: &str) -> Result<usize> {
    use jsonpath_rust::JsonPath;

    let path = JsonPath::try_from(items_path)
        .map_err(|e| Error::Recipe(format!("invalid `items_path` JSONPath `{items_path}`: {e}")))?;
    match path.find(body) {
        Value::Array(arr) => {
            // Single match wrapping an array → unwrap once.
            if arr.len() == 1 {
                if let Some(Value::Array(inner)) = arr.first() {
                    return Ok(inner.len());
                }
            }
            Ok(arr.len())
        }
        _ => Ok(0),
    }
}

fn cursor_next(
    current_url: &str,
    body: &Value,
    param: &str,
    response_path: &str,
) -> Result<NextPage> {
    use jsonpath_rust::JsonPath;

    let path = JsonPath::try_from(response_path).map_err(|e| {
        Error::Recipe(format!(
            "invalid `response_path` JSONPath `{response_path}`: {e}"
        ))
    })?;
    // `find()` always returns a Value (Array of matches); pull the
    // first non-null string.
    let cursor = match path.find(body) {
        Value::Array(arr) => arr.into_iter().find_map(|v| v.as_str().map(String::from)),
        _ => None,
    };
    match cursor {
        Some(c) if !c.is_empty() => Ok(NextPage::Url(set_query_param(current_url, param, &c))),
        _ => Ok(NextPage::Done),
    }
}

fn next_url_next(body: &Value, response_path: &str) -> Result<NextPage> {
    use jsonpath_rust::JsonPath;

    let path = JsonPath::try_from(response_path).map_err(|e| {
        Error::Recipe(format!(
            "invalid `response_path` JSONPath `{response_path}`: {e}"
        ))
    })?;
    let next_url = match path.find(body) {
        Value::Array(arr) => arr.into_iter().find_map(|v| v.as_str().map(String::from)),
        _ => None,
    };
    match next_url {
        Some(u) if !u.is_empty() => Ok(NextPage::Url(u)),
        _ => Ok(NextPage::Done),
    }
}

fn page_number_next(
    current_url: &str,
    end: usize,
    param: &str,
    state: &mut PaginationState,
) -> Result<NextPage> {
    if state.current_page == 0 {
        // First call — orchestrator sent the start page; we now
        // schedule the next.
        state.current_page = parse_query_param_usize(current_url, param).unwrap_or(1);
    }
    state.current_page += 1;
    if state.current_page > end {
        return Ok(NextPage::Done);
    }
    Ok(NextPage::Url(set_query_param(
        current_url,
        param,
        &state.current_page.to_string(),
    )))
}

// ---------------------------------------------------------------------------
// Tiny URL query helpers — kept minimal so we don't pull in the
// `url` crate just for two operations.
// ---------------------------------------------------------------------------

fn parse_query(url: &str) -> (String, BTreeMap<String, String>, Option<String>) {
    let (base, query, fragment) = match url.find('?') {
        None => (url.to_string(), BTreeMap::new(), None),
        Some(q_idx) => {
            let (base, rest) = url.split_at(q_idx);
            let rest = &rest[1..]; // drop '?'
            let (query_str, fragment) = match rest.find('#') {
                None => (rest, None),
                Some(f_idx) => (&rest[..f_idx], Some(rest[f_idx..].to_string())),
            };
            let mut params = BTreeMap::new();
            for kv in query_str.split('&') {
                if kv.is_empty() {
                    continue;
                }
                match kv.split_once('=') {
                    Some((k, v)) => {
                        params.insert(k.to_string(), v.to_string());
                    }
                    None => {
                        params.insert(kv.to_string(), String::new());
                    }
                }
            }
            (base.to_string(), params, fragment)
        }
    };
    (base, query, fragment)
}

fn assemble(base: &str, params: &BTreeMap<String, String>, fragment: Option<&str>) -> String {
    let mut out = base.to_string();
    if !params.is_empty() {
        out.push('?');
        let mut first = true;
        for (k, v) in params {
            if !first {
                out.push('&');
            }
            first = false;
            out.push_str(k);
            out.push('=');
            out.push_str(v);
        }
    }
    if let Some(f) = fragment {
        out.push_str(f);
    }
    out
}

fn parse_query_param_usize(url: &str, name: &str) -> Option<usize> {
    let (_, params, _) = parse_query(url);
    params.get(name).and_then(|v| v.parse::<usize>().ok())
}

fn set_query_param(url: &str, name: &str, value: &str) -> String {
    let (base, mut params, fragment) = parse_query(url);
    params.insert(name.to_string(), value.to_string());
    assemble(&base, &params, fragment.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn offset_strategy_increments_until_short_page() {
        let strat = PaginationStrategy::Offset {
            param: "offset".into(),
            page_size: 2,
            items_path: "$.items".into(),
        };
        let mut state = PaginationState::default();

        // First page: 2 items → keep going.
        let body = json!({"items": [1, 2]});
        let url = "https://api.example.com/q?offset=0";
        let next = next_page(&strat, url, &body, &mut state).unwrap();
        match next {
            NextPage::Url(u) => assert!(u.contains("offset=2"), "got {u}"),
            NextPage::Done => panic!("expected continuation"),
        }

        // Second page: 1 item (< page_size) → done.
        let body2 = json!({"items": [3]});
        let next2 = next_page(
            &strat,
            "https://api.example.com/q?offset=2",
            &body2,
            &mut state,
        )
        .unwrap();
        assert!(matches!(next2, NextPage::Done));
    }

    #[test]
    fn cursor_strategy_passes_response_value_to_next_request() {
        let strat = PaginationStrategy::Cursor {
            param: "after".into(),
            response_path: "$.next_cursor".into(),
        };
        let body = json!({"next_cursor": "abc123"});
        let mut state = PaginationState::default();
        let next = next_page(
            &strat,
            "https://api.example.com/items?limit=10",
            &body,
            &mut state,
        )
        .unwrap();
        match next {
            NextPage::Url(u) => assert!(u.contains("after=abc123"), "got {u}"),
            NextPage::Done => panic!("expected continuation"),
        }

        // Empty / missing cursor stops pagination.
        let body_done = json!({"next_cursor": null});
        let next_done = next_page(
            &strat,
            "https://api.example.com/items?after=abc123",
            &body_done,
            &mut state,
        )
        .unwrap();
        assert!(matches!(next_done, NextPage::Done));
    }

    #[test]
    fn next_url_strategy_returns_url_verbatim() {
        let strat = PaginationStrategy::NextUrl {
            response_path: "$.next".into(),
        };
        let body = json!({"next": "https://api.example.com/page2"});
        let mut state = PaginationState::default();
        let next = next_page(&strat, "https://api.example.com/page1", &body, &mut state).unwrap();
        match next {
            NextPage::Url(u) => assert_eq!(u, "https://api.example.com/page2"),
            NextPage::Done => panic!("expected continuation"),
        }
    }

    #[test]
    fn page_number_strategy_walks_until_end() {
        let strat = PaginationStrategy::PageNumber {
            param: "page".into(),
            start: 1,
            end: 3,
        };
        let body = json!({});
        let mut state = PaginationState::default();
        let next = next_page(
            &strat,
            "https://api.example.com/q?page=1",
            &body,
            &mut state,
        )
        .unwrap();
        match next {
            NextPage::Url(u) => assert!(u.contains("page=2")),
            NextPage::Done => panic!("expected continuation"),
        }
        let next2 = next_page(
            &strat,
            "https://api.example.com/q?page=2",
            &body,
            &mut state,
        )
        .unwrap();
        match next2 {
            NextPage::Url(u) => assert!(u.contains("page=3")),
            NextPage::Done => panic!("expected continuation"),
        }
        let next3 = next_page(
            &strat,
            "https://api.example.com/q?page=3",
            &body,
            &mut state,
        )
        .unwrap();
        assert!(matches!(next3, NextPage::Done));
    }

    #[test]
    fn set_query_param_replaces_existing() {
        let url = "https://api.example.com/q?a=1&b=2&c=3";
        let updated = set_query_param(url, "b", "99");
        assert!(updated.contains("b=99"));
        assert!(updated.contains("a=1"));
        assert!(updated.contains("c=3"));
        assert!(!updated.contains("b=2"));
    }

    #[test]
    fn set_query_param_adds_when_missing() {
        let url = "https://api.example.com/q";
        let updated = set_query_param(url, "page", "2");
        assert_eq!(updated, "https://api.example.com/q?page=2");
    }
}
