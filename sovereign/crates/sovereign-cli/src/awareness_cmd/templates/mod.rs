//! Built-in scenario templates for `sovereign awareness seed
//! --from-template <name>`.
//!
//! Each template ships as a TOML file checked in alongside this
//! module; `load_template(name)` parses one of them and returns a
//! [`Template`] the seed loader writes into the StateStore.
//!
//! Why TOML, not Rust constants: per ARCH §6 ("data ≠ program"),
//! templates and golden sets are data. TOML files are easy to read,
//! diff, and edit without recompiling. They also serve as a public
//! reference for what the developer can expect when running
//! `awareness scenario` against the same template (Phase 4).
//!
//! Schema (one TOML per template):
//!
//! ```toml
//! [meta]
//! name = "consulting"
//! description = "Solo consultant managing client relationships"
//! days = 30
//!
//! [[expected_entities]]
//! name = "Sarah Chen"
//! kind = "person"
//! affiliation = "Acme Corp"
//! role = "VP Engineering"
//!
//! [[conversations]]
//! id = "c1"
//! day_offset = -29   # 29 days before today
//! skill = "general"
//! [[conversations.messages]]
//! role = "user"
//! content = "Had a call with Sarah Chen at Acme today..."
//! [[conversations.messages]]
//! role = "assistant"
//! content = "That's useful — what did she think about pricing?"
//!
//! [[expected_suggestions]]
//! conversation_id = "c1"
//! turn = 3
//! kind = "commitment"
//! content_contains = "send pricing"
//! related_entity = "Sarah Chen"
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Template {
    pub meta: TemplateMeta,
    #[serde(default)]
    pub expected_entities: Vec<ExpectedEntity>,
    #[serde(default)]
    pub conversations: Vec<TemplateConversation>,
    #[serde(default)]
    pub expected_suggestions: Vec<ExpectedSuggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TemplateMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_days")]
    pub days: u32,
}

fn default_days() -> u32 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ExpectedEntity {
    pub name: String,
    pub kind: String, // "person" | "organization" | "initiative"
    #[serde(default)]
    pub affiliation: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub participants: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TemplateConversation {
    pub id: String,
    /// Offset (in days) from today. Negative for past, zero for
    /// today. The seed loader resolves this to an absolute Unix
    /// timestamp at run time.
    #[serde(default)]
    pub day_offset: i32,
    #[serde(default)]
    pub skill: Option<String>,
    pub messages: Vec<TemplateMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TemplateMessage {
    pub role: String, // "user" | "assistant" | "system"
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ExpectedSuggestion {
    pub conversation_id: String,
    pub turn: u32,
    pub kind: String, // "commitment" | "follow_up" | "goal"
    #[serde(default)]
    pub content_contains: Option<String>,
    #[serde(default)]
    pub related_entity: Option<String>,
}

/// Raw bodies of the bundled templates. Adding a new template means
/// (a) committing `<name>.toml` next to this module and (b) appending
/// a row here. `load_template(name)` parses on demand.
const BUILTINS: &[(&str, &str)] = &[
    ("consulting", include_str!("consulting.toml")),
    ("startup", include_str!("startup.toml")),
    ("team-lead", include_str!("team-lead.toml")),
    ("chaos-month", include_str!("chaos-month.toml")),
];

pub(super) fn list_builtin_names() -> Vec<&'static str> {
    BUILTINS.iter().map(|(n, _)| *n).collect()
}

pub(super) fn load_builtin(name: &str) -> Result<Template, String> {
    let body = BUILTINS
        .iter()
        .find_map(|(n, body)| if *n == name { Some(*body) } else { None })
        .ok_or_else(|| {
            format!(
                "no built-in template '{name}' (available: {})",
                list_builtin_names().join(", ")
            )
        })?;
    parse_template(body).map_err(|e| format!("parse {name}.toml: {e}"))
}

pub(super) fn load_from_path(path: &std::path::Path) -> Result<Template, String> {
    let body = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_template(&body).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn parse_template(body: &str) -> Result<Template, toml::de::Error> {
    toml::from_str::<Template>(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consulting_template_parses() {
        let t = load_builtin("consulting").expect("consulting.toml should parse");
        assert_eq!(t.meta.name, "consulting");
        assert!(t.meta.days >= 7, "days field should be set");
        assert!(
            !t.conversations.is_empty(),
            "consulting template must contain at least one conversation"
        );
        assert!(
            !t.expected_entities.is_empty(),
            "consulting template must declare expected entities"
        );
    }

    #[test]
    fn startup_template_parses() {
        load_builtin("startup").expect("startup.toml should parse");
    }

    #[test]
    fn team_lead_template_parses() {
        load_builtin("team-lead").expect("team-lead.toml should parse");
    }

    #[test]
    fn chaos_month_template_parses() {
        let t = load_builtin("chaos-month").expect("chaos-month.toml should parse");
        assert_eq!(t.meta.name, "chaos-month");
        assert!(
            t.conversations.len() >= 15,
            "chaos-month should have a meaningful number of noisy conversations"
        );
        assert!(
            !t.expected_entities.is_empty(),
            "chaos-month must declare its clean ground-truth entity set"
        );
    }

    #[test]
    fn unknown_template_returns_error_with_available_list() {
        let err = load_builtin("nonexistent").unwrap_err();
        assert!(err.contains("consulting"));
    }

    #[test]
    fn message_roles_are_well_formed() {
        let t = load_builtin("consulting").unwrap();
        for c in &t.conversations {
            for m in &c.messages {
                assert!(
                    matches!(m.role.as_str(), "user" | "assistant" | "system"),
                    "unexpected role '{}' in conversation {}",
                    m.role,
                    c.id
                );
            }
        }
    }

    #[test]
    fn expected_entity_kinds_are_well_formed() {
        let t = load_builtin("consulting").unwrap();
        for e in &t.expected_entities {
            assert!(
                matches!(e.kind.as_str(), "person" | "organization" | "initiative"),
                "unexpected entity kind '{}' for '{}'",
                e.kind,
                e.name
            );
        }
    }
}
