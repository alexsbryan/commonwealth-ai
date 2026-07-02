// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn recipe-agent` — recipe-author project lifecycle CLI.
//!
//! Thin command surface for M1:
//!
//! - `recipe-agent new --charter <file> [--title <title>]`
//!   Provision a new recipe-author project. Prints the new
//!   `feature_id` to stdout; the partner / driver hands this id to
//!   the actual agent loop (`svrn chat` with the
//!   `recipe-author` skill activated).
//!
//! - `recipe-agent show <feature_id>`
//!   Print the per-turn situated-context block for a project.
//!   Useful as a sanity check that the renderer works against real
//!   on-disk + NoteStore state, and as the dashboard's plain-text
//!   stand-in until M2 lays down the desktop view.
//!
//! - `recipe-agent list`
//!   List every recipe-author project on the local install.
//!
//! The full agent-driven chat loop is exercised through the
//! existing chat REPL once the recipe-author skill is on
//! `~/.sovereign/skills/`. M1 acceptance is the persistence layer +
//! tools + skill + renderer; the chat-loop integration is mechanical
//! extension that M2 polishes.

use std::path::PathBuf;
use std::sync::Arc;

use sovereign_store::recipe_project_store::{RecipeProjectRow, RecipeProjectStore};
use corpus_engine_notes::NoteStore;
use sovereign_tools::recipe_author::{
    capability_request::CapabilityRequest, maintainer_inbox_dir, situated_context, RecipeProject,
};

fn print_help() {
    eprintln!(
        "Usage:\n  \
         sovereign recipe-agent new --charter <FILE> [--title <TITLE>]\n  \
         sovereign recipe-agent show <FEATURE_ID>\n  \
         sovereign recipe-agent list\n  \
         sovereign recipe-agent live-trial --charter <FILE> --script <FILE> [...]\n\n\
        \"new\" prints the new project's feature_id to stdout. Pass it to \n\
        the chat REPL (with the recipe-author skill on \n\
        ~/.sovereign/skills/) to drive the agent loop.\n\
        \"live-trial\" drives the agent end-to-end against the running\n\
        daemon's /v1/chat/completions, using a script of partner messages,\n\
        then validates the generated recipe + runs an initial fetch.\n\
        See `svrn recipe-agent live-trial --help` for flags.\n"
    );
}

pub async fn run_recipe_agent(args: &[String]) -> i32 {
    let Some(sub) = args.first() else {
        print_help();
        return 1;
    };
    match sub.as_str() {
        "--help" | "-h" | "help" => {
            print_help();
            0
        }
        "new" => run_new(&args[1..]).await,
        "show" => run_show(&args[1..]).await,
        "list" => run_list(&args[1..]).await,
        "live-trial" => crate::recipe_agent_live_trial::run_live_trial(&args[1..]).await,
        other => {
            eprintln!("recipe-agent: unknown subcommand `{other}`");
            print_help();
            1
        }
    }
}

async fn run_new(args: &[String]) -> i32 {
    let mut charter_path: Option<PathBuf> = None;
    let mut title: Option<String> = None;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--charter" => {
                charter_path = iter.next().map(PathBuf::from);
            }
            "--title" => {
                title = iter.next().cloned();
            }
            other => {
                eprintln!("recipe-agent new: unknown flag `{other}`");
                return 1;
            }
        }
    }
    let Some(path) = charter_path else {
        eprintln!("recipe-agent new: --charter <FILE> is required");
        return 1;
    };
    let charter = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("recipe-agent new: failed to read {}: {e}", path.display());
            return 1;
        }
    };
    let title = title.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("recipe-author project")
            .to_string()
    });

    let (notes, features) = match open_stores() {
        Ok(s) => s,
        Err(code) => return code,
    };
    let project = match RecipeProject::new(&title, &charter, notes, features).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("recipe-agent new: provision failed: {e}");
            return 2;
        }
    };
    println!("{}", project.feature_id());
    eprintln!(
        "Provisioned recipe-author project `{}` (feature_id={}).\n\
         Project dir: {}",
        title,
        project.feature_id(),
        project.project_dir().display()
    );
    0
}

