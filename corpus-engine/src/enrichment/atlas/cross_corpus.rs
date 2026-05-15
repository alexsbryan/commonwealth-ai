//! Cross-corpus edge detection — Phase C Step 8.
//!
//! Connects atoms across two (or more) corpora's atlases. An atom
//! in corpus A is often the same *thing* as an atom in corpus B —
//! the reader's "Kant" in a literary atlas IS the SEP
//! article's Kant entity atom. When that relationship holds, a
//! cross-corpus edge captures it so a brief assembled from one
//! atlas can pull grounding from the other.
//!
//! Three edge types per spec §3.1:
//!
//! - **Grounding** — entity↔entity by canonical_name / alias match.
//!   The cheapest, most deterministic detector. Ships now.
//! - **Framing** — manifest overlap + LLM verification. Reserved
//!   for a follow-up landing.
//! - **Provenance** — citation extraction from claim/description
//!   text (Paper A cites Paper B). Reserved for a follow-up.
//!
//! ## Glass-box observability
//!
//! This module's public surface is explicitly designed for
//! operator inspection, not just output collection. Every
//! detector returns a [`CrossCorpusReport`] with:
//!
//! - A summary line per detector (candidates scanned, matches
//!   accepted, rejections grouped by reason).
//! - Sample rejections with the exact folded forms that failed,
//!   so an operator auditing the output can see where the
//!   detector drew its line.
//! - Per-edge [`MatchTrace`] captured at accept time, carrying
//!   the exact signal path (what matched, how strong, alternatives
//!   considered).
//!
//! The CLI layer surfaces these traces behind an `--explain` flag
//! so a user can ask "why was this edge created?" and see the
//! full decision path instead of a confidence number and nothing
//! else.

use serde::{Deserialize, Serialize};

use super::atoms::{AtomId, ChunkRef, Entity};
use super::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use super::resolution::fold;

// ── Edge record ──────────────────────────────────────────────

/// A single directed cross-corpus edge. Uses the canonical `Edge`
/// shape (so readers can treat cross-corpus and intra-corpus edges
/// uniformly) plus a `cross_corpus` envelope carrying the opposite-
/// side corpus id + the match trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossCorpusEdge {
    /// Inner edge — `edge_type` is always one of `Grounding`,
    /// `Framing`, or `Provenance`. `source` and `target` atom ids
    /// point at atoms on the **local** (this file's) corpus side;
    /// the matching atom on the other corpus lives in
    /// `peer.atom_id`.
    pub edge: Edge,
    /// Opposite-side reference. A traversal walking the bridge
    /// uses `peer.corpus_id + peer.atom_id` to open the other
    /// atlas and continue.
    pub peer: PeerAtomRef,
    /// Why this edge exists — the exact signal path the detector
    /// took. Surfaced via `sovereign enrich atlas-cross-corpus
    /// --explain <edge-id>`.
    pub trace: MatchTrace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerAtomRef {
    pub corpus_id: String,
    pub atom_id: AtomId,
    /// Canonical_name on the peer side, copied in so a traversal
    /// that hasn't opened the peer atlas yet still has a
    /// human-readable anchor.
    pub canonical_name: String,
}

impl CrossCorpusEdge {
    /// Produce the mirror-view of this edge for the peer corpus.
    /// Swaps `source`/`target` atom ids, flips the `local`/`peer`
    /// canonical name in the trace, and updates the peer reference
    /// so corpus B's file reads "my atom X → bk's atom Y" instead
    /// of "bk's atom X → my atom Y".
    ///
    /// Takes the local entity's canonical_name because it's not
    /// stored on the edge itself (only the atom id is). Callers
    /// reach into the local atlas's entities to fetch it.
    pub fn flip_for_peer(&self, local_canonical_name: String, local_corpus_id: String) -> Self {
        let mut mirror = self.clone();
        std::mem::swap(&mut mirror.edge.source, &mut mirror.edge.target);
        let new_peer_atom_id = mirror.edge.target.clone();
        mirror.peer = PeerAtomRef {
            corpus_id: local_corpus_id,
            atom_id: new_peer_atom_id,
            canonical_name: local_canonical_name,
        };
        std::mem::swap(&mut mirror.trace.local_form, &mut mirror.trace.peer_form);
        mirror
    }
}

