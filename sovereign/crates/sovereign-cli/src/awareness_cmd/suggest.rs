//! `sovereign awareness suggest <conversation-id>` — replay a
//! conversation turn-by-turn and report what the model would suggest
//! as commitment / follow_up / goal notes.
//!
//! Phase 3 ships a *focused-detection-prompt simulator* — for each
//! user turn we hand the model a tightly-scoped prompt asking
//! "did the user just commit to / express / promise something?"
//! and parse the JSON response. The fully-faithful production path
//! (model emits a `suggest_note` tool call routed through
//! ApprovalChannel) is reachable via real conversations; this CLI
//! gives the developer the same detection signal without OICP
//! tool-dispatch plumbing in the loop.
//!
//! The one-per-turn priority gate (`goal > commitment > follow_up`)
//! is enforced after detection: when multiple kinds fire we surface
//! the highest-priority one and mark the others suppressed.

use std::sync::Arc;

use sovereign_core::traits::ConversationStore;
use sovereign_core::types::{Conversation, Message, Role};
use sovereign_store::sqlite::SqliteStateStore;

use super::args::{has_flag, split_args};
use super::inference::resolve_inference;
use super::render::display_path;
use super::store_open::{sovereign_root, state_db_path};

pub(super) async fn cmd_suggest(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(conv_id) = positional.into_iter().next() else {
        eprintln!("awareness suggest: <conversation-id> is required");
        eprintln!("usage: sovereign awareness suggest <conversation-id> [--all-turns] [--verbose]");
        return 2;
    };
    let verbose = has_flag(&flags, "verbose");
    let _all_turns = has_flag(&flags, "all-turns"); // stub flag — every turn already evaluated

    let (inference, mode) = match resolve_inference(&flags).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("awareness suggest: {e}");
            return 1;
        }
    };
    println!("awareness suggest: inference mode = {:?}", mode);

    let root = sovereign_root(&flags);
    let db_path = state_db_path(&root);
    if !db_path.exists() {
        eprintln!(
            "awareness suggest: no state db at {} (seed first)",
            display_path(&db_path)
        );
        return 1;
    }
    let store = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "awareness suggest: open {} failed: {e}",
                display_path(&db_path)
            );
            return 1;
        }
    };
    let conversation = match store.get_conversation(&conv_id).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("awareness suggest: conversation '{conv_id}': {e}");
            return 1;
        }
    };

    print_header(&conversation);

    let mut grand_detected = 0usize;
    let mut grand_suppressed = 0usize;

    for (idx, msg) in conversation.messages.iter().enumerate() {
        let turn_no = idx + 1;
        if msg.role != Role::User {
            print_assistant_or_system_turn(turn_no, msg);
            continue;
        }
        let prompt = build_detection_prompt(&conversation, idx);
        if verbose {
            eprintln!("─── awareness suggest --verbose: turn {turn_no} prompt ───");
            eprintln!("{prompt}");
            eprintln!("──────────────────────────────────────────────────────");
        }
        let raw = match (inference)(&prompt).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("awareness suggest: inference failed on turn {turn_no}: {e}");
                continue;
            }
        };
        if verbose {
            eprintln!("turn {turn_no} response: {raw}");
        }
        let detections = parse_detections(&raw);
        let (chosen, suppressed) = apply_priority_gate(detections);

        print_user_turn(turn_no, msg);
        if let Some(d) = chosen.as_ref() {
            grand_detected += 1;
            print_suggested(d);
        } else {
            println!("    (no suggestion fired)");
        }
        for s in &suppressed {
            grand_suppressed += 1;
            print_suppressed(s);
        }
    }

    println!();
    println!(
        "Summary: {} suggestion{} ({} suppressed by priority gate)",
        grand_detected,
        if grand_detected == 1 { "" } else { "s" },
        grand_suppressed
    );
    0
}

fn print_header(c: &Conversation) {
    println!(
        "Conversation {} ({} message{}{}):",
        c.id,
        c.messages.len(),
        if c.messages.len() == 1 { "" } else { "s" },
        c.skill_id
            .as_deref()
            .map(|s| format!(", skill: {s}"))
            .unwrap_or_default()
    );
}

fn print_user_turn(n: usize, m: &Message) {
    println!();
    println!("Turn {n} (user): \"{}\"", truncate_inline(&m.content, 200));
}

