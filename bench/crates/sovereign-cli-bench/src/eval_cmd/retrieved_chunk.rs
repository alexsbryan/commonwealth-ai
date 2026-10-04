//! One retrieved chunk as a run file records it — including what the model
//! actually read of it.
//!
//! Split out of `runner.rs` when that file hit its arch-gate ceiling.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedChunk {
    pub corpus_id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub score: f32,
    /// The chunk's full text, newlines flattened. NOT truncated.
    ///
    /// It was capped at 600 chars "to keep run files readable" — a cost
    /// nobody measured — and the cap silently biased every metric read
    /// off this field downward, because a fact past the cap scored as
    /// absent. Cap it again only when a measured run-file cost says to,
    /// and raise the consuming metrics' floor in the same commit.
    pub snippet: String,
    /// Provenance tag from the chunk's `metadata.source` — "raptor",
    /// "atlas", "atom-enum", or absent for organically-retrieved
    /// chunks. Makes structural-layer injection visible in the run file
    /// so a bench can confirm (not infer) which layer surfaced a hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Did this chunk reach the model at all?
    ///
    /// `None` = not known on this path (the runtime passes `None` to
    /// `project_retrieved_chunks` where no prompt was built), never a
    /// defaulted `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_prompt: Option<bool>,
    /// What the model ACTUALLY read of this chunk.
    ///
    /// The runtime has emitted this since the formatter gained
    /// `FormattedChunks::admitted`, and this struct dropped it — so every
    /// measurement taken off a run file was taken off `snippet` (the chunk as
    /// RETRIEVED) and silently attributed to what the model saw. Those are not
    /// the same text: chunk-level eviction is rare but per-chunk truncation to
    /// `MAX_CHUNK_CHARS` is near-universal, and the whole pool competes for one
    /// clamped character budget — so a pool that grows makes every seat
    /// smaller. A retrieval metric can improve while the model reads less, and
    /// nothing in the run file could show it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_text: Option<String>,
}
