// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn portfolio …` — a user's named set of corpus_ids (FR-11).
//!
//! A portfolio is a named set of installed `corpus_id`s (today: the
//! per-issuer `proxy-cik…` corpora; the set generalizes to fund corpora
//! later). A `portfolio ask` seals retrieval to that set and fans the
//! query across them, so one question yields per-company answers with
//! per-company citations (AC-6) — NOT a physical merge.
//!
//! PRIVACY (FR-11 / AC-7): WHICH companies a user holds reveals the user
//! and is sensitive, even though the public `proxy-cik…` corpora it names
//! are freely replicable. The set is stored in a CLI-owned, user-global
//! `MeshStore` under the `portfolio-private` app_id — which is in
//! `GOSSIP_EXCLUDED_APP_IDS`, so it never gossips (the same structural
//! guarantee as peer-preferences / notes-private / activity-private), and
//! it lives in its own file the daemon does not replicate.

use bytes::Bytes;
use commonwealth_core::ids::NodeId;
use commonwealth_state::{MeshStore, PORTFOLIO_PRIVATE_APP_ID};

pub mod ask;

const USAGE: &str = "\
usage: sovereign portfolio <subcommand>

subcommands:
  create <name> [corpus-id ...]   Create (or replace) a portfolio with these corpora.
  add    <name> <corpus-id ...>   Add corpora to an existing portfolio (deduped).
  list                            List portfolios and their sizes.
  show   <name>                   Show the corpora in a portfolio.
  ask    <name> \"<question>\"      Ask one question across the portfolio's corpora,
                                  cite-or-abstain, with per-company citations.

A portfolio is a gossip-excluded local set of corpus_ids (FR-11). The public
per-issuer corpora it names still replicate; the SET never leaves the machine.";

/// CLI-owned, user-global portfolio store path. Separate from the daemon's
/// MeshStore so portfolio writes never contend with a running daemon; the
/// `portfolio-private` app_id keeps it gossip-excluded regardless.
fn store_path() -> std::path::PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".sovereign").join("portfolio.db"))
        .unwrap_or_else(|| std::path::PathBuf::from("portfolio.db"))
}

pub(crate) fn open_store() -> Result<(MeshStore, NodeId), String> {
    let p = store_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let store =
        MeshStore::open(&p).map_err(|e| format!("open portfolio store {}: {e}", p.display()))?;
    let data_dir = dirs::home_dir()
        .map(|h| h.join(".sovereign"))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let node_id = sovereign_mesh::persist::load_or_generate_self_node_id(&data_dir);
    Ok((store, node_id))
}

/// The corpus_ids in a named portfolio, or `None` if it doesn't exist.
pub(crate) fn get_portfolio(store: &MeshStore, name: &str) -> Option<Vec<String>> {
    store
        .get(PORTFOLIO_PRIVATE_APP_ID, name)
        .ok()
        .flatten()
        .and_then(|e| serde_json::from_slice::<Vec<String>>(e.value.as_ref()).ok())
}

fn put_portfolio(
    store: &MeshStore,
    node: NodeId,
    name: &str,
    corpora: &[String],
) -> Result<(), String> {
    let bytes = serde_json::to_vec(corpora).map_err(|e| format!("encode portfolio: {e}"))?;
    store
        .set(PORTFOLIO_PRIVATE_APP_ID, name, Bytes::from(bytes), node)
        .map_err(|e| format!("write portfolio: {e}"))?;
    Ok(())
}

/// Dedup-preserving-order union.
fn merge_unique(existing: &[String], add: &[String]) -> Vec<String> {
    let mut out = existing.to_vec();
    for c in add {
        if !out.iter().any(|e| e == c) {
            out.push(c.clone());
        }
    }
    out
}

pub async fn run_portfolio(args: &[String]) -> i32 {
    let mut it = args.iter();
    let Some(sub) = it.next() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let rest: Vec<String> = it.cloned().collect();
    match sub.as_str() {
        "create" => cmd_create(&rest),
        "add" => cmd_add(&rest),
        "list" => cmd_list(),
        "show" => cmd_show(&rest),
        "ask" => ask::cmd_ask(&rest).await,
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            0
        }
        other => {
            eprintln!("error: unknown `portfolio` subcommand `{other}`\n\n{USAGE}");
            2
        }
    }
}

fn cmd_create(args: &[String]) -> i32 {
    let Some((name, corpora)) = args.split_first() else {
        eprintln!("error: usage: sovereign portfolio create <name> [corpus-id ...]");
        return 2;
    };
    let (store, node) = match open_store() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let corpora = merge_unique(&[], corpora);
    if let Err(e) = put_portfolio(&store, node, name, &corpora) {
        eprintln!("error: {e}");
        return 1;
    }
    println!(
        "created portfolio `{name}` with {} corpus(es)",
        corpora.len()
    );
    for c in &corpora {
        println!("  · {c}");
    }
    0
}

fn cmd_add(args: &[String]) -> i32 {
    let Some((name, add)) = args.split_first() else {
        eprintln!("error: usage: sovereign portfolio add <name> <corpus-id ...>");
        return 2;
    };
    if add.is_empty() {
        eprintln!("error: pass at least one corpus-id to add");
        return 2;
    }
    let (store, node) = match open_store() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let existing = get_portfolio(&store, name).unwrap_or_default();
    let merged = merge_unique(&existing, add);
    if let Err(e) = put_portfolio(&store, node, name, &merged) {
        eprintln!("error: {e}");
        return 1;
    }
    println!(
        "portfolio `{name}` now has {} corpus(es) (added {})",
        merged.len(),
        merged.len() - existing.len()
    );
    0
}

fn cmd_list() -> i32 {
    let (store, _node) = match open_store() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let entries = match store.scan(PORTFOLIO_PRIVATE_APP_ID, "") {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: scan portfolios: {e}");
            return 1;
        }
    };
    if entries.is_empty() {
        println!(
            "no portfolios yet — create one with `svrn portfolio create <name> <corpus-id ...>`"
        );
        return 0;
    }
    println!("portfolios ({}):", entries.len());
    for e in entries {
        let n = serde_json::from_slice::<Vec<String>>(e.value.as_ref())
            .map(|v| v.len())
            .unwrap_or(0);
        println!(
            "  · {} ({} corpus{})",
            e.key,
            n,
            if n == 1 { "" } else { "es" }
        );
    }
    0
}

fn cmd_show(args: &[String]) -> i32 {
    let Some(name) = args.first() else {
        eprintln!("error: usage: sovereign portfolio show <name>");
        return 2;
    };
    let (store, _node) = match open_store() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    match get_portfolio(&store, name) {
        Some(corpora) => {
            println!("portfolio `{name}` ({} corpus(es)):", corpora.len());
            for c in &corpora {
                println!("  · {c}");
            }
            0
        }
        None => {
            eprintln!("error: no portfolio named `{name}`");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::merge_unique;

    #[test]
    fn merge_unique_dedups_preserving_order() {
        let base = vec!["a".to_string(), "b".to_string()];
        let merged = merge_unique(&base, &["b".into(), "c".into(), "a".into(), "c".into()]);
        assert_eq!(
            merged,
            vec!["a", "b", "c"],
            "dedup, first-seen order preserved"
        );
    }

    #[test]
    fn merge_unique_from_empty_is_the_added_set_deduped() {
        let merged = merge_unique(&[], &["x".into(), "x".into(), "y".into()]);
        assert_eq!(merged, vec!["x", "y"]);
    }
}
