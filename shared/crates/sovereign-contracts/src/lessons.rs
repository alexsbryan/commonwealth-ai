// SPDX-License-Identifier: AGPL-3.0-or-later
//! The lesson object — the note payload a taught lesson is stored as, and
//! the enforcement rung it compiles to. The compiler and the active-set
//! lane stay in `sovereign_core::lessons`; only the wire/stored shape is
//! here, because the desktop's save command names it (via
//! `TurnEvent::LessonProposed` → [`LessonPayload::from_proposed`]).

use serde::{Deserialize, Serialize};

use crate::types::LessonProposedPayload;

/// Note kind for the lesson lane (`corpus-engine-notes` MIGRATION_V11).
pub const LESSON_KIND: &str = "lesson";

/// Version stamp on the DERIVED fields (`prompt_form`, `enforcement`,
/// `params`). Bump when the compile ladder changes shape; a recompile
/// pass (P1) re-derives every lesson whose stamp is older.
pub const COMPILER_VERSION: u32 = 1;

// ─── The lesson object ───────────────────────────────────────────────

/// Enforcement rung (TEACHABLE §7), cheapest first. P0 ships three;
/// `retrieval`/config is P1, `conditioning` (cartridge/adapter) is the
/// declared rung 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonEnforcement {
    Param,
    Transform,
    Prompt,
}

impl LessonEnforcement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Param => "param",
            Self::Transform => "transform",
            Self::Prompt => "prompt",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "param" => Some(Self::Param),
            "transform" => Some(Self::Transform),
            "prompt" => Some(Self::Prompt),
            _ => None,
        }
    }
}

/// Provenance: the verbatim coaching moment a lesson was taught from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaughtFrom {
    pub excerpt: String,
    pub conversation_id: String,
    /// Prior assistant message the coaching referred to ("" when none).
    pub message_id: String,
}

/// The `payload_json` schema for `kind = "lesson"` notes — the single
/// source of truth both the runtime loader and the desktop commands
/// deserialize.
///
/// Source of record: `display`, `taught_from`, lifecycle fields.
/// Derived (re-derivable, stamped `compiler_version`): `prompt_form`,
/// `enforcement`, `params`. `drafted_display` is Some only when the
/// user edited the draft on the card before saving — the consented
/// correction pair (TEACHABLE §11).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonPayload {
    pub display: String,
    #[serde(default)]
    pub prompt_form: String,
    pub enforcement: LessonEnforcement,
    #[serde(default)]
    pub params: serde_json::Value,
    /// Activation scopes. Empty = global (all P0 lessons).
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub taught_from: TaughtFrom,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Unix seconds at save time.
    #[serde(default)]
    pub created: i64,
    /// Unix seconds when the lesson first influenced an answer — the
    /// whisper-once marker. `None` until first application.
    #[serde(default)]
    pub first_applied_at: Option<i64>,
    #[serde(default)]
    pub last_affirmed: Option<i64>,
    #[serde(default = "default_compiler_version")]
    pub compiler_version: u32,
    /// The pre-edit draft sentence, kept only when the user edited the
    /// card before saving.
    #[serde(default)]
    pub drafted_display: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_compiler_version() -> u32 {
    1
}

impl LessonPayload {
    /// Build the note payload from a proposal the user kept (the
    /// desktop save command's helper — no duplicate schema in the
    /// desktop crate). The caller sets `drafted_display` afterwards
    /// when the user edited the card.
    pub fn from_proposed(p: &LessonProposedPayload, now: i64) -> Self {
        Self {
            display: p.display.clone(),
            prompt_form: p.prompt_form.clone(),
            // Unknown strings compile to Prompt — the most visible rung
            // (its cost shows in settings) and the only one that can
            // carry an arbitrary rule.
            enforcement: LessonEnforcement::parse(&p.enforcement)
                .unwrap_or(LessonEnforcement::Prompt),
            params: p.params.clone(),
            scope: Vec::new(),
            taught_from: TaughtFrom {
                excerpt: p.taught_from.clone(),
                conversation_id: p.conversation_id.clone(),
                message_id: p.message_id.clone(),
            },
            enabled: true,
            created: now,
            first_applied_at: None,
            last_affirmed: None,
            compiler_version: COMPILER_VERSION,
            drafted_display: None,
        }
    }
}
