// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-`DiscourseMode` Phase 1 schemas, prompts, and parsers.
//!
//! Workstream B of the routed-Phase-1 plan, MECE-axis revision.
//! Each submodule covers one `DiscourseMode` from the Phase 0
//! classifier's vector and exposes:
//!
//!   - `PHASE1_<MODE>_SYSTEM` — the system preamble (markdown).
//!   - `phase1_<mode>_schema()` — the JSON schema for grammar-
//!     constrained decoding.
//!   - `parse_phase1_<mode>(response)` — strict parser that turns
//!     the model's JSON into the mode's typed extension. Returns a
//!     `TypeExtension` variant.
//!
//! Pipelines that opt into routed Phase 1 (today: `obsidian_atlas`)
//! consult `cache/section_classifications.json`, dispatch on each
//! section's discourse-mode distribution (primary + secondaries
//! above `DISCOURSE_ROUTING_THRESHOLD`), and call every active
//! mode's compose/parse pair. Literary and philosophy pipelines stay
//! on their existing prompts unchanged — they don't read the cache.

pub mod argumentative;
pub mod descriptive;
pub mod lyric;
pub mod modulators;
pub mod narrative;
pub mod procedural;
pub mod reflective;
pub mod source_recovery;

pub use source_recovery::{
    render_source_recovery_block, SOURCE_RECOVERY_DISCIPLINE, SOURCE_RECOVERY_QUOTE_CHAR_CAP,
};
