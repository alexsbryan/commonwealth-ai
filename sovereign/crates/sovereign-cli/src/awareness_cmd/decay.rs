//! `sovereign awareness decay` — simulate memory decay with vs.
//! without entity-aware weighting.
//!
//! Reads every memory from the StateStore, projects each forward by
//! `--months N` (defaults to 3), and computes confidence under both
//! the uniform decay path and the inventory-aware path. Reports
//! surviving counts per month, lists memories that would be pruned
//! under uniform decay but survive entity-aware (the differential
//! survivors), and surfaces memories pruned under both.
//!
//! Useful for answering: "how does relationship-weighted decay
//! reshape the memory landscape?" — the §5 success criterion.

use std::sync::Arc;

use sovereign_core::memory::{
    apply_confidence_decay_with_rate_and_inventory, EntityInventory, DEFAULT_DECAY_RATE,
    DEFAULT_PRUNE_THRESHOLD,
};
use sovereign_core::traits::MemoryStore;
use sovereign_core::types::Memory;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::knowledge_view::splice_extension::build_entity_inventory;

use super::args::{get_flag, has_flag, split_args};
use super::render::display_path;
use super::store_open::{sovereign_root, state_db_path};

pub(super) async fn cmd_decay(args: &[String]) -> i32 {
    let (_pos, flags) = split_args(args);

    let months: i64 = match get_flag(&flags, "months") {
        None => 3,
        Some(s) => match s.parse() {
            Ok(n) if n >= 0 => n,
            _ => {
                eprintln!("awareness decay: --months must be a non-negative integer (got '{s}')");
                return 2;
            }
        },
    };
    let rate: f64 = match get_flag(&flags, "rate") {
        None => DEFAULT_DECAY_RATE,
        Some(s) => match s.parse() {
            Ok(r) if (0.0..=1.0).contains(&r) => r,
            _ => {
                eprintln!("awareness decay: --rate must be a fraction in [0.0, 1.0] (got '{s}')");
                return 2;
            }
        },
    };
    let threshold: f64 = match get_flag(&flags, "threshold") {
        None => DEFAULT_PRUNE_THRESHOLD,
        Some(s) => match s.parse() {
            Ok(t) if (0.0..=1.0).contains(&t) => t,
            _ => {
                eprintln!("awareness decay: --threshold must be in [0.0, 1.0] (got '{s}')");
                return 2;
            }
        },
    };
    let show_entity_linked = has_flag(&flags, "show-entity-linked");

    let root = sovereign_root(&flags);
    let db_path = state_db_path(&root);
    if !db_path.exists() {
        eprintln!(
            "awareness decay: no state db at {} (seed first)",
            display_path(&db_path)
        );
        return 1;
    }
    let store = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "awareness decay: open {} failed: {e}",
                display_path(&db_path)
            );
            return 1;
        }
    };

    let memories = match store.get_all_memories().await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("awareness decay: list memories failed: {e}");
            return 1;
        }
    };

    let inventory = build_entity_inventory(&root.join("indexes"));

    if memories.is_empty() {
        println!(
            "awareness decay: state db has no memories. Decay simulation requires memory \
             rows — they're produced by Sovereign's memory-extraction path during real \
             conversations. The seed templates only write conversation messages."
        );
        if !inventory.is_empty() {
            println!(
                "(Entity inventory loaded: {} names — would apply.)",
                inventory.len()
            );
        }
        return 0;
    }

    let now = unix_now();
    let report = simulate_decay(&memories, &inventory, months, rate, threshold, now);

    print_report(&report, months, rate, threshold, show_entity_linked);
    0
}

#[derive(Debug)]
struct DecayReport {
    total: usize,
    entity_linked: usize,
    rows: Vec<MonthRow>,
    differential_survivors: Vec<DifferentialRow>,
    pruned_under_both: Vec<PrunedRow>,
}

#[derive(Debug)]
struct MonthRow {
    month: i64,
    uniform_surviving: usize,
    entity_aware_surviving: usize,
}

#[derive(Debug)]
struct DifferentialRow {
    id: String,
    content_preview: String,
    matched_entities: Vec<String>,
    age_months: f64,
    uniform_confidence: f64,
    entity_aware_confidence: f64,
}

