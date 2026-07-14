// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project charter` / `amend` — the team governance docs.
//!
//! `cmd_charter` creates/edits the free-form CHARTER.md; `cmd_amend` runs
//! the post-founding charter edit with adversarial review (logging who,
//! why, and what was argued against); `cmd_amend_design` is the DESIGN.md
//! sibling flow. Split out of `project_cmd` (2026-07-13); pure move. The
//! shared git-identity + date helpers (`git_committer_identity_for_amend`,
//! `today_iso`, `find_repo_root`) resolve through `use super::*`.

use super::*;

const HELP_CHARTER: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn project charter",
    summary: "Create or edit the team's free-form CHARTER.md (governance, culture, onboarding). Distinct from DESIGN.md.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn project charter [--print]"),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--print", "Print the current CHARTER.md to stdout and exit without opening $EDITOR"),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "CHARTER.md is the low-ceremony team governance doc — who we are, how we decide, \
             onboarding pointers. It is NOT auto-generated from DESIGN.md: DESIGN.md says what \
             we're building; CHARTER.md says how we work together on it.\n\n\
             First invocation writes a minimal skeleton and opens $EDITOR. Subsequent invocations \
             just open the existing file. The file lives at `.sovereign/CHARTER.md` (the path \
             `svrn project amend` already uses for drift detection).",
        ),
    ],
};

/// Minimal free-form CHARTER.md skeleton. Data, not program — lives
/// next to its consumer here rather than in an asset file because
/// it's a one-off with no anticipated operator-tuning. See
/// ARCH_PRINCIPLES.md §6 for when to split prose out.
const CHARTER_SKELETON: &str = r#"# Charter

<!-- Low-ceremony governance + onboarding doc. NOT auto-generated.
     DESIGN.md says what we're building; CHARTER.md says how we work
     together on it. Free-form — write honestly, not form-fill. -->

## Who we are


## How we decide


## Onboarding pointers

<!-- Where should a new teammate start? What should they read first,
     in what order? Pointers to DESIGN.md, IMPLEMENTATION_PLAN.md,
     runbooks, dashboards — whatever they'll need. -->


## Amendment log

<!-- Appended to by `svrn project amend`. -->
"#;

// ─── Amend routing ─────────────────────────────────────────────

