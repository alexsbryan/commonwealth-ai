// SPDX-License-Identifier: AGPL-3.0-or-later
//! The grounding gate's custody surface — the per-chunk ledger row and the
//! legacy metadata key.
//!
//! The [`Custody`] CLASS itself moved to `kernel-types` on 2026-08-20
//! (noun-convergence rung nc-1-kernel): custody is provenance, provenance is
//! what layer 0 is for, and while it lived here it sat in a product domain's
//! types crate — so `corpus-engine` naming it, at the acquisition point where
//! the stamp actually belongs, was a backflow edge by the workspace's own
//! layer map. It is re-exported below, so every existing
//! `sovereign_contracts::types::Custody` import is unchanged.
//!
//! What stayed, and why: these two are the GATE's business rather than the
//! kernel's. `ChunkCustody` is the shape of one row in the judge's evidence
//! ledger. `CUSTODY_META_KEY` is the untyped `HashMap<String, String>`
//! channel the campaign is closing — putting it at layer 0 would bless the
//! very thing being removed.

use serde::{Deserialize, Serialize};

pub use kernel_types::custody::{join_custody, Custody};

/// The metadata key a chunk's custody stamp rides under — the ONE key
/// the acquisition stamp sites write (`retrieval_pipeline`'s
/// `step_store_search`, the web-fetch leg) and the gate's evidence
/// builder reads (`grounding::gate_evidence_with_sources`), so a stamp
/// typo cannot silently diverge into a second key (ARCH §10.6 — one
/// implementation per key). The released `retrieved_chunks[].custody`
/// surface reads the same key.
///
/// This is the LEGACY channel. The target shape is a typed field on the
/// retrieval unit; until that lands, every read of this key must go through
/// `Custody::parse_wire` so a typo is an error rather than a silent default.
pub const CUSTODY_META_KEY: &str = "custody";

/// The per-chunk custody ledger the gate's judge saw (custody.md §5):
/// `{locator, custody, provenance_class, source_url}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkCustody {
    /// The chunk's locator (id / evidence id) — the judge-side handle.
    pub locator: String,
    /// The custody class; `Unknown` spells `provenance_class: "unknown"`
    /// and refuses.
    pub custody: Custody,
    /// `known | unknown` — distinguishes the three stamped classes from
    /// the refusal trigger.
    #[serde(default = "default_provenance_class")]
    pub provenance_class: String,
    /// The chunk's source URL (non-null for every fetched chunk).
    #[serde(default)]
    pub source_url: Option<String>,
}

fn default_provenance_class() -> String {
    "known".to_string()
}

impl ChunkCustody {
    /// Build a ledger row with the provenance class derived from the
    /// custody value — `unknown` ⇒ `"unknown"`, else `"known"`. One
    /// derivation, computed here (never by a model).
    pub fn new(locator: impl Into<String>, custody: Custody, source_url: Option<String>) -> Self {
        let provenance_class = if custody == Custody::Unknown {
            "unknown".to_string()
        } else {
            "known".to_string()
        };
        ChunkCustody {
            locator: locator.into(),
            custody,
            provenance_class,
            source_url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relocated_class_is_the_same_one_callers_had() {
        // The move must be a relocation, not a fork: if a second Custody
        // ever appears, this stops compiling rather than silently letting
        // two enums with the same wire spellings coexist (ARCH §10.6).
        let c: Custody = kernel_types::Custody::Personal;
        assert_eq!(c.as_str(), "personal");
        assert_eq!(Custody::parse_wire("public-web"), Some(Custody::PublicWeb));
    }

    #[test]
    fn an_unknown_stamp_marks_the_row_unknown_and_refuses() {
        let row = ChunkCustody::new("ev-1", Custody::Unknown, None);
        assert_eq!(row.provenance_class, "unknown");
        assert!(!row.custody.is_released_class());
    }

    #[test]
    fn a_stamped_row_is_known() {
        let row = ChunkCustody::new("ev-2", Custody::Peer, Some("https://x".into()));
        assert_eq!(row.provenance_class, "known");
        assert!(row.custody.is_released_class());
    }
}
