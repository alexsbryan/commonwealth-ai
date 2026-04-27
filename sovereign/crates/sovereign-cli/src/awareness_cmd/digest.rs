//! `sovereign awareness digest` — render the Relational + Strategic
//! digest blocks the way they would appear in the system prompt.
//!
//! Calls the same pure formatters the production splice path uses
//! (`format_relational`, `format_strategic`) so what's printed
//! matches what the model sees on the next turn.
//!
//! `--context "<text>"` simulates a current-turn message so the
//! "mentioned in current conversation" boost is exercised. Default
//! is no current context (every entity scored without that boost).
//!
//! `--budget relational=N,strategic=M` overrides the default token
//! budgets. Default = the pinned values from `ViewKind`
//! (currently 150 / 100).
//!
//! Phase 2 ships the rendered output without per-entity score
//! breakdowns — `--show-scores` and `--show-rejected` are deferred to
//! a follow-up that splits the formatters' internals into a
//! `RankingDetail`-emitting variant. For now the existing pure
//! formatters are reused as-is.

use std::sync::Arc;

use sovereign_tools::knowledge_view::relational::{format_relational, RelationalNote};
use sovereign_tools::knowledge_view::splice_extension::{
    load_chunk_timestamps, relational_notes_for_entity, strategic_goals_for_entity,
    AtosSnapshot, ConversationCorpus,
};
use sovereign_tools::knowledge_view::strategic::{format_strategic, StrategicGoal};
use sovereign_tools::knowledge_view::timeline::{
    assemble_timelines_from_atlas, InteractionTimeline,
};
use sovereign_tools::knowledge_view::view_kind::ViewKind;

use super::args::{get_flag, has_flag, split_args};
use super::render::display_path;
use super::store_open::{
    notes_db_path, project_toml_path, sovereign_root, state_db_path, try_open_features,
    try_open_notes,
};

const RELATIONAL_VIEWS: &[&str] = &["personal-knowledge", "conversation-history"];