/// Detailed record of what the detector saw when it accepted this
/// edge. The CLI's `--explain` flag pretty-prints this; the brief
/// assembler can also surface a short form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchTrace {
    /// Which detector produced this edge — `"grounding"`,
    /// `"framing"`, `"provenance"`.
    pub detector: String,
    /// Which signal fired — `"canonical_exact"`,
    /// `"alias_exact"`, `"canonical_token_unique"`, etc. Stable
    /// tag so tests can pin behaviour.
    pub signal: String,
    /// Folded text form on the local side.
    pub local_form: String,
    /// Folded text form on the peer side.
    pub peer_form: String,
    /// 0.0–1.0; exact matches are 1.0, token-unique matches
    /// drop to 0.8, LLM-verified framing drops further.
    pub confidence: f32,
    /// Alternatives the detector considered but rejected. Empty
    /// when there was no competing candidate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected_alternatives: Vec<String>,
}

// ── On-disk file ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossCorpusEdgesFile {
    pub schema_version: String,
    pub local_corpus_id: String,
    pub edges: Vec<CrossCorpusEdge>,
}

impl CrossCorpusEdgesFile {
    pub const SCHEMA_VERSION: &'static str = "2.0";

    pub fn new(local_corpus_id: String, edges: Vec<CrossCorpusEdge>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.to_string(),
            local_corpus_id,
            edges,
        }
    }
}

// ── Glass-box report ─────────────────────────────────────────