/// Amend DESIGN.md: open in $EDITOR, diff H2 sections against the
/// curated catalog (`design.anchors` / `design.data-interfaces` /
/// `design.open-questions`), ask the matching adversarial question
/// for each changed section, and append the Q&A to
/// `## Amendment log` inside DESIGN.md itself.
///
/// Does NOT bump any lifecycle flags — DESIGN.md is iterative by
/// design (pun intended). The provenance is the log + git history.
async fn cmd_amend_design(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("svrn project amend design");
        println!();
        println!("Opens <repo>/DESIGN.md in $EDITOR. On save, section-level diff");
        println!("against the curated DESIGN catalog. For each changed section,");
        println!("asks one adversarial question. Appends the Q&A to DESIGN.md's");
        println!("`## Amendment log` section (creating it if absent).");
        return 0;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let path = crate::amend::design_md_path(&repo_root);
    let old_text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("  \u{2717} No DESIGN.md at repo root ({}).", path.display());
            eprintln!("    Run `svrn project design` first.");
            return 2;
        }
    };

    // Snapshot BEFORE opening the editor so we can diff after.
    let old_signals = corpus_engine_atos::design_signals::extract(&old_text);
    let old_hash = crate::found::hash_charter(&old_text);

    // Hand off to $EDITOR. `invoke_editor` returns the re-read file
    // contents on a clean exit, None if the editor failed — in
    // which case we exit without writing anything.
    let new_text = match crate::amend::invoke_editor(&path) {
        Some(s) => s,
        None => {
            eprintln!("  \u{2717} editor did not return cleanly — no amendment recorded.");
            return 1;
        }
    };
    if new_text == old_text {
        eprintln!("  \u{2026} DESIGN.md unchanged — nothing to amend.");
        return 0;
    }

    let new_signals = corpus_engine_atos::design_signals::extract(&new_text);
    let new_hash = crate::found::hash_charter(&new_text);
    let changed = crate::amend::changed_design_sections(&old_signals, &new_signals);

    // Build adversarial Q&A — but only for curated sections. Non-
    // curated edits still get logged (the diff is real) but without
    // a forced question set. Keeps the catalog precise.
    let mut interlocutor = crate::amend::StdinAmendmentInterlocutor::new();
    let questions = crate::amend::questions_for(&changed);
    let mut qa: Vec<(crate::amend::AdversarialQuestion, String)> =
        Vec::with_capacity(questions.len());
    if !questions.is_empty() {
        eprintln!();
        eprintln!(
            "  {} curated section{} changed — adversarial review:",
            questions.len(),
            if questions.len() == 1 { "" } else { "s" }
        );
        for q in &questions {
            let answer = crate::amend::AmendmentInterlocutor::ask_adversarial(&mut interlocutor, q);
            qa.push((q.clone(), answer));
        }
    } else {
        eprintln!();
        eprintln!(
            "  \u{2026} DESIGN.md edited but no curated sections changed — logging the \
             diff without a Q&A (anchors/data-interfaces/open-questions untouched)."
        );
    }

    // Render the amendment-log entry and splice it into the
    // DESIGN.md we're about to persist. Newest entries at the top
    // of the log — matches the append_design_amendment_log
    // implementation and the convention in CHARTER.md's log.
    let today = today_iso();
    let entry = crate::amend::render_design_amendment_entry(&today, &qa, &old_hash, &new_hash);
    let final_md = crate::amend::append_design_amendment_log(&new_text, &entry);

    if let Err(e) = std::fs::write(&path, &final_md) {
        eprintln!("  \u{2717} could not write {}: {e}", path.display());
        return 1;
    }

    // Best-effort re-index.
    let docs_db = repo_root.join(".sovereign").join("project_docs.db");
    match corpus_engine_notes::ProjectDocsStore::open(&docs_db) {
        Ok(docs) => {
            if let Err(e) = docs.index_file(&path, &repo_root).await {
                eprintln!("    \u{26a0} project_docs index failed: {e}");
            }
        }
        Err(e) => eprintln!("    \u{26a0} could not open project_docs.db: {e}"),
    }

    eprintln!(
        "  \u{2713} DESIGN.md amended ({} section{} with Q&A; old sha=`{}` → new sha=`{}`).",
        qa.len(),
        if qa.len() == 1 { "" } else { "s" },
        &old_hash[..old_hash.len().min(12)],
        &new_hash[..new_hash.len().min(12)],
    );
    eprintln!("    Amendment log updated inline at top of the section.");
    eprintln!("    Next: re-run `svrn project plan` if DESIGN.md structure changed.");
    0
}

