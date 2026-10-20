// SPDX-License-Identifier: AGPL-3.0-or-later
//! How RESOLVE decided a claim's subject, as the claim keeps it
//! (`RESOLVE_LINK_ATTRIBUTE`). Moved whole from `resolution_records.rs`.

use std::collections::HashMap;

use serde_json::Value;

use super::super::atoms::AtomId;
use super::super::resolve_records::Choice;

/// How a statement's subject was decided: by key, by the model's choice, or
/// opened; and, when a forced choice was asked, the model's choice scores
/// over what it was shown: the decided record's (none's, for a record the
/// statement opened), the best other candidate's, and none's. They are the
/// model's uncalibrated distribution over the candidates of that one
/// question, kept for inspection; no code reads them as the chance a link is
/// right.
pub(super) fn resolve_link(
    how: &str,
    choice: Option<&Choice>,
    record: &str,
    atom_of: &HashMap<&str, AtomId>,
) -> Value {
    let mut link = serde_json::json!({ "decided_by": how });
    let Some(c) = choice else {
        return link;
    };
    let chosen = c
        .candidates
        .iter()
        .find(|(id, _)| id == record)
        .map_or(c.none, |(_, p)| *p);
    link["choice_score"] = serde_json::json!(chosen);
    link["none_score"] = serde_json::json!(c.none);
    if let Some((id, p)) = c
        .candidates
        .iter()
        .filter(|(id, _)| id != record)
        .max_by(|a, b| a.1.total_cmp(&b.1))
    {
        link["runner_up"] = serde_json::json!({
            "record": atom_of.get(id.as_str()).map(|a| a.as_str()),
            "score": p,
        });
    }
    link
}
