// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code-editing assistance contract types — which model serves editing
//! help, and **which of the two lanes it can actually serve**
//! (`sovereign/docs/INLINE_COMPLETION.md`, `sovereign/docs/NEXT_EDIT.md`).
//! These cross the `InferenceProvider` seam so the daemon's HTTP layer
//! can report slot state (`/status.inference.edit`) without knowing
//! engine internals.
//!
//! # Two lanes, not one slot with a mode
//!
//! Editing assistance is two genuinely different capabilities that a
//! single model may serve either, both, or neither of:
//!
//! - **Next-edit suggestion (NES)** — `POST /v1/edit_predictions`.
//!   Proposes a rewrite of an editable region given recent edit
//!   history. Rides the model's ordinary prompt surface (the chat
//!   template for `region_instruct`, a raw completion prompt for
//!   `zeta2`/`sweep`), so **any competent chat model can serve it**.
//!   This is the bulk use case.
//! - **Fill-in-the-middle (FIM)** — `POST /v1/completions`. Classic
//!   inline completion between a prefix and a suffix. Requires FIM
//!   marker tokens in the model's *vocabulary*, which only
//!   purpose-built coder models carry (Mellum, Qwen2.5-Coder,
//!   StarCoder2, Seed-Coder).
//!
//! The lanes were once one struct with a mandatory [`FimStyle`], which
//! made the marker probe a gate on *both*: a user whose only model was
//! an ordinary chat model got no editing assistance at all, even
//! though NES could have served them. Measured on the 60-case gen bank
//! (2026-08-07), a chat primary on `region_instruct` with thinking off
//! scored 21/30 useful with 0 wrong edits, against a 1.5B next-edit
//! specialist's 19/30 — the quality was there the whole time; only the
//! plumbing said no.
//!
//! Each lane is therefore `Option`al and **present if and only if the
//! slot can serve it**. Ask the lane, never re-derive capability from a
//! marker enum or a model name: `/v1/completions` 503s exactly when
//! [`EditSlotInfo::fim`] is `None`, and that is the only place the
//! question is answered (ARCH §10.6).

use serde::{Deserialize, Serialize};

/// Which model family's FIM marker convention a slot speaks. Detected
/// by vocab probe at slot install (`sovereign_inference::fim::detect_fim_style`),
/// NOT keyed off `ModelFamily` (which is `Unknown` on all production
/// slots — plan correction #5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FimStyle {
    /// Qwen2.5-Coder family: `<|fim_prefix|>` / `<|fim_suffix|>` /
    /// `<|fim_middle|>`, PSM ordering.
    QwenCoder,
    /// JetBrains Mellum family (Mellum2-12B-A2.5B, Mellum-4b):
    /// `<fim_prefix>` / `<fim_suffix>` / `<fim_middle>` — the SAME
    /// spellings as StarCoder2, disambiguated by an additional
    /// vocab token (`<|im_start|>`) in the probe.
    Mellum,
    /// StarCoder2 family: `<fim_prefix>` / `<fim_suffix>` / `<fim_middle>`.
    StarCoder2,
    /// ByteDance Seed-Coder family (Zeta 2.x is a Seed-Coder-8B
    /// fine-tune): `<[fim-prefix]>` / `<[fim-suffix]>` / `<[fim-middle]>`,
    /// SPM ordering in Zeta's next-edit prompt.
    SeedCoder,
}

impl FimStyle {
    /// Stable wire/debug string.
    pub const fn as_str(self) -> &'static str {
        match self {
            FimStyle::QwenCoder => "qwen_coder",
            FimStyle::Mellum => "mellum",
            FimStyle::StarCoder2 => "starcoder2",
            FimStyle::SeedCoder => "seed_coder",
        }
    }
}

/// Which prompt/parse contract the next-edit lane speaks to this
/// slot's model (`NEXT_EDIT.md` §Bakeoff). Explicit config
/// (`[models.edit].next_edit_format`), never sniffed from the model id
/// — a wrong guess would silently feed a model a register it was
/// never trained on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextEditFormat {
    /// Marker-bracketed region + instruct prose over the chat
    /// template. The default, and the only dialect that works on an
    /// untrained chat model — which makes it the degraded lane's
    /// format as well as Mellum2-Instruct's.
    #[default]
    RegionInstruct,
    /// Zeta 2.x raw SPM prompt: `<[fim-suffix]>` / `<[fim-prefix]>` /
    /// numbered `<|marker_N|>` editable region / `<[fim-middle]>`.
    Zeta2,
    /// Sweep next-edit raw prompt: `<|file_sep|>` sections with
    /// `.diff` original/updated blocks; completes `updated/{path}`.
    Sweep,
}

