// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-genre atlas extension payloads: descriptive, reflective, procedural,
//! lyric.
//!
//! These are the `TypeExtension` arms a section carries when its discourse
//! mode is one of those four — the container struct plus the leaf `*Sketch`
//! shapes it holds. `TypeExtension` itself, and the argumentative and
//! narrative arms, stay in [`super::atlas`] next to the core sketches the
//! ontology work extends; every type here is re-exported from that module, so
//! no caller's import changes.
//!
//! **The seam is size, not concept** (ARCH §3.1, and §11 — say what is true):
//! `atlas.rs` is a 1.6k-line file of pure data shapes and was over the
//! oversized ratchet. The cut takes four of the six genre families because the
//! arch-gate's approach band (800-1200 lines, no slack) forbids leaving the
//! residual inside it, so a clean "all six families" split is not available in
//! one step. The full split — core sketches, vocabulary enums, and all six
//! extension families as peers, each under 800 lines — is the roadmap entry in
//! `SYSTEM_OVERVIEW.md` §10, to be done when no other work is live in this file.

use serde::{Deserialize, Serialize};

use super::atlas::{is_empty_str, RelationSketch};

/// Descriptive discourse mode: definitions, property claims,
/// structural relationships, examples, provenance pointers. The
/// extractor that fires on `DiscourseMode::Descriptive` writes here
/// instead of (or alongside) the literary base schema's claims field.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DescriptiveExtension {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub definitions: Vec<DefinitionSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub property_claims: Vec<PropertyClaimSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<RelationSketch>,
    /// Concrete examples used to illustrate a definition / claim.
    /// Distinct from `evidence_invocations` on the argumentative
    /// extension — descriptive examples illustrate rather than
    /// support.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<ExampleSketch>,
    /// Source pointers — citations, footnotes, "as described in X"
    /// references. Empty when the section is voice-only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<ProvenanceSketch>,
}

impl DescriptiveExtension {
    pub fn atom_count(&self) -> usize {
        self.definitions.len()
            + self.property_claims.len()
            + self.relationships.len()
            + self.examples.len()
            + self.provenance.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionSketch {
    pub term: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PropertyClaimSketch {
    pub subject: String,
    pub property: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExampleSketch {
    pub label: String,
    pub content: String,
    /// Definition / claim / concept the example illustrates. Empty
    /// when ungrounded.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub illustrates: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvenanceSketch {
    /// Short label for the source — author + title, URL, or DOI-ish
    /// identifier.
    pub label: String,
    /// One sentence of context for the source.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub context: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// Reflective discourse mode: interactions with others / texts,
/// observations, open threads (questions or lines of thinking the
/// author leaves unresolved), mood shifts, realisations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReflectiveExtension {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interactions: Vec<InteractionSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<ObservationSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_threads: Vec<OpenThreadSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mood_shifts: Vec<MoodShiftSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub realisations: Vec<RealisationSketch>,
}

impl ReflectiveExtension {
    pub fn atom_count(&self) -> usize {
        self.interactions.len()
            + self.observations.len()
            + self.open_threads.len()
            + self.mood_shifts.len()
            + self.realisations.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InteractionSketch {
    /// Named other(s) the author interacted with — colleague, friend,
    /// author of a text. Empty when the interaction is with an
    /// inanimate object or a non-personalised idea.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub with: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObservationSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenThreadSketch {
    /// One sentence stating the unresolved thread — a question, a
    /// hunch the author can't yet pin down, a line of work to return
    /// to.
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MoodShiftSketch {
    /// Where the author started.
    pub from: String,
    /// Where the author ended.
    pub to: String,
    /// What moved them — the trigger / catalyst.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub catalyst: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RealisationSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// Procedural discourse mode: tasks (what will be done), decisions
/// (what was chosen), artifacts (the produced things), dependencies
/// (what blocks what), blockers (active obstacles), status signals
/// (progress markers).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProceduralExtension {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<DecisionSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<DependencySketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<BlockerSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_signals: Vec<StatusSignalSketch>,
}

impl ProceduralExtension {
    pub fn atom_count(&self) -> usize {
        self.tasks.len()
            + self.decisions.len()
            + self.artifacts.len()
            + self.dependencies.len()
            + self.blockers.len()
            + self.status_signals.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub owner: String,
    /// Free-form due-at hint — "by Thursday", "Q3", "before the merge
    /// freeze". The temporal modulator (task #36) may upgrade this to
    /// a structured date when context permits.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub due_at: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionSketch {
    pub content: String,
    /// Alternatives considered and rejected. Empty when the decision
    /// is voiced without alternatives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactSketch {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub description: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DependencySketch {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockerSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub blocks: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusSignalSketch {
    /// One of `done`, `in_progress`, `paused`, `cancelled`, `unknown`.
    /// Free-form to tolerate future expansion; routing on it stays
    /// optional.
    pub state: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// Lyric discourse mode: images, motifs, formal devices, voice shifts,
/// tonal movements. The atom shapes here are deliberately
/// expressive-domain — they don't try to recover argumentation or
/// fact-claims from verse.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LyricExtension {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub motifs: Vec<MotifSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formal_devices: Vec<FormalDeviceSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voice_shifts: Vec<VoiceShiftSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tonal_movements: Vec<TonalMovementSketch>,
}

impl LyricExtension {
    pub fn atom_count(&self) -> usize {
        self.images.len()
            + self.motifs.len()
            + self.formal_devices.len()
            + self.voice_shifts.len()
            + self.tonal_movements.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MotifSketch {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub description: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FormalDeviceSketch {
    /// Anaphora, enjambment, caesura, refrain, parallelism, etc.
    /// Free-form so the prompt can name a device without a fixed
    /// taxonomy.
    pub name: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub example: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceShiftSketch {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TonalMovementSketch {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}
