// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cross-corpus edge records (`atlas/cross_corpus_edges.json`) and the
//! detector's glass-box report types, with the file's reader. The detector
//! that produces them is corpus-engine's `cross_corpus`; the atlas view reads
//! them here without the engine (pb-ingest-dial-tools).

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use understanding_vocab::atoms::AtomId;
use understanding_vocab::edges::Edge;

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
    pub peer: CrossCorpusAtomRef,
    /// Why this edge exists — the exact signal path the detector
    /// took. Surfaced via `sovereign enrich atlas-cross-corpus
    /// --explain <edge-id>`.
    pub trace: MatchTrace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossCorpusAtomRef {
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
        mirror.peer = CrossCorpusAtomRef {
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

/// Read the cross-corpus edges file back from disk. Used by
/// traversal + operator inspection paths.
pub fn read_atlas_cross_corpus_edges(atlas_dir: &Path) -> io::Result<CrossCorpusEdgesFile> {
    let path = atlas_dir.join("cross_corpus_edges.json");
    let data = fs::read(&path)?;
    serde_json::from_slice(&data).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("parse cross_corpus_edges.json: {e}"),
        )
    })
}