impl NextEditFormat {
    /// Stable wire/debug string.
    pub const fn as_str(self) -> &'static str {
        match self {
            NextEditFormat::RegionInstruct => "region_instruct",
            NextEditFormat::Zeta2 => "zeta2",
            NextEditFormat::Sweep => "sweep",
        }
    }

    /// True when this dialect is rendered through the model's **chat
    /// template** rather than as a raw completion prompt.
    ///
    /// The distinction is load-bearing, not cosmetic: chat-template
    /// dialects run on thinking-capable general models, where an
    /// unsuppressed reasoning block consumes the entire generation
    /// budget before the first answer byte (measured 2026-08-07: ~1044
    /// tokens of `reasoning_content` against a 64–1024 token lane
    /// grant, i.e. every case truncated). Raw dialects ride
    /// purpose-built completion fine-tunes that have no thinking phase
    /// to suppress. See `ConsultPlan::suppress_thinking`.
    pub const fn uses_chat_template(self) -> bool {
        match self {
            NextEditFormat::RegionInstruct => true,
            NextEditFormat::Zeta2 | NextEditFormat::Sweep => false,
        }
    }
}

/// The fill-in-the-middle lane — everything `POST /v1/completions`
/// needs to serve a request. Present only when the slot's model
/// carries FIM markers in its vocab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FimLane {
    /// Detected marker convention. Anything building a FIM prompt
    /// reads it from here, so there is exactly one answer to "which
    /// markers does this model speak" — a guessed convention produces
    /// confident garbage rather than an error.
    pub style: FimStyle,
    /// Generation cap per completion.
    pub max_tokens: usize,
    /// Sampling temperature.
    pub temperature: f32,
    /// Server keeps the TAIL of the client prefix beyond this many chars.
    pub max_prefix_chars: usize,
    /// Server keeps the HEAD of the client suffix beyond this many chars.
    pub max_suffix_chars: usize,
}

/// The next-edit-suggestion lane — everything `POST /v1/edit_predictions`
/// needs from the *slot*. Present whenever the slot has a model at all,
/// since NES requires no special vocabulary.
///
/// Deliberately thin. Sampling for this lane (`max_tokens`, `stop`,
/// `temperature`, thinking suppression) is **per-consult policy** and
/// lives on `commonwealth_api::next_edit_model::ConsultPlan`, which
/// both the daemon and the offline scorer read — duplicating it here
/// would let the two drift and silently measure different models
/// (ARCH §10.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextEditLane {
    /// Prompt/parse dialect this slot's model speaks.
    pub format: NextEditFormat,
}

/// Live description of the daemon's code-editing serving arrangement.
///
/// `None` from `InferenceProvider::edit_slot_info()` means no editing
/// model is available at all. A *present* value with a `None` lane
/// means "this model exists but cannot serve that lane" — which is a
/// supported, reportable state, not a failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditSlotInfo {
    /// Slot serving: `"edit"` for a dedicated pinned extra, or the
    /// fast slot's name when the editing model aliases the fast model
    /// (lean mode, decision D8).
    pub slot: String,
    /// Model id requests should carry to route to this slot (the
    /// advertised `/v1/models` name — gguf file stem).
    pub model_id: String,
    /// True when serving from the shared fast slot (alias mode);
    /// false when a dedicated pinned extra slot was loaded.
    pub aliased_to_fast: bool,
    /// True when this slot is the automatic fallback rather than an
    /// operator-chosen edit model — i.e. next-edit is being served by
    /// whatever chat weights happened to be resident.
    ///
    /// Drives the nudge on `/status.inference.edit`: the user gets
    /// working suggestions, and is told a specialist would be faster.
    /// This is about *provenance* (did anyone choose this model for
    /// the job), distinct from a `None` lane, which is about
    /// *capability*.
    #[serde(default)]
    pub degraded: bool,
    /// The next-edit-suggestion lane, when this slot can serve it.
    #[serde(default)]
    pub next_edit: Option<NextEditLane>,
    /// The fill-in-the-middle lane, when this slot's model carries FIM
    /// markers. `None` is the ordinary case for a general chat model.
    #[serde(default)]
    pub fim: Option<FimLane>,
}
