// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fill-in-the-middle (FIM) inline-completion contract types
//! (`sovereign/docs/INLINE_COMPLETION.md`). These cross the
//! `InferenceProvider` seam so the daemon's HTTP layer can report FIM
//! slot state (`/status.inference.fim`) without knowing engine internals.

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

/// Which prompt/parse contract the next-edit model lane speaks to the
/// FIM slot's model (`NEXT_EDIT.md` §Bakeoff). Explicit config
/// (`[models.fim].next_edit_format`), never sniffed from the model id
/// — a wrong guess would silently feed a model a register it was
/// never trained on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextEditFormat {
    /// Marker-bracketed region + instruct prose over the chat
    /// template (the Mellum2-Instruct v1 lane).
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
}

/// Live description of the daemon's FIM serving arrangement. `None`
/// from `InferenceProvider::fim_slot_info()` means "no FIM configured"
/// (or the configured model failed the marker probe) — the HTTP route
/// maps that to 503 with an actionable message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FimSlotInfo {
    /// Slot name serving FIM: `"fim"` for a dedicated extra, or the
    /// fast slot's name when `[models.fim].path` aliases the fast
    /// model (lean mode, decision D8).
    pub slot: String,
    /// Model id requests should carry to route to this slot (the
    /// advertised `/v1/models` name — gguf file stem).
    pub model_id: String,
    /// Detected marker convention.
    pub fim_style: FimStyle,
    /// Sampling defaults from `[models.fim]` (or built-in defaults).
    pub max_tokens: usize,
    /// Sampling temperature.
    pub temperature: f32,
    /// Server keeps the TAIL of the client prefix beyond this many chars.
    pub max_prefix_chars: usize,
    /// Server keeps the HEAD of the client suffix beyond this many chars.
    pub max_suffix_chars: usize,
    /// True when serving from the shared fast slot (alias mode);
    /// false when a dedicated pinned extra slot was loaded.
    pub aliased_to_fast: bool,
    /// Next-edit prompt/parse contract for this slot's model
    /// (`[models.fim].next_edit_format`; default `region_instruct`).
    #[serde(default)]
    pub next_edit_format: NextEditFormat,
}
