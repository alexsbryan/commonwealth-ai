// SPDX-License-Identifier: AGPL-3.0-or-later
//! The window reader's answer, parsed so a malformed value costs at most the
//! particular or claim holding it, never the window (`window.rs`). Two steps,
//! each counted and traced, neither guessing at what the model meant:
//!
//! - a number written with no digit before its point (`.02`) is read as the
//!   number it writes (`0.02`): JSON requires the leading zero, the value is
//!   not in doubt, and text inside a string is never touched;
//! - if the answer still does not parse, each element of its `particulars` and
//!   `claims` arrays is parsed on its own: one that does not parse is dropped
//!   and counted, and so is every claim about a dropped particular;
//! - a key a claim writes outside its envelope (the keys `window_prompt.md`
//!   gives a claim) is a field written beside `fields`, and is read in
//!   `fields` when `fields` does not hold it: a claim has no other place a
//!   field can mean anything, so its value is not in doubt.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use serde_json::{json, Map, Value};
use tracing::{debug, warn};

/// A parsed window answer and what parsing it cost.
pub(super) struct Parsed {
    pub(super) value: Value,
    /// Bare fractions read with their leading zero.
    pub(super) repaired: usize,
    /// Elements not read: particulars and claims that did not parse, and the
    /// claims about a particular that did not.
    pub(super) dropped: usize,
    /// Claim fields written beside `fields`, read in it.
    pub(super) beside: usize,
}

/// `json`, the answer's JSON block (`prepare_phase_json`), parsed. An error
/// only when the whole answer fails and neither array can be found in it.
pub(super) fn parse(json: &str, chapter: &str) -> Result<Parsed, String> {
    let (json, repaired) = bare_fractions(json);
    if repaired > 0 {
        warn!(chapter, repaired, "document_read/window: numbers written without a leading zero read as the numbers they write");
    }
    let mut parsed = match serde_json::from_str(&json) {
        Ok(value) => Parsed {
            value,
            repaired,
            dropped: 0,
            beside: 0,
        },
        Err(whole) => by_element(&json, chapter, repaired).ok_or_else(|| whole.to_string())?,
    };
    parsed.beside = fields_beside(&mut parsed.value, chapter);
    Ok(parsed)
}

/// The keys of a claim's envelope, read from the prompt's answer shape (its
/// `"claims": [{...}]` element), so the prompt stays their one statement.
pub(super) static CLAIM_KEYS: LazyLock<BTreeSet<String>> =
    LazyLock::new(|| element_keys(super::PROMPT, "\"claims\": [{"));