fn print_assistant_or_system_turn(n: usize, m: &Message) {
    let role = match m.role {
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::User => "user",
    };
    println!();
    println!(
        "Turn {n} ({role}): \"{}\"",
        truncate_inline(&m.content, 200)
    );
    println!("    (skipped — only user turns trigger suggestion detection)");
}

fn print_suggested(d: &Detection) {
    println!(
        "    ✓ SUGGESTED: {} — \"{}\"",
        d.kind,
        truncate_inline(&d.content, 100)
    );
    if let Some(re) = &d.related_entity {
        println!("        Related entity: {re}");
    }
    if let Some(r) = &d.reasoning {
        println!("        Detection signal: {}", truncate_inline(r, 120));
    }
}

fn print_suppressed(d: &Detection) {
    println!(
        "    ✗ SUPPRESSED: {} — \"{}\" (priority gate)",
        d.kind,
        truncate_inline(&d.content, 100)
    );
}

fn truncate_inline(s: &str, max: usize) -> String {
    let line = s.replace('\n', " ");
    if line.chars().count() > max {
        let head: String = line.chars().take(max).collect();
        format!("{}…", head.trim_end())
    } else {
        line
    }
}

#[derive(Debug, Clone)]
struct Detection {
    kind: SuggestionKind,
    content: String,
    related_entity: Option<String>,
    reasoning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuggestionKind {
    Goal,
    Commitment,
    FollowUp,
}

impl SuggestionKind {
    fn priority(self) -> u8 {
        // Higher wins.
        match self {
            SuggestionKind::Goal => 3,
            SuggestionKind::Commitment => 2,
            SuggestionKind::FollowUp => 1,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "goal" => Some(Self::Goal),
            "commitment" => Some(Self::Commitment),
            "follow_up" | "follow-up" | "followup" => Some(Self::FollowUp),
            _ => None,
        }
    }
}

impl std::fmt::Display for SuggestionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SuggestionKind::Goal => "goal",
            SuggestionKind::Commitment => "commitment",
            SuggestionKind::FollowUp => "follow_up",
        })
    }
}

/// Build the focused detection prompt. Includes a small recency
/// window of conversation context so the model can resolve
/// `related_entity` from earlier turns ("Sarah" → "Sarah Chen" if
/// the full name was used in turn 1).
fn build_detection_prompt(c: &Conversation, turn_idx: usize) -> String {
    let mut window = String::new();
    let start = turn_idx.saturating_sub(3);
    for (i, m) in c
        .messages
        .iter()
        .enumerate()
        .skip(start)
        .take(turn_idx + 1 - start)
    {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        };
        let marker = if i == turn_idx {
            " ← current turn"
        } else {
            ""
        };
        window.push_str(&format!(
            "[Turn {}, {}{}]\n{}\n\n",
            i + 1,
            role,
            marker,
            m.content
        ));
    }
    format!(
        r#"You are a development-time evaluator for the Sovereign suggest_note pipeline.

Read the conversation excerpt below. For the CURRENT TURN only, decide:

  - **commitment**: did the user explicitly commit to an action with an external party, deadline, or deliverable? ("I'll send …", "I told her I'd …", "by Friday")
  - **follow_up**: did the user say they will revisit something later? ("let's check back on …", "remind me to …")
  - **goal**: did the user state a measurable objective? ("our goal is …", "by Q3 we want …")

Only flag *explicit* signals. Do NOT flag implicit or hedged language ("maybe", "thinking about", "should probably") unless the deadline or measurable criterion is concrete.

If multiple kinds fire on the same turn, list each independently — the development tool will apply a goal > commitment > follow_up priority gate.

Conversation excerpt:

{window}

Respond ONLY with JSON in this shape:
{{
  "detections": [
    {{
      "kind": "commitment|follow_up|goal",
      "content": "<one-line summary the developer can read into a note>",
      "related_entity": "<canonical entity name from the conversation, or null>",
      "reasoning": "<short justification — what wording triggered the detection>"
    }}
  ]
}}

If the current turn carries no eligible signal, return: {{"detections": []}}"#
    )
}