pub(super) async fn cmd_digest(args: &[String]) -> i32 {
    let (_pos, flags) = split_args(args);

    let context = get_flag(&flags, "context").unwrap_or_default();
    let (rel_budget, strat_budget) = parse_budgets(&flags);
    let show_scores = has_flag(&flags, "show-scores");
    let show_rejected = has_flag(&flags, "show-rejected");
    if show_scores || show_rejected {
        eprintln!(
            "awareness digest: --show-scores / --show-rejected are stubbed for Phase 2 \
             (rendered output reflects the production formatter; per-entity score \
             breakdown lands in the follow-up `format_*_with_scores` split)."
        );
    }

    let root = sovereign_root(&flags);
    let db_path = state_db_path(&root);
    if !db_path.exists() {
        eprintln!(
            "awareness digest: no state db at {} (run `awareness seed` then `awareness extract` first)",
            display_path(&db_path)
        );
        return 1;
    }

    let chunk_ts = load_chunk_timestamps(&db_path);
    let resolver = move |id: &str| -> Option<i64> { chunk_ts.get(id).copied() };

    let toml_path = project_toml_path();
    let toml_path_opt = if toml_path.exists() {
        Some(toml_path.as_path())
    } else {
        None
    };
    let features = try_open_features();
    let atos = AtosSnapshot::build(features.as_ref(), toml_path_opt).await;

    // Walk both atlas dirs.
    let mut all_timelines: Vec<InteractionTimeline> = Vec::new();
    let mut atlases_seen = 0usize;
    for view_id in RELATIONAL_VIEWS {
        let corpus_dir = root.join("indexes").join(view_id);
        let atlas_dir = corpus_dir.join("atlas");
        if !atlas_dir.exists() {
            continue;
        }
        atlases_seen += 1;
        match assemble_timelines_from_atlas(&corpus_dir, &resolver, &atos) {
            Ok(mut t) => all_timelines.append(&mut t),
            Err(e) => {
                eprintln!(
                    "awareness digest: failed to assemble {}: {e}",
                    display_path(&corpus_dir)
                );
                return 1;
            }
        }
    }

    if all_timelines.is_empty() {
        if atlases_seen == 0 {
            eprintln!(
                "awareness digest: no atlases found at {}/indexes/* — \
                 run `awareness extract` after seeding.",
                display_path(&root)
            );
        } else {
            eprintln!("awareness digest: atlases present but no relational entities yet.");
        }
        return 0;
    }

    // Notes lookups via NoteStore.
    let notes_path = notes_db_path();
    let notes_arc = if notes_path.exists() {
        match sovereign_tools::knowledge_view::splice_extension::AtosSnapshot::empty() {
            // Tiny dummy to remind the reader: AtosSnapshot construction
            // is async; if you change this block, async-await it.
            _ => {}
        }
        try_open_notes()
    } else {
        None
    };

    // Pre-compute the per-entity note set so the formatter can stay
    // sync. Builds two indexes: relational (commitments/follow-ups/
    // goals) keyed by entity name, and strategic (goals only).
    let mut rel_index: std::collections::HashMap<String, Vec<RelationalNote>> =
        std::collections::HashMap::new();
    let mut strat_index: std::collections::HashMap<String, Vec<StrategicGoal>> =
        std::collections::HashMap::new();
    if let Some(notes) = notes_arc {
        let names: Vec<String> = all_timelines
            .iter()
            .map(|t| t.entity_name.clone())
            .collect();
        for name in &names {
            let rel = relational_notes_for_entity(&notes, name).await;
            if !rel.is_empty() {
                rel_index.insert(name.clone(), rel);
            }
            let goals = strategic_goals_for_entity(&notes, name).await;
            if !goals.is_empty() {
                strat_index.insert(name.clone(), goals);
            }
        }
    }
    let rel_index = Arc::new(rel_index);
    let strat_index = Arc::new(strat_index);

    let rel_index_for_closure = Arc::clone(&rel_index);
    let rel_lookup = move |entity: &str| -> Vec<RelationalNote> {
        rel_index_for_closure.get(entity).cloned().unwrap_or_default()
    };
    let strat_index_for_closure = Arc::clone(&strat_index);
    let strat_lookup = move |entity: &str| -> Vec<StrategicGoal> {
        strat_index_for_closure
            .get(entity)
            .cloned()
            .unwrap_or_default()
    };

    let corpus = ConversationCorpus::from_messages(if context.is_empty() {
        Vec::new()
    } else {
        vec![context.clone()]
    });
    let in_conv = move |entity: &str| -> bool { corpus.contains_entity(entity) };

    let now = unix_now();

    // Render relational.
    let (rel_block, rel_count) = format_relational(
        &all_timelines,
        &rel_lookup,
        &in_conv,
        now,
        rel_budget,
    );
    let (strat_block, strat_count) = format_strategic(
        &all_timelines,
        &strat_lookup,
        &in_conv,
        now,
        strat_budget,
    );

    if rel_block.is_empty() && strat_block.is_empty() {
        println!("(no digest output — all timelines empty or below budget)");
        return 0;
    }

    if !rel_block.is_empty() {
        println!("═══ Relational Digest ({} {}, budget {}) ═══", rel_count,
            if rel_count == 1 { "entry" } else { "entries" },
            rel_budget);
        println!();
        println!("{rel_block}");
    }
    if !strat_block.is_empty() {
        println!();
        println!("═══ Strategic Digest ({} {}, budget {}) ═══", strat_count,
            if strat_count == 1 { "entry" } else { "entries" },
            strat_budget);
        println!();
        println!("{strat_block}");
    }
    if !context.is_empty() {
        println!();
        println!("Current-turn context applied: \"{context}\"");
    }
    0
}

fn parse_budgets(flags: &[(String, String)]) -> (usize, usize) {
    let default_rel = ViewKind::Relational.default_budget_tokens();
    let default_strat = ViewKind::Strategic.default_budget_tokens();
    let mut rel = default_rel;
    let mut strat = default_strat;
    if let Some(b) = get_flag(flags, "budget").filter(|s| !s.is_empty()) {
        for pair in b.split(',') {
            let mut iter = pair.splitn(2, '=');
            match (iter.next(), iter.next()) {
                (Some("relational"), Some(v)) => {
                    if let Ok(n) = v.parse() {
                        rel = n;
                    }
                }
                (Some("strategic"), Some(v)) => {
                    if let Ok(n) = v.parse() {
                        strat = n;
                    }
                }
                _ => {}
            }
        }
    }
    (rel, strat)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(k: &str, v: &str) -> (String, String) {
        (k.into(), v.into())
    }

    #[test]
    fn parse_budgets_defaults_when_unset() {
        let (rel, strat) = parse_budgets(&Vec::new());
        // ViewKind defaults — assert they're > 0.
        assert!(rel > 0);
        assert!(strat > 0);
    }

    #[test]
    fn parse_budgets_overrides_relational_only() {
        let flags = vec![flag("budget", "relational=200")];
        let (rel, strat) = parse_budgets(&flags);
        assert_eq!(rel, 200);
        assert!(strat > 0); // unchanged default
    }

    #[test]
    fn parse_budgets_overrides_both() {
        let flags = vec![flag("budget", "relational=200,strategic=50")];
        let (rel, strat) = parse_budgets(&flags);
        assert_eq!(rel, 200);
        assert_eq!(strat, 50);
    }

    #[test]
    fn parse_budgets_ignores_unknown_keys() {
        let flags = vec![flag("budget", "unknown=99,relational=200")];
        let (rel, _strat) = parse_budgets(&flags);
        assert_eq!(rel, 200);
    }
}
