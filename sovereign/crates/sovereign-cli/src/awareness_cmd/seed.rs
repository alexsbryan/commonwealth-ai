// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign awareness seed` — write synthetic conversations + memories
//! into the StateStore.
//!
//! Two source modes:
//!
//!   - `--from-template <name>` loads a built-in template (`consulting`,
//!     `startup`, `team-lead`).
//!   - `--from-file <path>` loads a TOML file matching the same
//!     template schema.
//!
//! `--days N` overrides the template's day window. Day offsets in
//! the template are interpreted relative to that window's "today"
//! (the wall-clock day the seed runs).
//!
//! `--dry-run` reports what would be written and skips persistence.
//!
//! Backdating: messages are written via `ConversationStore::save_message`
//! (which carries explicit `created_at`). The conversation row's
//! `updated_at` is patched to the latest message's timestamp via a
//! direct SQL UPDATE so the splice path's chunk-timestamp resolver
//! picks up the seeded times. (`save_message` hardcodes
//! `updated_at = now()` for the conversation row.)

use std::sync::Arc;

use rusqlite::{params, Connection, OpenFlags};
use sovereign_core::traits::ConversationStore;
use sovereign_core::types::{Message, Role};
use sovereign_store::sqlite::SqliteStateStore;

use super::args::{get_flag, has_flag, split_args};
use super::render::display_path;
use super::store_open::{sovereign_root, state_db_path};
use super::templates::{list_builtin_names, load_builtin, load_from_path, Template};
#[cfg(test)]
use super::templates::{TemplateConversation, TemplateMessage};

pub(super) async fn cmd_seed(args: &[String]) -> i32 {
    let (_pos, flags) = split_args(args);

    let template = match resolve_template(&flags) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("awareness seed: {e}");
            return 2;
        }
    };

    let dry_run = has_flag(&flags, "dry-run");
    let now = unix_now();
    let plan = build_seed_plan(&template, now);

    print_plan(&template, &plan);

    if dry_run {
        println!("(--dry-run; no writes)");
        return 0;
    }

    // Open the state DB. Create the parent dir if missing —
    // awareness against a fresh sandbox path is a common workflow.
    let root = sovereign_root(&flags);
    if let Err(e) = std::fs::create_dir_all(&root) {
        eprintln!(
            "awareness seed: failed to create {}: {e}",
            display_path(&root)
        );
        return 1;
    }
    let db_path = state_db_path(&root);

    let store = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "awareness seed: open {} failed: {e}",
                display_path(&db_path)
            );
            return 1;
        }
    };

    let mut written: usize = 0;
    for c in &plan.conversations {
        for m in &c.messages {
            let msg = Message {
                id: m.id.clone(),
                conversation_id: c.id.clone(),
                role: m.role,
                content: m.content.clone(),
                created_at: m.created_at,
                metadata: None,
                version: 0,
            };
            if let Err(e) = store.save_message(&msg).await {
                eprintln!("awareness seed: save_message {}: {e}", m.id);
                return 1;
            }
            written += 1;
        }
    }

    // Patch up conversations.updated_at. save_message stamps it to
    // now() per upsert; we want it to reflect the latest message
    // timestamp so the splice-path chunk-timestamp resolver returns
    // the seeded date instead of the wall-clock time.
    if let Err(e) = patch_conversation_timestamps(&db_path, &plan.conversations) {
        eprintln!(
            "awareness seed: patch updated_at failed: {e}\n\
             (messages were written; conversations carry now() rather than the seeded date)"
        );
        return 1;
    }

    let conv_count = plan.conversations.len();
    println!();
    println!(
        "awareness seed: wrote {} message{} across {} conversation{} to {}",
        written,
        if written == 1 { "" } else { "s" },
        conv_count,
        if conv_count == 1 { "" } else { "s" },
        display_path(&db_path)
    );
    println!("Run `sovereign awareness extract` to populate the entity atlases.");
    0
}

fn resolve_template(flags: &[(String, String)]) -> Result<Template, String> {
    if let Some(name) = get_flag(flags, "from-template").filter(|s| !s.is_empty()) {
        return load_builtin(&name);
    }
    if let Some(p) = get_flag(flags, "from-file").filter(|s| !s.is_empty()) {
        return load_from_path(std::path::Path::new(&p));
    }
    Err(format!(
        "pass --from-template <name> or --from-file <path>\n\
         available built-in templates: {}",
        list_builtin_names().join(", ")
    ))
}

#[derive(Debug)]
struct SeedPlan {
    conversations: Vec<PlannedConversation>,
}

#[derive(Debug)]
struct PlannedConversation {
    id: String,
    skill: Option<String>,
    last_msg_at: i64,
    messages: Vec<PlannedMessage>,
}

#[derive(Debug)]
struct PlannedMessage {
    id: String,
    role: Role,
    content: String,
    created_at: i64,
}

fn build_seed_plan(t: &Template, now: i64) -> SeedPlan {
    let mut conversations: Vec<PlannedConversation> = Vec::new();
    for (ci, c) in t.conversations.iter().enumerate() {
        // Anchor each conversation to its day_offset and spread its
        // messages across a 30-minute window so they sort
        // deterministically without all sharing a timestamp.
        let day_anchor = now + i64::from(c.day_offset) * 86_400;
        let mut last_msg_at = day_anchor;
        let mut messages = Vec::with_capacity(c.messages.len());
        for (mi, m) in c.messages.iter().enumerate() {
            let role = parse_role(&m.role);
            let created_at = day_anchor + (mi as i64) * 60; // 60s apart
            last_msg_at = created_at;
            messages.push(PlannedMessage {
                id: format!("{c_id}-m{m_idx}", c_id = c.id, m_idx = mi),
                role,
                content: m.content.clone(),
                created_at,
            });
            let _ = ci;
        }
        conversations.push(PlannedConversation {
            id: c.id.clone(),
            skill: c.skill.clone(),
            last_msg_at,
            messages,
        });
    }
    SeedPlan { conversations }
}

