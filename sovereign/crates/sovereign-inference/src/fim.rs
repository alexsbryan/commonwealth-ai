// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fill-in-the-middle (FIM) inline-completion support — the prompt
//! builder, the marker-style vocab probe, and the stream stop
//! tracker (`sovereign/docs/INLINE_COMPLETION.md`).
//!
//! FIM for the coder families we ship is expressed as a plain-text
//! prompt using the model's own special-token markers, tokenized with
//! special-token parsing on and no chat-template wrapping
//! (`PromptShape::Raw`). This module owns:
//!
//! - the **marker table** ([`FimMarkers`]) — one row per supported
//!   family; a second family is a table addition, not a rewrite;
//! - [`build_fim_prompt`] — PSM-ordered assembly
//!   (`{prefix-marker}{prefix}{suffix-marker}{suffix}{middle-marker}`),
//!   which is both Qwen's documented shape and prefix-cache friendly
//!   (the prefix section only appends as the user types);
//! - [`detect_fim_style`] — vocab probe run once at slot install.
//!   `ModelFamily` is `Unknown` on all production slots, so family-
//!   keyed detection won't work; instead every marker must tokenize
//!   to EXACTLY ONE token in the loaded model's vocab. No match →
//!   `None` → the daemon refuses the slot with an actionable message;
//! - [`FimStopTracker`] — the pure stream filter that decides when a
//!   completion is done (INLINE_COMPLETION.md §3.3). F0 implements
//!   the stop-string scan with a holdback buffer (a stop string split
//!   across token boundaries must never leak into the suggestion);
//!   the single/multi-line mode decision, depth tracking, and suffix
//!   dedupe land in F1.


// The pure FIM text moved DOWN to `sovereign-contracts` (domains
// `REVIEW-build-serving-drop-inference`): it is arithmetic over `FimStyle`,
// which already lives there, and the serving host must reach it without
// linking the inference stack. Re-exported here so
// `sovereign_inference::fim::<Item>` stays valid for every caller.
pub use sovereign_contracts::fim::*;

use sovereign_core::types::FimStyle;

use crate::llama::cpp::model::{AddBos, LlamaModel};

/// Probe the loaded model's vocab for a FIM marker set. Every marker
/// (prefix/suffix/middle) must tokenize to EXACTLY ONE token — a
/// marker that splits into pieces means the model was never trained
/// with it as an atomic unit and FIM prompting would degrade into
/// garbage. Returns the first matching table row, `None` when no
/// family's markers are all atomic (caller refuses the slot).
pub fn detect_fim_style(model: &LlamaModel) -> Option<FimStyle> {
    'rows: for row in FIM_MARKER_TABLE {
        for marker in row
            .also_requires
            .iter()
            .chain([row.prefix, row.suffix, row.middle].iter())
        {
            match model.str_to_token(marker, AddBos::Never) {
                Ok(tokens) if tokens.len() == 1 => {}
                _ => continue 'rows,
            }
        }
        return Some(row.style);
    }
    None
}