/// Summary + diagnostics returned by every detector call. The CLI
/// prints this verbatim; tests assert on individual counts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrossCorpusReport {
    pub detectors: Vec<DetectorSummary>,
    pub accepted_edges: Vec<CrossCorpusEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectorSummary {
    pub detector: String,
    pub candidates_scanned: usize,
    pub matches_accepted: usize,
    pub rejections_by_reason: Vec<RejectionBucket>,
    /// Cap-limited sample of concrete rejected pairs, for the
    /// operator to spot systematic misses. Not the full list —
    /// we keep the report fixed-size regardless of corpus size.
    pub sample_rejections: Vec<RejectionSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectionBucket {
    pub reason: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectionSample {
    pub local_atom_id: AtomId,
    pub peer_atom_id: AtomId,
    pub local_form: String,
    pub peer_form: String,
    pub reason: String,
}

/// Maximum number of rejection samples any single detector carries
/// in its report. Keeps the report payload bounded regardless of
/// the corpora's size.
const MAX_SAMPLE_REJECTIONS: usize = 10;

// ── Grounding detector ───────────────────────────────────────

/// Inputs for a cross-corpus detection pass. Borrow-only — the
/// caller owns the loaded atlas state.
#[derive(Debug, Clone, Copy)]
pub struct CrossCorpusInput<'a> {
    pub local_corpus_id: &'a str,
    pub local_entities: &'a [Entity],
    pub peer_corpus_id: &'a str,
    pub peer_entities: &'a [Entity],
}

/// Detect Grounding edges between two atlases.
///
/// Algorithm (deterministic, no LLM):
///
/// 1. Build a name-index for the peer side — `fold(name)` →
///    peer entity id. Covers both canonical_name and aliases.
/// 2. For each local entity, try the signal ladder in order:
///    - `canonical_exact` — fold(local.canonical) matches a peer
///      name (canonical or alias).
///    - `alias_exact` — any fold(local.alias) matches a peer
///      name.
///    - `canonical_token_unique` — fold(local.canonical) has a
///      long token (len ≥ 5) that appears in exactly one peer
///      entity's token set. Confidence 0.8 (vs 1.0 for exact).
/// 3. If the ladder returns None, record a rejection with the
///    reason that got us furthest (e.g. "no shared long token"
///    beats "no matching name"). Rejection samples are
///    truncated to `MAX_SAMPLE_REJECTIONS`.
///
/// The output is bidirectional-ready: callers write the same
/// edges into both corpora's `cross_corpus_edges.json`, flipping
/// `local`/`peer` as they go. See [`write_both_sides`] for the
/// writer helper.
pub fn detect_grounding(input: CrossCorpusInput<'_>) -> CrossCorpusReport {
    let mut accepted: Vec<CrossCorpusEdge> = Vec::new();
    let mut rejections_by_reason: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    let mut sample_rejections: Vec<RejectionSample> = Vec::new();

    // Build peer name index (fold → peer entity) + peer long-token
    // inverted index.
    let mut peer_name_index: std::collections::HashMap<String, &Entity> =
        std::collections::HashMap::new();
    let mut peer_token_index: std::collections::HashMap<String, Vec<&Entity>> =
        std::collections::HashMap::new();
    for e in input.peer_entities {
        for name in std::iter::once(&e.canonical_name).chain(e.aliases.iter()) {
            let folded = fold(name);
            if folded.is_empty() {
                continue;
            }
            peer_name_index.insert(folded.clone(), e);
            for token in folded.split_whitespace() {
                if token.len() < 5 {
                    continue;
                }
                let bucket = peer_token_index.entry(token.to_string()).or_default();
                if !bucket.iter().any(|pe| pe.id == e.id) {
                    bucket.push(e);
                }
            }
        }
    }

    let candidates_scanned = input.local_entities.len();
    let mut next_edge_ordinal: usize = 1;

    for local in input.local_entities {
        // Try the signal ladder.
        let signal_result = try_grounding_signals(local, &peer_name_index, &peer_token_index);

        match signal_result {
            GroundingSignal::CanonicalExact { peer, local_form, peer_form } => {
                accepted.push(build_edge(
                    next_edge_ordinal,
                    input.local_corpus_id,
                    input.peer_corpus_id,
                    local,
                    peer,
                    "canonical_exact",
                    local_form,
                    peer_form,
                    1.0,
                    Vec::new(),
                ));
                next_edge_ordinal += 1;
            }
            GroundingSignal::AliasExact { peer, local_form, peer_form } => {
                accepted.push(build_edge(
                    next_edge_ordinal,
                    input.local_corpus_id,
                    input.peer_corpus_id,
                    local,
                    peer,
                    "alias_exact",
                    local_form,
                    peer_form,
                    1.0,
                    Vec::new(),
                ));
                next_edge_ordinal += 1;
            }
            GroundingSignal::TokenUnique {
                peer,
                local_form,
                peer_form,
                alternatives,
            } => {
                accepted.push(build_edge(
                    next_edge_ordinal,
                    input.local_corpus_id,
                    input.peer_corpus_id,
                    local,
                    peer,
                    "canonical_token_unique",
                    local_form,
                    peer_form,
                    0.8,
                    alternatives,
                ));
                next_edge_ordinal += 1;
            }
            GroundingSignal::Ambiguous { sample_peer, local_form, peer_form } => {
                let reason = "ambiguous_token_match";
                *rejections_by_reason.entry(reason).or_insert(0) += 1;
                if sample_rejections.len() < MAX_SAMPLE_REJECTIONS {
                    sample_rejections.push(RejectionSample {
                        local_atom_id: local.id.clone(),
                        peer_atom_id: sample_peer.id.clone(),
                        local_form,
                        peer_form,
                        reason: reason.to_string(),
                    });
                }
            }
            GroundingSignal::NoMatch { local_form } => {
                let reason = "no_shared_name_or_token";
                *rejections_by_reason.entry(reason).or_insert(0) += 1;
                if sample_rejections.len() < MAX_SAMPLE_REJECTIONS {
                    sample_rejections.push(RejectionSample {
                        local_atom_id: local.id.clone(),
                        peer_atom_id: AtomId::from_raw("n/a"),
                        local_form,
                        peer_form: String::new(),
                        reason: reason.to_string(),
                    });
                }
            }
        }
    }

    let matches_accepted = accepted.len();
    let summary = DetectorSummary {
        detector: "grounding".to_string(),
        candidates_scanned,
        matches_accepted,
        rejections_by_reason: rejections_by_reason
            .into_iter()
            .map(|(reason, count)| RejectionBucket {
                reason: reason.to_string(),
                count,
            })
            .collect(),
        sample_rejections,
    };

    CrossCorpusReport {
        detectors: vec![summary],
        accepted_edges: accepted,
    }
}

/// Internal — the outcome of running the signal ladder on one
/// local entity.
enum GroundingSignal<'a> {
    CanonicalExact {
        peer: &'a Entity,
        local_form: String,
        peer_form: String,
    },
    AliasExact {
        peer: &'a Entity,
        local_form: String,
        peer_form: String,
    },
    TokenUnique {
        peer: &'a Entity,
        local_form: String,
        peer_form: String,
        alternatives: Vec<String>,
    },
    Ambiguous {
        sample_peer: &'a Entity,
        local_form: String,
        peer_form: String,
    },
    NoMatch {
        local_form: String,
    },
}