fn parse_role(s: &str) -> Role {
    match s {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "system" => Role::System,
        // Anything else is a template-author bug — surface it loud
        // by defaulting to system, which the existing role display
        // path renders distinctly.
        _ => Role::System,
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn print_plan(t: &Template, plan: &SeedPlan) {
    let total_msgs: usize = plan.conversations.iter().map(|c| c.messages.len()).sum();
    let oldest = plan
        .conversations
        .iter()
        .flat_map(|c| c.messages.iter())
        .map(|m| m.created_at)
        .min()
        .unwrap_or(0);
    let newest = plan
        .conversations
        .iter()
        .flat_map(|c| c.messages.iter())
        .map(|m| m.created_at)
        .max()
        .unwrap_or(0);

    println!(
        "awareness seed — template '{}' ({})",
        t.meta.name, t.meta.description
    );
    println!(
        "  Will write {} conversation{}, {} message{}",
        plan.conversations.len(),
        if plan.conversations.len() == 1 {
            ""
        } else {
            "s"
        },
        total_msgs,
        if total_msgs == 1 { "" } else { "s" }
    );
    println!(
        "  Date range: {} → {}",
        super::render::format_date(Some(oldest)),
        super::render::format_date(Some(newest))
    );

    if !t.expected_entities.is_empty() {
        let person_n = t
            .expected_entities
            .iter()
            .filter(|e| e.kind == "person")
            .count();
        let org_n = t
            .expected_entities
            .iter()
            .filter(|e| e.kind == "organization")
            .count();
        let init_n = t
            .expected_entities
            .iter()
            .filter(|e| e.kind == "initiative")
            .count();
        println!();
        println!("Expected entities (declared in template):");
        println!(
            "  People:        {} — {}",
            person_n,
            t.expected_entities
                .iter()
                .filter(|e| e.kind == "person")
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!(
            "  Organizations: {} — {}",
            org_n,
            t.expected_entities
                .iter()
                .filter(|e| e.kind == "organization")
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!(
            "  Initiatives:   {} — {}",
            init_n,
            t.expected_entities
                .iter()
                .filter(|e| e.kind == "initiative")
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !t.expected_suggestions.is_empty() {
        let n_commit = t
            .expected_suggestions
            .iter()
            .filter(|s| s.kind == "commitment")
            .count();
        let n_follow = t
            .expected_suggestions
            .iter()
            .filter(|s| s.kind == "follow_up")
            .count();
        let n_goal = t
            .expected_suggestions
            .iter()
            .filter(|s| s.kind == "goal")
            .count();
        println!(
            "  Suggestions:   {} commitment{}, {} follow-up{}, {} goal{}",
            n_commit,
            if n_commit == 1 { "" } else { "s" },
            n_follow,
            if n_follow == 1 { "" } else { "s" },
            n_goal,
            if n_goal == 1 { "" } else { "s" }
        );
    }
}

fn patch_conversation_timestamps(
    db_path: &std::path::Path,
    conversations: &[PlannedConversation],
) -> rusqlite::Result<()> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut stmt = conn.prepare(
        "UPDATE conversations
         SET updated_at = ?1, created_at = ?1, skill_id = ?2
         WHERE id = ?3",
    )?;
    for c in conversations {
        stmt.execute(params![c.last_msg_at, c.skill, c.id])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_template() -> Template {
        Template {
            meta: super::super::templates::TemplateMeta {
                name: "fixture".into(),
                description: "test".into(),
                days: 30,
            },
            expected_entities: Vec::new(),
            expected_suggestions: Vec::new(),
            conversations: vec![
                TemplateConversation {
                    id: "c1".into(),
                    day_offset: -10,
                    skill: None,
                    messages: vec![
                        TemplateMessage {
                            role: "user".into(),
                            content: "Hi".into(),
                        },
                        TemplateMessage {
                            role: "assistant".into(),
                            content: "Hello".into(),
                        },
                    ],
                },
                TemplateConversation {
                    id: "c2".into(),
                    day_offset: 0,
                    skill: Some("inner-work".into()),
                    messages: vec![TemplateMessage {
                        role: "user".into(),
                        content: "Today".into(),
                    }],
                },
            ],
        }
    }

    #[test]
    fn build_seed_plan_anchors_conversations_to_day_offsets() {
        let now = 1_700_000_000_i64;
        let plan = build_seed_plan(&fixture_template(), now);
        assert_eq!(plan.conversations.len(), 2);

        let c1 = &plan.conversations[0];
        assert_eq!(c1.id, "c1");
        // Two messages, 60s apart, anchored 10 days back.
        assert_eq!(c1.messages.len(), 2);
        let ten_days_ago = now - 10 * 86_400;
        assert_eq!(c1.messages[0].created_at, ten_days_ago);
        assert_eq!(c1.messages[1].created_at, ten_days_ago + 60);
        assert_eq!(c1.last_msg_at, ten_days_ago + 60);

        let c2 = &plan.conversations[1];
        assert_eq!(c2.skill.as_deref(), Some("inner-work"));
        assert_eq!(c2.messages[0].created_at, now);
    }

    #[test]
    fn parse_role_handles_known_strings() {
        assert!(matches!(parse_role("user"), Role::User));
        assert!(matches!(parse_role("assistant"), Role::Assistant));
        assert!(matches!(parse_role("system"), Role::System));
        assert!(matches!(parse_role("invalid"), Role::System));
    }
}