// ─── Charter ─────────────────────────────────────────────────
//
// Step 7 of the ATOS onboarding redesign. `cmd_charter` is the
// free-form culture/governance doc — distinct from DESIGN.md, which
// is the technical design artifact. First invocation writes a
// minimal skeleton and opens $EDITOR; subsequent runs just open
// the existing file.
//
// The existing canonical path (`.sovereign/CHARTER.md`) is
// preserved so `svrn project amend` and its drift detection
// stay wired up. (The plan's longer-term repo-root-move is a
// separate migration that affects amend + drift detection and
// hasn't landed yet.)
pub(crate) async fn cmd_charter(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_CHARTER);
        return 0;
    }

    let mut print_only = false;
    for arg in args {
        match arg.as_str() {
            "--print" => print_only = true,
            flag if flag.starts_with("--") => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let charter_path = crate::amend::charter_path(&repo_root);
    let sovereign_dir = match charter_path.parent() {
        Some(p) => p.to_path_buf(),
        None => {
            eprintln!(
                "  \u{2717} could not resolve .sovereign/ parent of {}",
                charter_path.display()
            );
            return 1;
        }
    };

    // Ensure .sovereign/ exists — init creates it, but cmd_charter
    // might be invoked in a repo where init was skipped. Safe/cheap.
    if let Err(e) = std::fs::create_dir_all(&sovereign_dir) {
        eprintln!(
            "  \u{2717} could not create {}: {e}",
            sovereign_dir.display()
        );
        return 1;
    }

    let created_fresh = !charter_path.exists();
    if created_fresh {
        if let Err(e) = std::fs::write(&charter_path, CHARTER_SKELETON) {
            eprintln!("  \u{2717} could not write {}: {e}", charter_path.display());
            return 1;
        }
        eprintln!(
            "  \u{2713} CHARTER.md skeleton written at {}.",
            charter_path.display()
        );
    }

    if print_only {
        let text = std::fs::read_to_string(&charter_path).unwrap_or_default();
        println!("{text}");
        return 0;
    }

    // Open in $EDITOR. `crate::amend::invoke_editor` already owns
    // the shell-escape + waitpid dance and is exactly what amend
    // uses when the user edits the charter — one code path, one
    // set of quirks.
    let _ = crate::amend::invoke_editor(&charter_path);

    // Post-edit: re-hash the file, index it, and persist the hash
    // into project.toml so amend's drift detection has a baseline.
    let charter_text = match std::fs::read_to_string(&charter_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "  \u{2717} could not re-read {}: {e}",
                charter_path.display()
            );
            return 1;
        }
    };
    let new_hash = crate::found::hash_charter(&charter_text);

    let project_toml_path = sovereign_dir.join("project.toml");
    if let Ok(mut tf) = crate::project_toml::ProjectTomlFile::read(&project_toml_path) {
        tf.lifecycle.charter_hash = new_hash.clone();
        if let Err(e) = tf.write(&project_toml_path) {
            eprintln!("    \u{26a0} could not persist charter_hash to project.toml: {e}");
        }
    }

    // Index via ProjectDocsStore so `project_context("who we are")`
    // surfaces charter content. Best-effort — the markdown is the
    // source of truth; the index is a query accelerator.
    let docs_db = sovereign_dir.join("project_docs.db");
    match corpus_engine_notes::ProjectDocsStore::open(&docs_db) {
        Ok(docs) => {
            if let Err(e) = docs.index_file(&charter_path, &repo_root).await {
                eprintln!("    \u{26a0} project_docs index failed: {e}");
            }
        }
        Err(e) => eprintln!("    \u{26a0} could not open project_docs.db: {e}"),
    }

    eprintln!(
        "  \u{2713} charter saved ({} bytes, sha=`{}`).",
        charter_text.len(),
        &new_hash[..new_hash.len().min(12)]
    );
    if created_fresh {
        eprintln!(
            "    Next: fill in the Onboarding pointers section — new teammates read this first."
        );
    }
    0
}

// ─── sovereign project amend (M6.7) ──────────────────────────

