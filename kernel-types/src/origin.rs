// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Origin`] — where a piece of knowledge came from.
//!
//! The product claim is *an assistant that runs on your machine and proves
//! what it says*. As a type invariant that reads: nothing reaches the user
//! that did not come from evidence, and no evidence exists without an origin
//! and a custody. This is the origin half.
//!
//! # Three questions, three fields, one struct
//!
//! An origin answers three separate questions, and keeping them separate is
//! what lets the [`Source`] sum stay closed and small:
//!
//! | Field | Question | Was, before this type |
//! |---|---|---|
//! | [`source`](Origin::source) | which store or channel | a nine-way `if/else` on `metadata` in the prompt formatter |
//! | [`served_by`](Origin::served_by) | which machine | `metadata["peer"]`, absent when local |
//! | [`grain`](Origin::grain) | may it ground verbatim | `metadata["source"] == "raptor"` |
//!
//! That split does real work. A chunk that came from a corpus on a peer
//! machine is `Source::Corpus` + `Server::Peer`, not a fifth source variant.
//! A RAPTOR summary of corpus text is `Source::Corpus` + `Grain::Summary`, not
//! a sixth. Both of those were separate string conventions before, and both
//! collapse into the two orthogonal fields rather than widening the sum.
//!
//! # Every field is required
//!
//! There is no `Default`, no `Option` on any field, and no builder that can
//! leave one unset. The reason is the failure this replaces: provenance rode
//! a `HashMap<String, String>`, so "no stamp" and "stamped as local" were the
//! same value, and a missing stamp read as agreement rather than as absence.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{ContentHash, CorpusId, NodeId};

/// Where a piece of knowledge came from. Constructed at acquisition, carried
/// with the content, never reconstructed downstream by re-reading a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// Which store or channel the content was acquired through.
    pub source: Source,
    /// Which machine served it.
    pub served_by: Server,
    /// Whether it may ground a claim verbatim, or is derived from something
    /// that can.
    pub grain: Grain,
}

/// The store or channel a piece of content was acquired through — a CLOSED
/// sum.
///
/// # Closed means closed
///
/// There is no `Other(String)` variant and there must never be one. An escape
/// hatch re-opens exactly the untyped channel this type exists to close: with
/// `Other`, "we did not classify this" becomes representable, and the compiler
/// stops being able to tell a caller that a new acquisition path needs a
/// decision. Adding a path is one variant and one match arm — a reviewable
/// change — which is the point (ARCH §2, principle 9).
///
/// # Why seven and not the four this rung was drafted with
///
/// The rung specified `Corpus | Web | Attachment | ToolOutput` and instructed
/// that the sum be WIDENED rather than escaped if a shipping path was not
/// covered. A census of every path by which content reaches the model today
/// found three that no variant could hold, each cited below. Two other
/// apparent gaps — peer-served chunks and RAPTOR/atlas summaries — turned out
/// to be covered by [`Server`] and [`Grain`] and did NOT widen the sum.
///
/// # What is deliberately not here
///
/// System prompts, persona text, compiled lessons, the tool dossier and the
/// epistemic contract all reach the model and none of them is a `Source`.
/// They are the assistant's own instructions, not knowledge with provenance,
/// and nothing may ground a factual claim on them. Where such text is today
/// injected INTO the evidence pool as if it were a chunk — the corpus-
/// readiness disclosure is the live example — that is a defect to fix at the
/// injection site, not a variant to mint here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// An installed corpus index. The ordinary retrieval path, and also the
    /// estate document store and the focused-passage handoff.
    Corpus {
        corpus: CorpusId,
        /// The document's identity, content-addressed. There is no
        /// `DocumentId`: a document IS its bytes (ARCH §7.5), and an edited
        /// document is honestly a different one to cite.
        document: ContentHash,
        locator: Locator,
    },
    /// Fetched from the open web.
    Web {
        /// The URL fetched. A `String`, not a newtype: the URL space is an
        /// open set and a newtype over it would buy no invariant the kernel
        /// can enforce without taking a parser dependency at layer 0.
        url: String,
        /// Unix epoch SECONDS at fetch time. A primitive rather than a
        /// timestamp type because the kernel cannot depend on
        /// `sovereign-time` — that crate sits in a product domain, so the
        /// edge would be the exact backflow this layer forbids. Matches
        /// `sovereign_time::unix_now()`'s units and width.
        fetched_at: i64,
    },
    /// Content the user supplied for this session — an uploaded file, or text
    /// pasted into the turn. Content-addressed rather than named, so the same
    /// bytes supplied twice are one asset.
    Attachment {
        asset: ContentHash,
        locator: Locator,
    },
    /// A tool computed it. The tool's registry key is a `String` because the
    /// registry is an open set — a tool exists iff `ToolRegistry` lists it,
    /// and a closed enum here would have to be edited to add a tool
    /// (principle 9). `call_hash` identifies the specific invocation.
    ToolOutput {
        tool: String,
        call_hash: ContentHash,
    },
    /// The estate's long-term memory pool. WIDENS the drafted sum: memory
    /// recall reaches the model on every turn that has any, and it is
    /// modelled today by a separate `Provenance::Memory` enum precisely
    /// because no origin type could hold it.
    Memory { entry: ContentHash },
    /// An earlier turn of a conversation — this one or a retrieved other.
    /// WIDENS the drafted sum: conversation history reaches the model on
    /// essentially every turn, and some of it is sealed into the grounding
    /// gate's evidence universe, where it currently arrives with no custody
    /// row at all.
    Conversation {
        conversation: ContentHash,
        /// 0-based index of the turn within the conversation.
        turn: u32,
    },
    /// The note store. WIDENS the drafted sum: notes are already a distinct
    /// evidence channel with their own store and their own content hash, and
    /// folding them into `Corpus` would erase a distinction the retrieval
    /// path already makes.
    Note { note: ContentHash },
}