/// The quoted keys of the object that opens with the `{` ending `opens` in
/// `shape`, at its own depth (a nested `{<field>: <value>}` names none).
fn element_keys(shape: &str, opens: &str) -> BTreeSet<String> {
    let Some(at) = shape.find(opens) else {
        return BTreeSet::new();
    };
    let body = &shape[at + opens.len()..];
    let (mut keys, mut depth, mut quote) = (BTreeSet::new(), 1usize, None::<usize>);
    for (i, c) in body.char_indices() {
        match (c, quote) {
            ('"', None) => quote = Some(i + 1),
            ('"', Some(start)) => {
                let after = body[i + 1..].trim_start();
                if depth == 1 && after.starts_with(':') {
                    keys.insert(body[start..i].to_string());
                }
                quote = None;
            }
            (_, Some(_)) => {}
            ('{', None) => depth += 1,
            ('}', None) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    keys
}

/// Every claim's keys outside its envelope moved into its `fields` when
/// `fields` does not hold them; how many were moved. A key `fields` already
/// holds is left where it was, unread, and traced.
fn fields_beside(answer: &mut Value, chapter: &str) -> usize {
    let mut moved = 0usize;
    for claim in answer["claims"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
    {
        let beside: Vec<String> = claim
            .keys()
            .filter(|k| !CLAIM_KEYS.contains(*k))
            .cloned()
            .collect();
        if beside.is_empty() {
            continue;
        }
        if claim.get("fields").is_none_or(Value::is_null) {
            claim.insert("fields".into(), Value::Object(Map::new()));
        }
        let Some(Value::Object(fields)) = claim.get("fields") else {
            debug!(chapter, keys = ?beside, "document_read/window: claim keys beside a `fields` that is not an object; not read");
            continue;
        };
        let (held, free): (Vec<String>, Vec<String>) =
            beside.into_iter().partition(|k| fields.contains_key(k));
        if !held.is_empty() {
            debug!(chapter, keys = ?held, "document_read/window: claim keys beside `fields`, which already holds them; not read");
        }
        let lifted: Vec<(String, Value)> = free
            .into_iter()
            .filter_map(|k| claim.remove(&k).map(|v| (k, v)))
            .collect();
        moved += lifted.len();
        if let Some(Value::Object(fields)) = claim.get_mut("fields") {
            fields.extend(lifted);
        }
    }
    if moved > 0 {
        warn!(
            chapter,
            moved, "document_read/window: claim fields written beside `fields` read in it"
        );
    }
    moved
}

/// `json` with a `0` written before every point that opens a number outside a
/// string (after `:`, `,`, `[` or a sign), and how many were written.
fn bare_fractions(json: &str) -> (String, usize) {
    let mut out = String::with_capacity(json.len());
    let (mut in_string, mut escaped, mut repaired) = (false, false, 0usize);
    let mut before = ' ';
    let mut chars = json.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else if c == '.'
            && matches!(before, ':' | ',' | '[' | '-')
            && chars.peek().is_some_and(|(_, n)| n.is_ascii_digit())
        {
            debug!(
                at,
                "document_read/window: a bare fraction read with its leading zero"
            );
            out.push('0');
            repaired += 1;
        }
        if !c.is_whitespace() {
            before = c;
        }
        out.push(c);
    }
    (out, repaired)
}

/// The answer read element by element; `None` when it holds neither array.
fn by_element(json: &str, chapter: &str, repaired: usize) -> Option<Parsed> {
    let (particulars, claims) = (elements(json, "particulars"), elements(json, "claims"));
    if particulars.is_none() && claims.is_none() {
        return None;
    }
    for (key, found) in [("particulars", &particulars), ("claims", &claims)] {
        if found.is_none() {
            warn!(
                chapter,
                key, "document_read/window: the unparsed answer holds no such array; read as none"
            );
        }
    }
    let mut dropped = 0usize;
    let mut gone = BTreeSet::new();
    let mut read = |text: &str, what: &str| match serde_json::from_str::<Value>(text) {
        Ok(v) => Some(v),
        Err(why) => {
            dropped += 1;
            let head: String = text.chars().take(160).collect();
            warn!(chapter, what, %why, element = %head, "document_read/window: an element of the answer is not JSON; dropped");
            if what == "particular" {
                gone.extend(ref_of(text));
            }
            None
        }
    };
    let particulars: Vec<Value> = particulars
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| read(t, "particular"))
        .collect();
    let claims: Vec<Value> = claims
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| read(t, "claim"))
        .collect();
    let claims: Vec<Value> = claims
        .into_iter()
        .filter(|c| {
            let about = c["about"].as_str().unwrap_or_default();
            let keep = !gone.contains(about);
            if !keep {
                dropped += 1;
                warn!(
                    chapter,
                    about, "document_read/window: a claim about a dropped particular; dropped"
                );
            }
            keep
        })
        .collect();
    warn!(
        chapter,
        particulars = particulars.len(),
        claims = claims.len(),
        dropped,
        "document_read/window: the answer read element by element"
    );
    Some(Parsed {
        value: json!({ "particulars": particulars, "claims": claims }),
        repaired,
        dropped,
        beside: 0,
    })
}

/// The text of each element of the array under the answer's top-level `key`,
/// split at the array's own commas (outside strings and nested values).
/// `None` when the answer holds no such array.
fn elements<'a>(json: &'a str, key: &str) -> Option<Vec<&'a str>> {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    let (mut opened, mut last_key, mut start) = (0usize, None::<&str>, None::<usize>);
    let mut out = Vec::new();
    let mut push = |text: &'a str| {
        if !text.trim().is_empty() {
            out.push(text.trim());
        }
    };
    for (i, c) in json.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
                if depth == 1 && start.is_none() {
                    last_key = Some(&json[opened..i]);
                }
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                opened = i + 1;
            }
            '{' | '[' => {
                depth += 1;
                if c == '[' && depth == 2 && start.is_none() && last_key == Some(key) {
                    start = Some(i + 1);
                }
            }
            '}' | ']' => {
                if let (Some(s), 2) = (start, depth) {
                    push(&json[s..i]);
                    return Some(out);
                }
                depth = depth.saturating_sub(1);
            }
            ',' => {
                if let (Some(s), 2) = (start, depth) {
                    push(&json[s..i]);
                    start = Some(i + 1);
                } else if depth == 1 {
                    last_key = None;
                }
            }
            _ => {}
        }
    }
    let s = start?;
    push(&json[s..]);
    Some(out)
}

/// The `ref` an element that did not parse names, read from its text.
fn ref_of(text: &str) -> Option<String> {
    let rest = &text[text.find("\"ref\"")? + 5..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}