fn try_grounding_signals<'a>(
    local: &Entity,
    peer_name_index: &std::collections::HashMap<String, &'a Entity>,
    peer_token_index: &std::collections::HashMap<String, Vec<&'a Entity>>,
) -> GroundingSignal<'a> {
    let local_folded = fold(&local.canonical_name);

    // 1. canonical_exact: fold(canonical) matches a peer name.
    if let Some(peer) = peer_name_index.get(&local_folded) {
        return GroundingSignal::CanonicalExact {
            peer,
            local_form: local_folded.clone(),
            peer_form: fold(&peer.canonical_name),
        };
    }

    // 2. alias_exact: fold(local.alias) matches a peer name.
    for alias in &local.aliases {
        let alias_folded = fold(alias);
        if let Some(peer) = peer_name_index.get(&alias_folded) {
            return GroundingSignal::AliasExact {
                peer,
                local_form: alias_folded,
                peer_form: fold(&peer.canonical_name),
            };
        }
    }

    // 3. canonical_token_unique: a long token of the local
    //    canonical appears in exactly one peer entity's tokens.
    for q_token in local_folded.split_whitespace() {
        if q_token.len() < 5 {
            continue;
        }
        if let Some(bucket) = peer_token_index.get(q_token) {
            if bucket.len() == 1 {
                let peer = bucket[0];
                return GroundingSignal::TokenUnique {
                    peer,
                    local_form: q_token.to_string(),
                    peer_form: fold(&peer.canonical_name),
                    alternatives: Vec::new(),
                };
            } else if bucket.len() > 1 {
                // Multi-owner — rejection; record and let outer
                // loop continue to the next signal (or fall
                // through to NoMatch if nothing else snaps).
                return GroundingSignal::Ambiguous {
                    sample_peer: bucket[0],
                    local_form: q_token.to_string(),
                    peer_form: fold(&bucket[0].canonical_name),
                };
            }
        }
    }

    GroundingSignal::NoMatch {
        local_form: local_folded,
    }
}