impl Source {
    /// A stable, lowercase discriminator for logs, metrics and wire rows.
    /// One implementation, so a trace and a stored row cannot disagree.
    pub fn kind(&self) -> &'static str {
        match self {
            Source::Corpus { .. } => "corpus",
            Source::Web { .. } => "web",
            Source::Attachment { .. } => "attachment",
            Source::ToolOutput { .. } => "tool_output",
            Source::Memory { .. } => "memory",
            Source::Conversation { .. } => "conversation",
            Source::Note { .. } => "note",
        }
    }
}

/// Which machine served this content.
///
/// Two variants and no third: a locally-served hit is `Local`, explicitly,
/// rather than an absent key. Before this type the fact rode
/// `metadata["peer"]`, so "served locally" and "we forgot to record it" were
/// the same observation.
///
/// The name is about provenance, not about HTTP — this is unrelated to the
/// `sovereign-server` crate and to `ServerConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum Server {
    /// This machine.
    Local,
    /// Another node on the mesh. Carrying the [`NodeId`] rather than only the
    /// display name is what makes attribution survive translation between the
    /// two projects.
    Peer {
        node: NodeId,
        /// The peer's display name. A `String`: it is a human label, not an
        /// identity, and `node` is the identity.
        name: String,
    },
}

impl Server {
    pub fn is_peer(&self) -> bool {
        matches!(self, Server::Peer { .. })
    }
}

/// Whether a piece of content may ground a claim verbatim.
///
/// Before this type the distinction was `metadata["source"] == "raptor"`,
/// compared inside the grounding gate. A summary is model-authored prose
/// ABOUT source text; quoting it as if it were the source is how a
/// hallucination acquires a citation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grain {
    /// The content itself, as acquired. May be quoted.
    Leaf,
    /// Derived from leaf content — a RAPTOR rollup, an atlas entry, an
    /// enrichment artifact. May orient retrieval; may not be quoted as
    /// source text.
    Summary,
}

impl Grain {
    /// True only for [`Grain::Leaf`]. Named for the question a caller is
    /// actually asking, so the check reads the same at every call site.
    pub fn may_be_quoted(self) -> bool {
        matches!(self, Grain::Leaf)
    }
}

/// Where inside its document a piece of content sits — the citation handle.
///
/// Opaque on purpose. The internal shape is domain-specific (a corpus chunk
/// id, an attachment page and offset, a note row) and the kernel cannot close
/// that set without knowing every store, so it carries the handle the issuing
/// domain minted rather than inventing a lowest common denominator. This
/// matches the `locator` field the custody ledger already carries.
///
/// Non-empty by construction: an empty locator is a citation that points at
/// nothing, which is worse than no citation because it renders as one.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Locator(String);

