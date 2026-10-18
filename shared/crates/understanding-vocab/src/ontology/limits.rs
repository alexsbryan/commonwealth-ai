// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ranges a recipe's size levers are held to at load (`recipe_ontology`
//! refuses outside them rather than clamping).

/// The narrowest `max_entities_per_section` a recipe may declare. Below this
/// a section cannot carry even a short list, so a number under it is an
/// authoring mistake rather than a choice.
pub const MIN_ENTITIES_PER_SECTION: usize = 5;
/// The widest `max_entities_per_section` a recipe may declare. The Phase-1
/// array caps are runaway prevention, not data-quality caps
/// (`PHASE1_SECTION_EXTRACTION_SCHEMA`'s own note); past this the cap stops
/// preventing runaway and one section can swallow the token budget.
pub const MAX_ENTITIES_PER_SECTION: usize = 60;
/// The smallest `window_chars`: below it a window holds less than one mail
/// message of the worlds (their longest own text runs to ~2k characters).
pub const MIN_WINDOW_CHARS: usize = 2_000;
/// The largest `window_chars`: the largest window read (40k, 2026-10-16 and
/// 2026-10-18), which fits the 128k call with the reader's 64k answer at any
/// script down to 1.5 characters a token (REVIEW_BRIEF, "no one-way doors").
pub const MAX_WINDOW_CHARS: usize = 40_000;
