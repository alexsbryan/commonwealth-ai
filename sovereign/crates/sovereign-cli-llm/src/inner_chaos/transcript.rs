// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared transcript representation for the inner-work chaos
//! harness. Both the brain (generating the next user turn) and the
//! judge (auditing a witness reply in context) render the running
//! thread the same way, so the two prompts can never drift apart on
//! speaker labels.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Witness,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::User => "USER",
            Role::Witness => "WITNESS",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptTurn {
    pub role: Role,
    pub text: String,
}

impl TranscriptTurn {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
        }
    }

    pub fn witness(text: impl Into<String>) -> Self {
        Self {
            role: Role::Witness,
            text: text.into(),
        }
    }
}

/// Render a transcript as `LABEL: text` lines. Empty transcript
/// renders as an explicit marker so prompts never contain a silent
/// blank section.
pub fn render(turns: &[TranscriptTurn]) -> String {
    if turns.is_empty() {
        return "(none yet)".to_string();
    }
    turns
        .iter()
        .map(|t| format!("{}: {}", t.role.label(), t.text))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_labels_speakers() {
        let t = vec![
            TranscriptTurn::user("hello"),
            TranscriptTurn::witness("hi there"),
        ];
        assert_eq!(render(&t), "USER: hello\nWITNESS: hi there");
    }

    #[test]
    fn render_empty_is_explicit() {
        assert_eq!(render(&[]), "(none yet)");
    }
}
