// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn awareness reset` — clear entity enrichment state.
//!
//! Two modes:
//!
//!   - `--entities-only` deletes `atoms.json` + `edges.json` from
//!     both relational atlas dirs. The next `awareness extract` run
//!     re-derives them from the same conversation history.
//!
//!   - `--full` additionally truncates the awareness scratch state:
//!     `conversations`, `messages`, and the relational note rows
//!     (`commitment` / `follow_up` / `goal` with non-null
//!     `related_entity`). Use this when you want a pristine start
//!     for a fresh template-driven scenario.
//!
//! No `--force` flag — the developer reads the confirmation prompt
//! before either mode commits.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use rusqlite::{params, Connection, OpenFlags};

use super::args::parse_args;
use super::render::display_path;
use super::store_open::{atlas_dir_for, notes_db_path, sovereign_root, state_db_path};

const RELATIONAL_VIEWS: &[&str] = &["personal-knowledge", "conversation-history"];

pub(super) async fn cmd_reset(args: &[String]) -> i32 {
    let flags = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("awareness: {e}");
            return 2;
        }
    };

    let entities_only = flags.has("entities-only");
    let full = flags.has("full");
    let assume_yes = flags.has("yes") || flags.has("y");

    if entities_only == full {
        eprintln!(
            "awareness reset: pass exactly one of --entities-only or --full\n\
             \n\
             USAGE\n  awareness reset --entities-only   Drop atoms.json + edges.json only\n\
             \x20 awareness reset --full              Also truncate conversations, messages, \n\
             \x20                                    and entity-linked notes."
        );
        return 2;
    }

    let root = sovereign_root(&flags);
    let mode = if entities_only {
        ResetMode::EntitiesOnly
    } else {
        ResetMode::Full
    };

    // Plan + confirm.
    let plan = plan_reset(&root, mode);
    print_plan(&plan, mode);

    if !assume_yes && !confirm() {
        eprintln!("awareness reset: aborted.");
        return 0;
    }

    // Execute.
    let mut errors: Vec<String> = Vec::new();
    for path in &plan.atlas_files {
        if let Err(e) = std::fs::remove_file(path) {
            // ENOENT is fine — the file was already absent.
            if e.kind() != std::io::ErrorKind::NotFound {
                errors.push(format!("remove {}: {e}", display_path(path)));
            }
        }
    }
    if matches!(mode, ResetMode::Full) {
        if plan.state_db_present {
            if let Err(e) = truncate_state_db(&plan.state_db_path) {
                errors.push(format!("state db: {e}"));
            }
        }
        if plan.notes_db_present {
            if let Err(e) = truncate_relational_notes(&plan.notes_db_path) {
                errors.push(format!("notes db: {e}"));
            }
        }
    }

    if !errors.is_empty() {
        for e in &errors {
            eprintln!("awareness reset: {e}");
        }
        return 1;
    }

    println!("awareness reset: complete.");
    0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResetMode {
    EntitiesOnly,
    Full,
}

#[derive(Debug)]
struct ResetPlan {
    atlas_files: Vec<PathBuf>,
    atlas_files_missing: Vec<PathBuf>,
    state_db_path: PathBuf,
    state_db_present: bool,
    notes_db_path: PathBuf,
    notes_db_present: bool,
}

fn plan_reset(root: &std::path::Path, _mode: ResetMode) -> ResetPlan {
    let mut atlas_files: Vec<PathBuf> = Vec::new();
    let mut atlas_files_missing: Vec<PathBuf> = Vec::new();
    for view in RELATIONAL_VIEWS {
        let atlas = atlas_dir_for(root, view);
        for f in ["atoms.json", "edges.json"] {
            let p = atlas.join(f);
            if p.exists() {
                atlas_files.push(p);
            } else {
                atlas_files_missing.push(p);
            }
        }
    }
    let sdb = state_db_path(root);
    let ndb = notes_db_path();
    ResetPlan {
        atlas_files,
        atlas_files_missing,
        state_db_present: sdb.exists(),
        state_db_path: sdb,
        notes_db_present: ndb.exists(),
        notes_db_path: ndb,
    }
}