fn build_edge(
    ordinal: usize,
    local_corpus_id: &str,
    peer_corpus_id: &str,
    local: &Entity,
    peer: &Entity,
    signal: &str,
    local_form: String,
    peer_form: String,
    confidence: f32,
    rejected_alternatives: Vec<String>,
) -> CrossCorpusEdge {
    let edge = Edge {
        id: EdgeId::from_raw(format!("cc-{}-{:04}", local_corpus_id, ordinal)),
        edge_type: EdgeType::Grounding,
        source: local.id.clone(),
        target: peer.id.clone(),
        evidence: Vec::<ChunkRef>::new(),
        trigger_event: None,
        sub_question: None,
        confidence,
        provenance: EdgeProvenance::Derived,
    };
    CrossCorpusEdge {
        edge,
        peer: PeerAtomRef {
            corpus_id: peer_corpus_id.to_string(),
            atom_id: peer.id.clone(),
            canonical_name: peer.canonical_name.clone(),
        },
        trace: MatchTrace {
            detector: "grounding".to_string(),
            signal: signal.to_string(),
            local_form,
            peer_form,
            confidence,
            rejected_alternatives,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    fn entity(idx: usize, name: &str, aliases: &[&str]) -> Entity {
        Entity {
            id: AtomId::entity(idx),
            canonical_name: name.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: format!("{name} description"),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
                    concept_kind: None,
}
}

    #[test]
    fn grounding_matches_canonical_exact() {
        // Both atlases have "Kant" as a canonical name; exact
        // fold match fires the highest-confidence signal.
        let a = vec![entity(1, "Kant", &[])];
        let b = vec![entity(1, "Kant", &[])];
        let rep = detect_grounding(CrossCorpusInput {
            local_corpus_id: "bk",
            local_entities: &a,
            peer_corpus_id: "sep",
            peer_entities: &b,
        });
        assert_eq!(rep.accepted_edges.len(), 1);
        let e = &rep.accepted_edges[0];
        assert_eq!(e.trace.signal, "canonical_exact");
        assert_eq!(e.trace.confidence, 1.0);
        assert_eq!(e.edge.edge_type, EdgeType::Grounding);
        assert_eq!(e.peer.corpus_id, "sep");
        assert_eq!(e.peer.canonical_name, "Kant");
    }

    #[test]
    fn grounding_matches_via_alias() {
        // Local side has "Shakespeare's Ophelia" as canonical
        // plus "Ophelia" as alias; peer has just "Ophelia"
        // canonical. Alias-exact fires.
        let a = vec![entity(1, "Shakespeare's Ophelia", &["Ophelia"])];
        let b = vec![entity(42, "Ophelia", &[])];
        let rep = detect_grounding(CrossCorpusInput {
            local_corpus_id: "bk",
            local_entities: &a,
            peer_corpus_id: "hamlet",
            peer_entities: &b,
        });
        assert_eq!(rep.accepted_edges.len(), 1);
        assert_eq!(rep.accepted_edges[0].trace.signal, "alias_exact");
    }

    #[test]
    fn grounding_folds_diacritics_on_both_sides() {
        // Diacritic-only drift: `Alexéi Karámazov` and `Alexei
        // Karamazov` fold to identical forms (NFD → drop combining
        // marks). The detector's exact-fold path must snap them
        // together at confidence 1.0.
        //
        // Note: drift that includes *letter-shape* differences
        // (`č` vs `ch`) is NOT caught by the exact path — it
        // falls through to the token-unique ladder step. A
        // separate test covers that.
        let a = vec![entity(1, "Alexéi Karámazov", &[])];
        let b = vec![entity(99, "Alexei Karamazov", &[])];
        let rep = detect_grounding(CrossCorpusInput {
            local_corpus_id: "bk",
            local_entities: &a,
            peer_corpus_id: "study",
            peer_entities: &b,
        });
        assert_eq!(rep.accepted_edges.len(), 1);
        assert_eq!(rep.accepted_edges[0].trace.signal, "canonical_exact");
    }

    #[test]
    fn grounding_drift_with_letter_shape_divergence_falls_through_to_token_unique() {
        // `pavlovič` vs `pavlovich` differ by letter shape (č
        // decomposes to c, which doesn't match `ch`). Exact-fold
        // misses; the unique long-token `fyodor` (or `karamazov`)
        // on each side carries the bridge at confidence 0.8.
        let a = vec![entity(1, "Fyodor Pavlovič Karamazov", &[])];
        let b = vec![entity(99, "Fyodor Pavlovich Karamazov", &[])];
        let rep = detect_grounding(CrossCorpusInput {
            local_corpus_id: "bk",
            local_entities: &a,
            peer_corpus_id: "study",
            peer_entities: &b,
        });
        assert_eq!(rep.accepted_edges.len(), 1);
        assert_eq!(
            rep.accepted_edges[0].trace.signal,
            "canonical_token_unique"
        );
    }

    #[test]
    fn grounding_falls_back_to_unique_long_token() {
        // No exact match but local's "Frankfurt" token appears in
        // exactly one peer's tokens — the SEP Frankfurt-Cases
        // article. Token-unique signal fires at 0.8 confidence.
        let a = vec![entity(1, "Frankfurt cases", &[])];
        let b = vec![entity(10, "Frankfurt and moral responsibility", &[])];
        let rep = detect_grounding(CrossCorpusInput {
            local_corpus_id: "compat",
            local_entities: &a,
            peer_corpus_id: "sep",
            peer_entities: &b,
        });
        assert_eq!(rep.accepted_edges.len(), 1);
        assert_eq!(
            rep.accepted_edges[0].trace.signal,
            "canonical_token_unique"
        );
        assert!((rep.accepted_edges[0].trace.confidence - 0.8).abs() < 1e-6);
    }

    #[test]
    fn grounding_rejects_ambiguous_token() {
        // "Karamazov" appears in multiple peer entities → token
        // fallback refuses to snap. Rejection recorded with the
        // `ambiguous_token_match` reason.
        let a = vec![entity(1, "Karamazov Study", &[])];
        let b = vec![
            entity(10, "Alexei Fyodorovich Karamazov", &[]),
            entity(11, "Dmitri Fyodorovich Karamazov", &[]),
            entity(12, "Ivan Fyodorovich Karamazov", &[]),
        ];
        let rep = detect_grounding(CrossCorpusInput {
            local_corpus_id: "study",
            local_entities: &a,
            peer_corpus_id: "bk",
            peer_entities: &b,
        });
        assert!(rep.accepted_edges.is_empty());
        let s = &rep.detectors[0];
        assert_eq!(s.matches_accepted, 0);
        assert_eq!(
            s.rejections_by_reason
                .iter()
                .find(|b| b.reason == "ambiguous_token_match")
                .map(|b| b.count),
            Some(1)
        );
        assert_eq!(s.sample_rejections.len(), 1);
        assert_eq!(s.sample_rejections[0].reason, "ambiguous_token_match");
    }

    #[test]
    fn grounding_rejects_when_no_signal_path_resolves() {
        let a = vec![entity(1, "Alyosha", &[])];
        let b = vec![entity(42, "Wittgenstein", &[])];
        let rep = detect_grounding(CrossCorpusInput {
            local_corpus_id: "bk",
            local_entities: &a,
            peer_corpus_id: "sep",
            peer_entities: &b,
        });
        assert!(rep.accepted_edges.is_empty());
        let s = &rep.detectors[0];
        assert_eq!(s.candidates_scanned, 1);
        assert_eq!(
            s.rejections_by_reason
                .iter()
                .find(|b| b.reason == "no_shared_name_or_token")
                .map(|b| b.count),
            Some(1)
        );
    }

    #[test]
    fn grounding_caps_rejection_samples_at_fixed_size() {
        // 20 local entities, none of which match the single
        // peer entity. Samples must cap at MAX_SAMPLE_REJECTIONS.
        let a: Vec<Entity> = (1..=20)
            .map(|i| entity(i, &format!("local-{i}"), &[]))
            .collect();
        let b = vec![entity(99, "Peer-single", &[])];
        let rep = detect_grounding(CrossCorpusInput {
            local_corpus_id: "local",
            local_entities: &a,
            peer_corpus_id: "peer",
            peer_entities: &b,
        });
        let s = &rep.detectors[0];
        assert_eq!(s.candidates_scanned, 20);
        assert_eq!(s.matches_accepted, 0);
        assert_eq!(s.sample_rejections.len(), MAX_SAMPLE_REJECTIONS);
        // Bucket still records the total, even though samples cap.
        assert_eq!(
            s.rejections_by_reason
                .iter()
                .find(|b| b.reason == "no_shared_name_or_token")
                .map(|b| b.count),
            Some(20)
        );
    }

    #[test]
    fn flip_for_peer_swaps_source_target_and_rewrites_peer_ref() {
        // The bidirectional write pattern: after `detect_grounding`
        // produces an edge for corpus A → B, the caller calls
        // `flip_for_peer` to build the mirror view B writes.
        let a = vec![entity(7, "Kant", &[])];
        let b = vec![entity(42, "Kant", &[])];
        let rep = detect_grounding(CrossCorpusInput {
            local_corpus_id: "bk",
            local_entities: &a,
            peer_corpus_id: "sep",
            peer_entities: &b,
        });
        let a_edge = &rep.accepted_edges[0];
        assert_eq!(a_edge.edge.source.as_str(), "entity-0007"); // local (bk)
        assert_eq!(a_edge.edge.target.as_str(), "entity-0042"); // peer (sep)
        assert_eq!(a_edge.peer.corpus_id, "sep");

        let b_edge = a_edge.flip_for_peer("Kant".to_string(), "bk".to_string());
        assert_eq!(b_edge.edge.source.as_str(), "entity-0042"); // local (sep) now
        assert_eq!(b_edge.edge.target.as_str(), "entity-0007"); // peer (bk) now
        assert_eq!(b_edge.peer.corpus_id, "bk");
        assert_eq!(b_edge.peer.atom_id.as_str(), "entity-0007");
        assert_eq!(b_edge.peer.canonical_name, "Kant");
        // Signal + confidence preserved — flipping is view-only.
        assert_eq!(b_edge.trace.signal, a_edge.trace.signal);
        assert_eq!(b_edge.trace.confidence, a_edge.trace.confidence);
    }

    #[test]
    fn grounding_writes_atom_ids_pointing_at_respective_corpora() {
        // Edge sanity: the `edge.source` points at the local
        // atom id, `edge.target` + `peer.atom_id` point at the
        // peer's. Lets a traversal walk from either side.
        let a = vec![entity(7, "Zossima", &[])];
        let b = vec![entity(13, "Zossima", &[])];
        let rep = detect_grounding(CrossCorpusInput {
            local_corpus_id: "bk",
            local_entities: &a,
            peer_corpus_id: "notes",
            peer_entities: &b,
        });
        assert_eq!(rep.accepted_edges.len(), 1);
        let e = &rep.accepted_edges[0];
        assert_eq!(e.edge.source.as_str(), "entity-0007");
        assert_eq!(e.edge.target.as_str(), "entity-0013");
        assert_eq!(e.peer.atom_id.as_str(), "entity-0013");
    }
}