/// Parse the model's JSON response. Tolerates extra fences / preamble
/// by locating the first `{` and last `}` like the production tool
/// dispatcher does.
fn parse_detections(raw: &str) -> Vec<Detection> {
    let trimmed = trim_to_json_object(raw);
    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = match value.get("detections").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|v| {
            let kind = SuggestionKind::parse(v.get("kind")?.as_str()?)?;
            let content = v.get("content")?.as_str()?.trim().to_string();
            if content.is_empty() {
                return None;
            }
            let related_entity = v
                .get("related_entity")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && s != "null");
            let reasoning = v
                .get("reasoning")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            Some(Detection {
                kind,
                content,
                related_entity,
                reasoning,
            })
        })
        .collect()
}

fn trim_to_json_object(s: &str) -> &str {
    let start = match s.find('{') {
        Some(i) => i,
        None => return s,
    };
    let end = match s.rfind('}') {
        Some(i) => i,
        None => return s,
    };
    if end > start {
        &s[start..=end]
    } else {
        s
    }
}

/// Apply the goal > commitment > follow_up priority gate. Returns
/// (chosen, suppressed). Suppressed is empty when zero or one
/// detections fire.
fn apply_priority_gate(mut detections: Vec<Detection>) -> (Option<Detection>, Vec<Detection>) {
    if detections.is_empty() {
        return (None, Vec::new());
    }
    detections.sort_by(|a, b| b.kind.priority().cmp(&a.kind.priority()));
    let chosen = detections.remove(0);
    (Some(chosen), detections)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(kind: SuggestionKind, content: &str) -> Detection {
        Detection {
            kind,
            content: content.into(),
            related_entity: None,
            reasoning: None,
        }
    }

    #[test]
    fn priority_gate_picks_goal_over_commitment_over_follow_up() {
        let d = vec![
            det(SuggestionKind::FollowUp, "f"),
            det(SuggestionKind::Commitment, "c"),
            det(SuggestionKind::Goal, "g"),
        ];
        let (chosen, suppressed) = apply_priority_gate(d);
        assert_eq!(chosen.unwrap().kind, SuggestionKind::Goal);
        let kinds: Vec<SuggestionKind> = suppressed.iter().map(|d| d.kind).collect();
        assert!(kinds.contains(&SuggestionKind::Commitment));
        assert!(kinds.contains(&SuggestionKind::FollowUp));
    }

    #[test]
    fn priority_gate_returns_none_for_empty() {
        let (chosen, suppressed) = apply_priority_gate(Vec::new());
        assert!(chosen.is_none());
        assert!(suppressed.is_empty());
    }

    #[test]
    fn parse_detections_handles_well_formed_json() {
        let raw = r#"{
            "detections": [
                {"kind": "commitment", "content": "send pricing", "related_entity": "Sarah", "reasoning": "told her I'd"},
                {"kind": "goal", "content": "40% enterprise"}
            ]
        }"#;
        let d = parse_detections(raw);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].kind, SuggestionKind::Commitment);
        assert_eq!(d[0].related_entity.as_deref(), Some("Sarah"));
        assert_eq!(d[1].kind, SuggestionKind::Goal);
    }

    #[test]
    fn parse_detections_tolerates_fenced_response() {
        let raw = "```json\n{\"detections\": [{\"kind\":\"goal\",\"content\":\"x\"}]}\n```";
        let d = parse_detections(raw);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, SuggestionKind::Goal);
    }

    #[test]
    fn parse_detections_returns_empty_for_garbage() {
        assert!(parse_detections("not json at all").is_empty());
    }

    #[test]
    fn parse_detections_filters_null_related_entity() {
        let raw = r#"{"detections":[{"kind":"commitment","content":"x","related_entity":"null"}]}"#;
        let d = parse_detections(raw);
        assert_eq!(d[0].related_entity, None);
    }

    #[test]
    fn suggestion_kind_parse_accepts_variants() {
        assert_eq!(
            SuggestionKind::parse("follow-up"),
            Some(SuggestionKind::FollowUp)
        );
        assert_eq!(
            SuggestionKind::parse("FOLLOW_UP"),
            Some(SuggestionKind::FollowUp)
        );
        assert_eq!(SuggestionKind::parse("goal "), Some(SuggestionKind::Goal));
        assert_eq!(SuggestionKind::parse("unknown"), None);
    }
}