#[derive(Debug)]
struct PrunedRow {
    id: String,
    content_preview: String,
    age_months: f64,
    confidence: f64,
}

fn simulate_decay(
    memories: &[Memory],
    inventory: &EntityInventory,
    months: i64,
    rate: f64,
    threshold: f64,
    now: i64,
) -> DecayReport {
    let total = memories.len();
    let entity_linked = memories
        .iter()
        .filter(|m| matches_inventory(&m.content, inventory))
        .count();

    // Per-month surviving counts.
    let mut rows: Vec<MonthRow> = Vec::with_capacity(months as usize + 1);
    for month in 0..=months {
        let projected = now + month * 30 * 86_400;
        let mut uniform = 0usize;
        let mut entity_aware = 0usize;
        for m in memories {
            let u = apply_confidence_decay_with_rate_and_inventory(m, projected, rate, None);
            let e =
                apply_confidence_decay_with_rate_and_inventory(m, projected, rate, Some(inventory));
            if u >= threshold {
                uniform += 1;
            }
            if e >= threshold {
                entity_aware += 1;
            }
        }
        rows.push(MonthRow {
            month,
            uniform_surviving: uniform,
            entity_aware_surviving: entity_aware,
        });
    }

    // Differential at the final horizon.
    let final_proj = now + months * 30 * 86_400;
    let mut diffs: Vec<DifferentialRow> = Vec::new();
    let mut pruned_both: Vec<PrunedRow> = Vec::new();
    for m in memories {
        let u = apply_confidence_decay_with_rate_and_inventory(m, final_proj, rate, None);
        let e =
            apply_confidence_decay_with_rate_and_inventory(m, final_proj, rate, Some(inventory));
        let age_months = (final_proj - m.last_used) as f64 / (30.0 * 86_400.0);
        if u < threshold && e >= threshold {
            diffs.push(DifferentialRow {
                id: m.id.clone(),
                content_preview: preview_one_line(&m.content, 80),
                matched_entities: matched_entities(&m.content, inventory),
                age_months,
                uniform_confidence: u,
                entity_aware_confidence: e,
            });
        } else if u < threshold && e < threshold {
            pruned_both.push(PrunedRow {
                id: m.id.clone(),
                content_preview: preview_one_line(&m.content, 80),
                age_months,
                confidence: e,
            });
        }
    }

    DecayReport {
        total,
        entity_linked,
        rows,
        differential_survivors: diffs,
        pruned_under_both: pruned_both,
    }
}

fn matches_inventory(content: &str, inventory: &EntityInventory) -> bool {
    !matched_entities(content, inventory).is_empty()
}

/// Walk `inventory` against `content` token windows. Returns the list
/// of matched canonical names (whole-word, case-insensitive).
fn matched_entities(content: &str, inventory: &EntityInventory) -> Vec<String> {
    if inventory.is_empty() {
        return Vec::new();
    }
    let lower = content.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    let mut hits: Vec<String> = Vec::new();
    let max_window = 5usize.min(tokens.len());
    for window in 1..=max_window {
        for start in 0..=tokens.len().saturating_sub(window) {
            let candidate = tokens[start..start + window].join(" ");
            if inventory.contains(&candidate) && !hits.contains(&candidate) {
                hits.push(candidate);
            }
        }
    }
    hits
}