fn print_plan(plan: &ResetPlan, mode: ResetMode) {
    println!();
    println!(
        "awareness reset — plan ({}):",
        match mode {
            ResetMode::EntitiesOnly => "--entities-only",
            ResetMode::Full => "--full",
        }
    );
    println!();
    println!("Will delete the following files:");
    if plan.atlas_files.is_empty() {
        println!("  · (no atlas files present)");
    } else {
        for p in &plan.atlas_files {
            println!("  · {}", display_path(p));
        }
    }
    if matches!(mode, ResetMode::Full) {
        println!();
        println!("Will truncate the following tables:");
        if plan.state_db_present {
            println!(
                "  · {} → conversations, messages",
                display_path(&plan.state_db_path)
            );
        } else {
            println!(
                "  · ({} not present — state DB skipped)",
                display_path(&plan.state_db_path)
            );
        }
        if plan.notes_db_present {
            println!(
                "  · {} → notes WHERE kind IN (commitment, follow_up, goal) AND related_entity IS NOT NULL",
                display_path(&plan.notes_db_path)
            );
        } else {
            println!(
                "  · ({} not present — notes DB skipped)",
                display_path(&plan.notes_db_path)
            );
        }
    }
    if !plan.atlas_files_missing.is_empty() {
        println!();
        println!("Already absent (not deleted):");
        for p in &plan.atlas_files_missing {
            println!("  · {}", display_path(p));
        }
    }
    println!();
}

fn confirm() -> bool {
    print!("Proceed? type 'y' to confirm, anything else aborts: ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    if io::stdin().lock().read_line(&mut answer).unwrap_or(0) == 0 {
        return false;
    }
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

fn truncate_state_db(path: &std::path::Path) -> rusqlite::Result<()> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // Order matters only loosely — both tables stand alone in the
    // sovereign state DB with no FK enforcement.
    conn.execute("DELETE FROM messages", params![])?;
    conn.execute("DELETE FROM conversations", params![])?;
    Ok(())
}

fn truncate_relational_notes(path: &std::path::Path) -> rusqlite::Result<()> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute(
        "DELETE FROM notes
         WHERE kind IN ('commitment', 'follow_up', 'goal')
           AND related_entity IS NOT NULL",
        params![],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_reset_lists_present_and_missing_atlas_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        // Create one atlas file but not the other.
        let atlas = atlas_dir_for(&root, "personal-knowledge");
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(atlas.join("atoms.json"), "[]").unwrap();
        // Don't create edges.json or the conversation-history atlas.

        let plan = plan_reset(&root, ResetMode::EntitiesOnly);
        assert_eq!(
            plan.atlas_files.len(),
            1,
            "only atoms.json should be present"
        );
        assert!(
            plan.atlas_files_missing.len() >= 3,
            "should list edges.json + conversation-history pair as missing"
        );
    }

    #[test]
    fn truncate_relational_notes_removes_only_entity_linked_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("notes.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                content TEXT NOT NULL,
                related_entity TEXT
            );
            INSERT INTO notes VALUES ('1', 'commitment', 'a', 'Sarah');
            INSERT INTO notes VALUES ('2', 'follow_up', 'b', 'Mike');
            INSERT INTO notes VALUES ('3', 'goal', 'c', 'API migration');
            INSERT INTO notes VALUES ('4', 'todo', 'd', NULL);
            INSERT INTO notes VALUES ('5', 'commitment', 'e', NULL);
            ",
        )
        .unwrap();
        drop(conn);

        truncate_relational_notes(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
            .unwrap();
        // Rows 4 and 5 should remain (todo without entity, commitment without entity).
        assert_eq!(count, 2);
    }
}