async fn run_show(args: &[String]) -> i32 {
    let Some(feature_id) = args.first() else {
        eprintln!("recipe-agent show: missing <FEATURE_ID>");
        return 1;
    };
    let (notes, features) = match open_stores() {
        Ok(s) => s,
        Err(code) => return code,
    };
    let project = match RecipeProject::load(feature_id, notes, features).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("recipe-agent show: {e}");
            return 2;
        }
    };
    let block = match situated_context::render(&project).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("recipe-agent show: render failed: {e}");
            return 2;
        }
    };
    println!("{block}");
    0
}

async fn run_list(_args: &[String]) -> i32 {
    let (_notes, features) = match open_stores() {
        Ok(s) => s,
        Err(code) => return code,
    };
    let all = match features.list(true).await {
        Ok(rs) => rs,
        Err(e) => {
            eprintln!("recipe-agent list: {e}");
            return 2;
        }
    };
    let rows: Vec<RecipeProjectRow> = all
        .into_iter()
        .collect();
    if rows.is_empty() {
        println!("(no recipe-author projects)");
        return 0;
    }
    for r in rows {
        print_row(&r);
    }
    0
}

fn print_row(r: &RecipeProjectRow) {
    println!("{}\t{}", r.id, r.title);
}

/// `svrn maintainer inbox` — dump every pending capability
/// request from the global inbox under
/// `~/.sovereign/capability-requests/inbox/`.
///
/// One JSON object per file, parsed and rendered as a short summary
/// per request followed by the full JSON. v1 ships read-only — the
/// maintainer flips status fields by editing inbox files directly.
pub async fn run_maintainer(args: &[String]) -> i32 {
    let Some(sub) = args.first() else {
        eprintln!("Usage: svrn maintainer inbox");
        return 1;
    };
    match sub.as_str() {
        "inbox" => run_inbox().await,
        "--help" | "-h" | "help" => {
            eprintln!("Usage: svrn maintainer inbox");
            0
        }
        other => {
            eprintln!("maintainer: unknown subcommand `{other}`");
            1
        }
    }
}

async fn run_inbox() -> i32 {
    let dir = match maintainer_inbox_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("maintainer inbox: {e}");
            return 2;
        }
    };
    if !dir.exists() {
        println!("(inbox empty: {})", dir.display());
        return 0;
    }
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect(),
        Err(e) => {
            eprintln!("maintainer inbox: failed to read {}: {e}", dir.display());
            return 2;
        }
    };
    files.sort();
    if files.is_empty() {
        println!("(inbox empty: {})", dir.display());
        return 0;
    }
    println!("Maintainer inbox: {}", dir.display());
    println!();
    for path in files {
        let body = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("- {}: read failed: {e}", path.display());
                continue;
            }
        };
        match serde_json::from_str::<CapabilityRequest>(&body) {
            Ok(req) => {
                println!(
                    "- {} [{}] project={} format={}",
                    req.request_id, req.status, req.feature_id, req.format_or_source
                );
                println!("  analysis: {}", req.analysis);
                if !req.failure_modes.is_empty() {
                    println!("  failure modes:");
                    for f in &req.failure_modes {
                        println!("    · {f}");
                    }
                }
                if !req.blocked_recipe_parts.is_empty() {
                    println!("  blocked: {}", req.blocked_recipe_parts.join(", "));
                }
                println!("  file: {}", path.display());
                println!();
            }
            Err(e) => {
                eprintln!(
                    "- {}: malformed inbox entry ({e}); raw body follows.\n{body}\n",
                    path.display()
                );
            }
        }
    }
    0
}

fn open_stores() -> std::result::Result<(Arc<NoteStore>, Arc<RecipeProjectStore>), i32> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("recipe-agent: HOME not set; cannot open stores");
            return Err(2);
        }
    };
    let sov = home.join(".sovereign");
    let notes_path = sov.join("notes.db");
    let features_path = sov.join("features.db");
    let notes = match NoteStore::open(&notes_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "recipe-agent: failed to open NoteStore at {}: {e}",
                notes_path.display()
            );
            return Err(2);
        }
    };
    let features = match RecipeProjectStore::open(&features_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "recipe-agent: failed to open RecipeProjectStore at {}: {e}",
                features_path.display()
            );
            return Err(2);
        }
    };
    Ok((notes, features))
}
