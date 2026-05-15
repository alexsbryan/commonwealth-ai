//! Epistemic + Temporal modulators for the routed-Phase-1 fan-out.
//!
//! After the dispatcher fires every active discourse-mode extension
//! and collects the resulting `Vec<TypeExtension>`, two post-extraction
//! passes apply the two non-discourse axes from the section's
//! classification vector:
//!
//! - **Epistemic posture** modulator: tags claims / positions with
//!   `normative_marker` when the section's posture is `Normative`,
//!   and with `scope: counterfactual` when the posture is
//!   `Hypothetical`. Story-world atoms produced under `Fictional`
//!   get a `story_world: true` flag so downstream retrieval can
//!   scope appropriately.
//! - **Temporal frame** modulator: tags `events` / `tasks` with
//!   `when` (Episodic), leaves them unbound (Atemporal), or attaches
//!   `target_state` (Prospective).
//!
//! **v1 stub.** The atom shapes don't yet carry the modulator-flag
//! fields these functions would write to. The functions are wired
//! into the dispatcher's fan-out (task #35) so downstream consumers
//! can opt into modulator-aware logic once the shapes grow the
//! fields. Today the modulators record the axis values in a span-
//! local `ModulatorContext` that the dispatcher attaches to the
//! `SectionExtraction` for telemetry.

use crate::enrichment::pipeline::atlas::TypeExtension;
use crate::enrichment::pipeline::types::{EpistemicPosture, TemporalFrame};

/// Side-channel context the dispatcher attaches alongside the
/// extracted `Vec<TypeExtension>`. Encodes the axis values the
/// modulator pass *would* tag atoms with once shapes grow the
/// fields. Lives next to the extensions so a future consumer can
/// rebuild typed flags from the cache without re-classifying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModulatorContext {
    pub epistemic_posture: EpistemicPosture,
    pub temporal_frame: TemporalFrame,
}

impl ModulatorContext {
    pub fn new(epistemic_posture: EpistemicPosture, temporal_frame: TemporalFrame) -> Self {
        Self {
            epistemic_posture,
            temporal_frame,
        }
    }
}

/// Apply the epistemic-posture pass. v1: no-op on the atom shapes
/// (they don't carry the flag fields yet) — the function exists so
/// the dispatcher can call it unconditionally and so future atom
/// growth can wire in without changing the call sites.
///
/// Returns `extensions` unmodified; the dispatcher passes
/// `ModulatorContext` to the cache writer separately.
pub fn apply_epistemic_modulator(
    extensions: Vec<TypeExtension>,
    _posture: EpistemicPosture,
) -> Vec<TypeExtension> {
    extensions
}

/// Apply the temporal-frame pass. Same v1 no-op shape as
/// `apply_epistemic_modulator`. The hook is wired so the temporal
/// signal records on the section even when the atom shapes can't
/// yet carry per-atom timestamps.
pub fn apply_temporal_modulator(
    extensions: Vec<TypeExtension>,
    _frame: TemporalFrame,
) -> Vec<TypeExtension> {
    extensions
}

/// Convenience — apply both modulators in order.
pub fn apply_modulators(
    extensions: Vec<TypeExtension>,
    ctx: ModulatorContext,
) -> Vec<TypeExtension> {
    let mid = apply_epistemic_modulator(extensions, ctx.epistemic_posture);
    apply_temporal_modulator(mid, ctx.temporal_frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::atlas::{ArgumentativeExtension, TypeExtension};

    #[test]
    fn no_op_v1_preserves_input() {
        let ext = TypeExtension::Argumentative(ArgumentativeExtension::default());
        let out = apply_modulators(
            vec![ext.clone()],
            ModulatorContext::new(EpistemicPosture::Normative, TemporalFrame::Atemporal),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], ext);
    }
}