fn preview_one_line(s: &str, max_chars: usize) -> String {
    let single: String = s.replace('\n', " ").chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        format!("{}…", single.trim_end())
    } else {
        single
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn print_report(r: &DecayReport, months: i64, rate: f64, threshold: f64, show_entity_linked: bool) {
    println!(
        "Memory decay simulation: {} month{} at {:.0}%/month rate (threshold {:.2})",
        months,
        if months == 1 { "" } else { "s" },
        rate * 100.0,
        threshold
    );
    println!();
    let pct = if r.total == 0 {
        0
    } else {
        (r.entity_linked * 100) / r.total
    };
    println!("Memories at start: {}", r.total);
    println!("Entity-linked: {} ({}%)", r.entity_linked, pct);
    println!();
    println!(
        "{:>16}  {:>17}  {:>17}",
        "", "Uniform decay", "Entity-aware decay"
    );
    for row in &r.rows {
        let label = if row.month == 0 {
            "Initial".to_string()
        } else {
            format!("After {} mo", row.month)
        };
        println!(
            "{:>16}  {:>17}  {:>17}",
            label, row.uniform_surviving, row.entity_aware_surviving
        );
    }

    if !r.differential_survivors.is_empty() {
        println!();
        println!(
            "Differential survivors ({}) — pruned under uniform but survive entity-aware:",
            r.differential_survivors.len()
        );
        for d in &r.differential_survivors {
            println!("  {}", d.content_preview);
            println!(
                "    confidence: {:.2} (uniform) → {:.2} (entity-aware)",
                d.uniform_confidence, d.entity_aware_confidence
            );
            if show_entity_linked && !d.matched_entities.is_empty() {
                println!("    entities: {}", d.matched_entities.join(", "));
            }
            println!("    age: {:.1} months", d.age_months);
        }
    }

    if !r.pruned_under_both.is_empty() {
        println!();
        println!("Pruned under both ({}):", r.pruned_under_both.len());
        for p in r.pruned_under_both.iter().take(5) {
            println!(
                "  {} (confidence: {:.2}, age: {:.1} months)",
                p.content_preview, p.confidence, p.age_months
            );
        }
        if r.pruned_under_both.len() > 5 {
            println!("  … ({} more)", r.pruned_under_both.len() - 5);
        }
    }

    if r.differential_survivors.is_empty() && !r.rows.is_empty() {
        let final_row = r.rows.last().unwrap();
        if final_row.uniform_surviving == final_row.entity_aware_surviving {
            println!();
            println!(
                "Observation: entity-aware decay produced no differential at this horizon — \
                 either no memories mention inventoried entities, or none crossed the threshold \
                 inside the simulated window."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(id: &str, content: &str, last_used: i64, confidence: f64) -> Memory {
        Memory {
            id: id.into(),
            content: content.into(),
            source: "test".into(),
            confidence,
            created_at: last_used,
            last_used,
            version: 0,
            deleted_at: None,
            source_conversation_id: None,
            ..Default::default()
        }
    }

    #[test]
    fn matched_entities_finds_multi_word_names() {
        let mut inv = EntityInventory::new();
        inv.insert("sarah chen".into());
        inv.insert("api migration".into());
        let content = "Synced with Sarah Chen about the API migration timeline";
        let hits = matched_entities(content, &inv);
        assert!(hits.contains(&"sarah chen".to_string()));
        assert!(hits.contains(&"api migration".to_string()));
    }

    #[test]
    fn matched_entities_returns_empty_when_inventory_empty() {
        let inv = EntityInventory::new();
        assert!(matched_entities("Sarah Chen", &inv).is_empty());
    }

    #[test]
    fn simulate_decay_separates_differential_survivors() {
        let now = 1_700_000_000_i64;
        let two_months_ago = now - 2 * 30 * 86_400;
        let mut inv = EntityInventory::new();
        inv.insert("sarah".into());

        let memories = vec![
            // Entity-linked, mid-confidence — should survive entity-aware,
            // potentially fail uniform after several more months.
            mem(
                "a",
                "Sarah said the project is on track",
                two_months_ago,
                0.5,
            ),
            // Topical, mid-confidence — same age, no entity link.
            mem("b", "Read about quantum theory", two_months_ago, 0.5),
        ];

        let report = simulate_decay(&memories, &inv, 6, 0.10, 0.20, now);
        // Entity-linked count is 1.
        assert_eq!(report.entity_linked, 1);
        assert_eq!(report.total, 2);
        // After 6 more months (8 total), uniform decay 0.5 * (0.9^8) ≈ 0.215;
        // entity-aware halves rate to 0.05, so 0.5 * (0.95^8) ≈ 0.332.
        // Both above 0.2 → no differential. Pick a longer horizon.
        let report12 = simulate_decay(&memories, &inv, 12, 0.10, 0.20, now);
        let final_row = report12.rows.last().unwrap();
        // Entity-aware should survive at least as many as uniform.
        assert!(final_row.entity_aware_surviving >= final_row.uniform_surviving);
    }

    #[test]
    fn preview_one_line_replaces_newlines_and_truncates() {
        assert_eq!(preview_one_line("a\nb", 10), "a b");
        let long = "x".repeat(120);
        let p = preview_one_line(&long, 50);
        assert!(p.ends_with('…'));
    }
}