/// Post-founding charter edit flow with adversarial review. See
/// `crate::amend` for the policy; this function owns the I/O.
///
/// Flow:
/// 1. Refuse unless founded.
/// 2. Read current CHARTER.md; check hash against `charter_hash`.
///    If drifted, ask the user to fold the drift in (y/N).
/// 3. Spawn `$EDITOR` on CHARTER.md.
/// 4. Diff → adversarial Q&A → preview → approve.
/// 5. On approve: write CHARTER.md, bump `charter_version`,
///    update `charter_hash`, persist a decision note with the full
///    Q&A so readers six weeks later can find "why".
pub(crate) async fn cmd_amend(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("svrn project amend [design|charter]");
        println!();
        println!("Edit with an adversarial review. Every amendment is logged —");
        println!("what changed, what arguments the system raised, and your");
        println!("responses.");
        println!();
        println!("  amend           (no target) — alias for `amend charter`");
        println!("  amend charter   — edit .sovereign/CHARTER.md; bumps charter_version");
        println!("  amend design    — edit DESIGN.md; appends `## Amendment log` entry");
        println!();
        println!("`amend charter` requires the project to be founded");
        println!("(`svrn project found`); `amend design` works on any repo");
        println!("with a DESIGN.md at repo root.");
        return 0;
    }

    // Subcommand routing: `amend design` goes to the DESIGN.md flow;
    // bare `amend` and `amend charter` fall through to the existing
    // CHARTER.md flow (preserved verbatim — back-compat). The charter
    // flow doesn't parse flags further so we don't rebind args here.
    if matches!(args.first().map(String::as_str), Some("design")) {
        return cmd_amend_design(&args[1..]).await;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let sovereign_dir = repo_root.join(".sovereign");
    let project_toml_path = sovereign_dir.join("project.toml");
    if !project_toml_path.exists() {
        eprintln!();
        eprintln!(
            "  sovereign project amend: no .sovereign/project.toml found.\n\
             Run `svrn project init` first, then `svrn project found`."
        );
        return 1;
    }
    let mut project_toml = match crate::project_toml::ProjectTomlFile::read(&project_toml_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  sovereign project amend: cannot read project.toml: {e}");
            return 1;
        }
    };
    if !project_toml.lifecycle.founded {
        eprintln!();
        eprintln!(
            "  sovereign project amend: this project hasn't been founded yet.\n\
             Run `svrn project found` first — the charter it produces is \
             what `amend` edits."
        );
        return 1;
    }

    let charter_path = crate::amend::charter_path(&repo_root);
    let old_charter = match std::fs::read_to_string(&charter_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  sovereign project amend: cannot read CHARTER.md: {e}");
            return 1;
        }
    };

    // Drift detection.
    let disk_hash = crate::found::hash_charter(&old_charter);
    let mut interlocutor = crate::amend::StdinAmendmentInterlocutor::new();
    if disk_hash != project_toml.lifecycle.charter_hash {
        let hint = crate::amend::drift_summary(&project_toml.lifecycle.charter_hash, &old_charter);
        if !crate::amend::AmendmentInterlocutor::confirm_drift(&mut interlocutor, &hint) {
            eprintln!();
            eprintln!(
                "  Amendment cancelled. To discard the drift first:\n    git checkout -- {}\n  Then re-run `svrn project amend`.",
                charter_path.display()
            );
            return 0;
        }
    }

    // Editor.
    println!();
    println!("  Opening CHARTER.md in your editor… save and exit when done.");
    let edited_charter = match crate::amend::invoke_editor(&charter_path) {
        Some(c) => c,
        None => {
            eprintln!("  Amendment cancelled (editor did not complete).");
            return 1;
        }
    };

    let committer = git_committer_identity_for_amend(&repo_root)
        .unwrap_or_else(|| "<unknown committer>".to_string());
    let next_version = project_toml.lifecycle.charter_version.saturating_add(1);
    let date = today_iso();

    let outcome = crate::amend::run_amend(
        &old_charter,
        &edited_charter,
        next_version,
        &date,
        &committer,
        &mut interlocutor,
    );

    match outcome {
        crate::amend::AmendOutcome::NoChange => {
            println!();
            println!("  No substantive changes detected — CHARTER.md left as-is.");
            0
        }
        crate::amend::AmendOutcome::Cancelled => {
            println!();
            println!(
                "  Amendment cancelled. Your editor changes are still in {} —\n\
                 re-open and re-run when ready, OR discard with `git checkout --`.",
                charter_path.display()
            );
            0
        }
        crate::amend::AmendOutcome::Approved { new_charter, entry } => {
            if let Err(e) = std::fs::write(&charter_path, &new_charter) {
                eprintln!("  Could not write CHARTER.md: {e}");
                return 1;
            }
            project_toml.lifecycle.charter_version = entry.version;
            project_toml.lifecycle.charter_hash = entry.new_charter_hash.clone();
            if let Err(e) = project_toml.write(&project_toml_path) {
                eprintln!("  Could not update project.toml: {e}");
                return 1;
            }

            // Decision-kind note mirrors the amendment log entry so
            // `read_notes --kind decision` surfaces it without
            // parsing CHARTER.md.
            let notes_path = sovereign_dir.join("notes.db");
            if let Ok(note_store) = corpus_engine_notes::NoteStore::open(&notes_path) {
                let body = crate::amend::render_amendment_note_body(&entry);
                let session_id = format!("amend-v{}", entry.version);
                let rt = tokio::runtime::Handle::current();
                let _ = tokio::task::block_in_place(|| {
                    rt.block_on(note_store.write_note_scoped(
                        "decision",
                        &body,
                        Vec::new(),
                        Vec::new(),
                        &session_id,
                        corpus_engine_notes::NoteScope::Global,
                        None,
                    ))
                });
            }

            println!();
            println!("    \u{2713} CHARTER.md updated");
            println!(
                "    \u{2713} project.toml: charter_version={}, charter_hash={}",
                entry.version,
                &entry.new_charter_hash[..8.min(entry.new_charter_hash.len())]
            );
            println!(
                "    \u{2713} decision note written (session=amend-v{})",
                entry.version
            );
            0
        }
    }
}
