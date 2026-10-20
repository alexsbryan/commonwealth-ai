// SPDX-License-Identifier: AGPL-3.0-or-later
//! The window's second look (model-track-prereg.md, plan step 2): the same
//! window asked once more, shown what its first answer wrote, for what it did
//! not. The local reader's misses vary by read: one read of world-s4 writes
//! about 17 of windows 2-3's 27 gold updates, eight together write 24
//! (runs/loop, 2026-10-20), so a second look is a second draw at them.
//!
//! Code folds the second answer into the first, deciding no identity: a
//! particular named again under its first ref, type and name is the first's;
//! a ref the second answer gives to anything else is renamed apart wherever
//! that answer uses it; a claim the first wrote (document, kind, particular,
//! quote) is dropped. A second look that is not read leaves the first answer,
//! traced, never the window lost.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use tracing::{info, warn};

use super::answer::{self, Parsed};
use crate::enrichment::pipeline::pipelines::literary::prepare_phase_json;
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

pub(super) const PROMPT: &str = include_str!("window_rest_prompt.md");

/// The second look's prompt: the first's, then what the first answer wrote.
pub(super) fn ask(first: &ChatPrompt, written: &Value) -> ChatPrompt {
    let written = json!({"particulars": written["particulars"], "claims": written["claims"]});
    let mut prompt = first.clone();
    prompt.user = format!(
        "{}\n\n{}",
        first.user,
        PROMPT.replace("{written}", &written.to_string())
    );
    prompt
}

/// Ask the second look and fold its answer into `first`.
pub(super) async fn look(
    chapter: &str,
    prompt: &ChatPrompt,
    first: &mut Parsed,
    infer: &InferenceFn,
) {
    let second = ask(prompt, &first.value);
    let answered = match infer(&second, None).await {
        Ok(raw) => {
            tracing::trace!(chapter, answer = %raw, "document_read/window: second answer");
            prepare_phase_json(&raw, "phase 1 (window second look)")
                .map_err(|e| e.to_string())
                .and_then(|json| answer::parse(&json, chapter))
        }
        Err(e) => Err(e.to_string()),
    };
    match answered {
        Ok(more) => {
            let (particulars, claims, repeated, renamed) = fold(&mut first.value, more.value);
            first.repaired += more.repaired;
            first.dropped += more.dropped;
            first.beside += more.beside;
            first.in_grade += more.in_grade;
            info!(
                chapter,
                particulars,
                claims,
                repeated,
                renamed,
                dropped = more.dropped,
                "document_read/window: second look folded"
            );
        }
        Err(error) => {
            warn!(chapter, %error, "document_read/window: the second look was not read; the window stands on its first answer")
        }
    }
}

/// Fold `second` into `first`; returns the particulars and claims added, the
/// claims dropped as the first's, and the refs renamed apart.
pub(super) fn fold(first: &mut Value, mut second: Value) -> (usize, usize, usize, usize) {
    let firsts: Vec<Value> = first["particulars"].as_array().cloned().unwrap_or_default();
    let mut refs: BTreeSet<String> = firsts
        .iter()
        .filter_map(|p| p["ref"].as_str())
        .map(String::from)
        .collect();
    let same = |a: &Value, b: &Value| {
        a["type"] == b["type"] && text(&a["name"]).to_lowercase() == text(&b["name"]).to_lowercase()
    };
    let mut renamed = 0;
    let mut kept = Vec::new();
    for p in second["particulars"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let r = text(&p["ref"]);
        match firsts.iter().find(|f| text(&f["ref"]) == r) {
            Some(f) if same(f, &p) => {}
            Some(_) => {
                let fresh = (2..)
                    .map(|n| format!("{r}~{n}"))
                    .find(|f| !refs.contains(f))
                    .unwrap();
                rename(&mut second, &r, &fresh);
                refs.insert(fresh.clone());
                renamed += 1;
                kept.push(fresh);
            }
            None => {
                refs.insert(r.clone());
                kept.push(r);
            }
        }
    }
    let new_particulars: Vec<Value> = second["particulars"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| kept.contains(&text(&p["ref"])))
        .cloned()
        .collect();
    let key = |c: &Value| {
        let quote = text(&c["quote"])
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "{}|{}|{}|{}",
            text(&c["document"]),
            text(&c["kind"]),
            text(&c["about"]),
            quote.to_lowercase()
        )
    };
    let mut written: BTreeSet<String> = first["claims"]
        .as_array()
        .into_iter()
        .flatten()
        .map(key)
        .collect();
    let (mut new_claims, mut repeated) = (Vec::new(), 0);
    for c in second["claims"].as_array().into_iter().flatten() {
        if written.insert(key(c)) {
            new_claims.push(c.clone());
        } else {
            repeated += 1;
        }
    }
    let (particulars, claims) = (new_particulars.len(), new_claims.len());
    extend(first, "particulars", new_particulars);
    extend(first, "claims", new_claims);
    (particulars, claims, repeated, renamed)
}

fn text(v: &Value) -> String {
    v.as_str().map(str::trim).unwrap_or_default().to_string()
}

/// Every string of the answer that is `from` exactly: a ref is written as
/// `about`, as a particular's `ref`, and as a reference field's value.
fn rename(v: &mut Value, from: &str, to: &str) {
    match v {
        Value::String(s) if s.trim() == from => *s = to.to_string(),
        Value::Array(items) => items.iter_mut().for_each(|x| rename(x, from, to)),
        Value::Object(map) => map.values_mut().for_each(|x| rename(x, from, to)),
        _ => {}
    }
}

fn extend(answer: &mut Value, key: &str, more: Vec<Value>) {
    if more.is_empty() {
        return;
    }
    match answer.get_mut(key).and_then(Value::as_array_mut) {
        Some(items) => items.extend(more),
        None => answer[key] = Value::Array(more),
    }
}

#[cfg(test)]
#[path = "window_rest_tests.rs"]
mod tests;
