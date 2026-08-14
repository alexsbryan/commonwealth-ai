// SPDX-License-Identifier: AGPL-3.0-or-later
//! Custody classification — a chunk's provenance class.
//!
//! Closed set → an enum, never a stringly registry (ARCH §2). The wire
//! spellings are a stable contract: the custody reds
//! (`sovereign-core/tests/custody_reds.rs`) pin exactly
//! `public-web | personal | peer` on released records, and the released
//! surfaces in `research/deep-research/notes/custody.md` §5 key on them.
//!
//! `Unknown` is the THIRD VARIANT, never a default: a chunk whose
//! provenance cannot be determined is not quietly `personal` or
//! `public-web` — it is `unknown`, and a factual claim resting on it
//! must refuse (custody.md §4, red R-3). `Unknown` never rides a
//! released record (`is_released_class` is false) — the refusal happens
//! before release, and the gate's per-chunk ledger records
//! `provenance_class: "unknown"` instead.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The metadata key a chunk's custody stamp rides under — the ONE key
/// the acquisition stamp sites write (`retrieval_pipeline`'s
/// `step_store_search`, the web-fetch leg) and the gate's evidence
/// builder reads (`grounding::gate_evidence_with_sources`), so a stamp
/// typo cannot silently diverge into a second key (ARCH §10.6 — one
/// implementation per key). The released `retrieved_chunks[].custody`
/// surface reads the same key.
pub const CUSTODY_META_KEY: &str = "custody";

/// A chunk's custody class (custody.md §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Custody {
    /// Fetched from the open web (a URL fetch, a crawl, an RSS item, a
    /// published page).
    PublicWeb,
    /// The estate's own material — local files, notes, conversations,
    /// imported personal data.
    Personal,
    /// Material that arrived from another node on the mesh (peer
    /// corpora, shared indexes, gossip-carried content).
    Peer,
    /// Provenance cannot be determined (no stamp, no derivable join).
    /// Refuses — never a default.
    Unknown,
}

impl Custody {
    /// The three released classes. `Unknown` is excluded: a claim resting
    /// on unknown provenance refuses before anything reaches a release
    /// surface (custody.md §4).
    pub const RELEASED_CLASSES: [Custody; 3] =
        [Custody::PublicWeb, Custody::Personal, Custody::Peer];

    /// The exact wire spelling — the one implementation of the contract
    /// strings (the custody reds pin them).
    pub fn as_str(self) -> &'static str {
        match self {
            Custody::PublicWeb => "public-web",
            Custody::Personal => "personal",
            Custody::Peer => "peer",
            Custody::Unknown => "unknown",
        }
    }

    /// True for the three stamped classes; `Unknown` never rides a
    /// released record.
    pub fn is_released_class(self) -> bool {
        !matches!(self, Custody::Unknown)
    }

    /// Parse a wire spelling (lenient: accepts the exact contract strings
    /// only — a typo is an error, not a silent `Unknown`).
    pub fn parse_wire(s: &str) -> Option<Custody> {
        match s {
            "public-web" => Some(Custody::PublicWeb),
            "personal" => Some(Custody::Personal),
            "peer" => Some(Custody::Peer),
            "unknown" => Some(Custody::Unknown),
            _ => None,
        }
    }
}

impl fmt::Display for Custody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Custody {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Custody::parse_wire(s).ok_or_else(|| format!("unknown custody wire spelling: {s:?}"))
    }
}

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

/// Max-restrictiveness join over derivation inputs (custody.md §3),
/// computed at creation: `personal > peer > public-web`; any `unknown`
/// input poisons the join to `unknown` (a derived artifact whose inputs
/// include unknown provenance is itself unknown — and refuses).
pub fn join_custody(inputs: &[Custody]) -> Custody {
    let mut joined = Custody::PublicWeb;
    for c in inputs {
        match c {
            Custody::Unknown => return Custody::Unknown,
            Custody::Personal => joined = Custody::Personal,
            Custody::Peer => {
                if joined != Custody::Personal {
                    joined = Custody::Peer;
                }
            }
            Custody::PublicWeb => {}
        }
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_spellings_are_the_contract() {
        assert_eq!(Custody::PublicWeb.as_str(), "public-web");
        assert_eq!(Custody::Personal.as_str(), "personal");
        assert_eq!(Custody::Peer.as_str(), "peer");
        assert_eq!(Custody::Unknown.as_str(), "unknown");
        for c in Custody::RELEASED_CLASSES {
            assert!(c.is_released_class());
        }
        assert!(!Custody::Unknown.is_released_class());
    }

    #[test]
    fn parse_wire_is_exact() {
        assert_eq!(Custody::parse_wire("public-web"), Some(Custody::PublicWeb));
        assert_eq!(Custody::parse_wire("public_web"), None);
        assert_eq!(Custody::parse_wire(""), None);
        assert_eq!("peer".parse::<Custody>().unwrap(), Custody::Peer);
        assert!("PEER".parse::<Custody>().is_err());
    }

    #[test]
    fn join_is_max_restrictiveness() {
        assert_eq!(
            join_custody(&[Custody::PublicWeb, Custody::PublicWeb]),
            Custody::PublicWeb
        );
        assert_eq!(
            join_custody(&[Custody::Peer, Custody::PublicWeb]),
            Custody::Peer
        );
        assert_eq!(
            join_custody(&[Custody::Personal, Custody::PublicWeb]),
            Custody::Personal
        );
        // personal > peer — order-independent
        assert_eq!(
            join_custody(&[Custody::Peer, Custody::Personal, Custody::PublicWeb]),
            Custody::Personal
        );
        // unknown poisons the join
        assert_eq!(
            join_custody(&[Custody::PublicWeb, Custody::Unknown]),
            Custody::Unknown
        );
    }
}