impl Locator {
    /// `None` on an empty or whitespace-only handle.
    pub fn new(handle: impl Into<String>) -> Option<Self> {
        let handle = handle.into();
        if handle.trim().is_empty() {
            return None;
        }
        Some(Locator(handle))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Locator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Locator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Locator({:?})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locator() -> Locator {
        Locator::new("chunk:42").unwrap()
    }

    fn corpus_origin() -> Origin {
        Origin {
            source: Source::Corpus {
                corpus: CorpusId::new("wikipedia").unwrap(),
                document: ContentHash::of_str("doc"),
                locator: locator(),
            },
            served_by: Server::Local,
            grain: Grain::Leaf,
        }
    }

    #[test]
    fn a_peer_served_corpus_chunk_does_not_need_a_source_variant() {
        // The mesh fan-out path. Before Origin this was
        // `metadata["peer"] = <name>` plus `metadata["source"] = "mesh"`,
        // and the two were set at different call sites.
        let o = Origin {
            served_by: Server::Peer {
                node: NodeId::from_u128(7),
                name: "halo".into(),
            },
            ..corpus_origin()
        };
        assert_eq!(o.source.kind(), "corpus");
        assert!(o.served_by.is_peer());
    }

    #[test]
    fn a_raptor_summary_does_not_need_a_source_variant() {
        // Before Grain this was `metadata["source"] == "raptor"`, compared
        // inside the grounding gate.
        let o = Origin {
            grain: Grain::Summary,
            ..corpus_origin()
        };
        assert_eq!(o.source.kind(), "corpus");
        assert!(!o.grain.may_be_quoted());
    }

    #[test]
    fn a_leaf_may_be_quoted_and_a_summary_may_not() {
        assert!(Grain::Leaf.may_be_quoted());
        assert!(!Grain::Summary.may_be_quoted());
    }

    #[test]
    fn locally_served_is_a_value_not_an_absent_key() {
        // The defect this replaces: `metadata["peer"]` absent meant both
        // "served locally" and "nobody recorded it".
        assert!(!Server::Local.is_peer());
        let j = serde_json::to_string(&Server::Local).unwrap();
        assert_eq!(j, r#"{"at":"local"}"#);
    }

    #[test]
    fn every_shipping_acquisition_path_has_a_variant() {
        // One row per path family found by the 2026-08-20 census of every
        // way content reaches the model. This test is the kill-clause check:
        // if a path exists with no variant, the sum is too narrow and must be
        // widened — never escaped with `Other(String)`.
        let paths: Vec<(&str, Source)> = vec![
            (
                "corpus index search",
                Source::Corpus {
                    corpus: CorpusId::new("c").unwrap(),
                    document: ContentHash::of_str("d"),
                    locator: locator(),
                },
            ),
            (
                "web fetch",
                Source::Web {
                    url: "https://example.org".into(),
                    fetched_at: 1_755_000_000,
                },
            ),
            (
                "uploaded document / pasted text",
                Source::Attachment {
                    asset: ContentHash::of_str("a"),
                    locator: locator(),
                },
            ),
            (
                "tool result transcript",
                Source::ToolOutput {
                    tool: "knowledge_lookup".into(),
                    call_hash: ContentHash::of_str("call"),
                },
            ),
            (
                "long-term memory recall",
                Source::Memory {
                    entry: ContentHash::of_str("m"),
                },
            ),
            (
                "earlier conversation turn",
                Source::Conversation {
                    conversation: ContentHash::of_str("conv"),
                    turn: 3,
                },
            ),
            (
                "note store",
                Source::Note {
                    note: ContentHash::of_str("n"),
                },
            ),
        ];
        assert_eq!(paths.len(), 7, "a path family lost its variant");
        let kinds: Vec<&str> = paths.iter().map(|(_, s)| s.kind()).collect();
        let mut sorted = kinds.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), kinds.len(), "two paths share a discriminator");
    }

    #[test]
    fn source_kind_matches_the_serde_tag() {
        // One decider (ARCH §10.6): a trace line and a stored row must not
        // be able to disagree about what this content is.
        for s in [
            Source::Web {
                url: "u".into(),
                fetched_at: 0,
            },
            Source::Memory {
                entry: ContentHash::of_str("m"),
            },
            Source::Note {
                note: ContentHash::of_str("n"),
            },
            Source::Conversation {
                conversation: ContentHash::of_str("c"),
                turn: 0,
            },
        ] {
            let v: serde_json::Value = serde_json::to_value(&s).unwrap();
            assert_eq!(v["kind"].as_str().unwrap(), s.kind());
        }
    }

    #[test]
    fn origin_round_trips_on_the_wire() {
        let o = corpus_origin();
        let j = serde_json::to_string(&o).unwrap();
        assert_eq!(serde_json::from_str::<Origin>(&j).unwrap(), o);
    }

    #[test]
    fn an_origin_missing_a_field_does_not_deserialize() {
        // The totality rule, checked rather than asserted in prose: there is
        // no Default and no Option, so a partial origin is not a value.
        let partial = r#"{"source":{"kind":"web","url":"u","fetched_at":0},"grain":"leaf"}"#;
        assert!(serde_json::from_str::<Origin>(partial).is_err());
    }

    #[test]
    fn an_empty_locator_is_refused() {
        assert_eq!(Locator::new(""), None);
        assert_eq!(Locator::new("  "), None);
    }
}
