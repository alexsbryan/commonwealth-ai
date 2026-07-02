// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atos spec diff|accept` — first-class gestures for spec
//! drift.
//!
//! `spec diff <id>` resolves the approved spec content (via MeshStore
//! snapshot or `git show <witness>:<path>`), writes it to a temp file,
//! and shells `diff -u` between that and the current on-disk spec.
//!
//! `spec accept <id> [--reason <text>]` rewrites the MeshStore approval
//! row with the current spec's hash + full snapshot. A
//! `deviation`-kind note captures the diff + operator reason so future
//! sessions see the justification inline. MeshStore wins over git at
//! resolution time (see [`sovereign_atos::approval::find_approval`]),
//! so drift goes silent on the next request.

use std::path::Path;

use super::args::{get_flag, split_args};
use super::feature::derive_node_id_from_git;
use super::stores::open_note_store;

pub(crate) async fn cmd_spec(args: &[String]) -> i32 {
    let Some(sub) = args.first().cloned() else {
        eprintln!("spec: missing subcommand (diff|accept)");
        return 2;
    };
    let rest = &args[1..];
    match sub.as_str() {
        "diff" => cmd_spec_diff(rest).await,
        "accept" => cmd_spec_accept(rest).await,
        other => {
            eprintln!("spec: unknown subcommand '{other}'");
            2
        }
    }
}

pub(crate) async fn cmd_spec_diff(args: &[String]) -> i32 {
    let (positional, _flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("spec diff: missing <feature-id>");
        return 2;
    };
    let repo_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("spec diff: cwd: {e}");
            return 1;
        }
    };

    let mesh = open_repo_mesh(&repo_root).ok();
    let approval =
        match sovereign_atos::approval::find_approval(&repo_root, &feature_id, mesh.as_ref()) {
            Some(a) => a,
            None => {
                eprintln!(
                    "spec diff: no approval found for '{feature_id}' — \
                 `git commit` the spec or run `svrn atos feature approve {feature_id}`"
                );
                return 1;
            }
        };

    let approved =
        match sovereign_atos::approval::resolve_approved_spec_content(&repo_root, &approval) {
            Some(c) => c,
            None => {
                eprintln!(
                    "spec diff: could not resolve approved spec content \
                 (witness commit may have been orphaned; run `spec accept` \
                 to re-anchor)"
                );
                return 1;
            }
        };

    let current_path = repo_root.join(&approval.spec_path);
    if !current_path.exists() {
        eprintln!(
            "spec diff: current spec missing at {}",
            current_path.display()
        );
        return 1;
    }

    // Write the approved text to a scratch file so `diff -u` can see
    // labels that include "approved". A heredoc-style stdin would
    // work but loses the filename label.
    let tmp = match tempfile::Builder::new()
        .prefix("atos-spec-approved-")
        .suffix(".md")
        .tempfile()
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("spec diff: tempfile: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::write(tmp.path(), approved.as_bytes()) {
        eprintln!("spec diff: write tempfile: {e}");
        return 1;
    }

    let approved_label = format!("approved ({})", truncate_witness(&approval.witness));
    let current_label = format!("current ({})", approval.spec_path);
    let output = std::process::Command::new("diff")
        .args([
            "-u",
            "--label",
            &approved_label,
            tmp.path().to_str().unwrap_or(""),
            "--label",
            &current_label,
            current_path.to_str().unwrap_or(""),
        ])
        .output();
    match output {
        Ok(out) => {
            // `diff -u` exits 0 if identical, 1 if different, 2+ on error.
            // We print stdout in both cases (empty on identical).
            if out.stdout.is_empty() && out.status.code() == Some(0) {
                println!("spec diff: no drift — current spec matches approval.");
                return 0;
            }
            print!("{}", String::from_utf8_lossy(&out.stdout));
            if let Some(2) = out.status.code() {
                eprint!("{}", String::from_utf8_lossy(&out.stderr));
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("spec diff: exec diff: {e}");
            1
        }
    }
}

pub(crate) async fn cmd_spec_accept(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("spec accept: missing <feature-id>");
        return 2;
    };
    let reason = get_flag(&flags, "reason").unwrap_or_default();
    let repo_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("spec accept: cwd: {e}");
            return 1;
        }
    };

    let mesh = match open_repo_mesh(&repo_root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("spec accept: mesh open: {e}");
            return 1;
        }
    };

    let prior = match sovereign_atos::approval::find_approval(&repo_root, &feature_id, Some(&mesh))
    {
        Some(a) => a,
        None => {
            eprintln!(
                "spec accept: no prior approval for '{feature_id}' — \
                 `feature approve` first, then iterate"
            );
            return 1;
        }
    };

    // Capture diff BEFORE accepting, so the deviation note records
    // the change. If the current spec matches the approval, bail —
    // there's nothing to accept.
    let approved_text = sovereign_atos::approval::resolve_approved_spec_content(&repo_root, &prior)
        .unwrap_or_default();
    let current_path = repo_root.join(&prior.spec_path);
    let current_text = match std::fs::read_to_string(&current_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("spec accept: read current spec: {e}");
            return 1;
        }
    };
    if approved_text == current_text {
        println!("spec accept: current spec already matches approval — nothing to do.");
        return 0;
    }

    let origin = derive_node_id_from_git(&repo_root)
        .unwrap_or_else(|| commonwealth_core::ids::NodeId::from_u128(0xA7057E07_A7057E07u128));

    let accepted = match sovereign_atos::approval::accept_drift(
        &mesh,
        origin,
        &repo_root,
        &feature_id,
        &prior,
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("spec accept: {e}");
            return 1;
        }
    };

    // Write a deviation note capturing the diff + reason. The
    // middleware's drift detector (now silent because the mesh hash
    // matches) won't re-fire, but the operator's history of
    // *deliberate* drift is preserved in the note log.
    let diff_body = unified_diff_string(&approved_text, &current_text, &prior.spec_path);
    let reason_line = if reason.is_empty() {
        "(none provided)".to_string()
    } else {
        reason
    };
    let committer =
        git_committer_identity(&repo_root).unwrap_or_else(|| "<unknown committer>".to_string());
    let note_content = format!(
        "Spec drift accepted by {committer}.\n\n\
         Reason: {reason_line}\n\n\
         Previous hash: {}\n\
         Current hash:  {}\n\n\
         Diff:\n{diff_body}\n",
        prior.spec_content_hash, accepted.spec_content_hash,
    );
    match open_note_store() {
        Ok(notes) => {
            if let Err(e) = notes
                .write_note_scoped(
                    "deviation",
                    &note_content,
                    Vec::new(),
                    vec![prior.spec_path.clone()],
                    "atos-spec-accept",
                    corpus_engine_notes::NoteScope::Feature,
                    Some(&feature_id),
                )
                .await
            {
                // Non-fatal — the approval still succeeded. Warn so
                // the operator knows the note log is incomplete.
                eprintln!("spec accept: warning: note write failed: {e}");
            }
        }
        Err(e) => eprintln!("spec accept: warning: notes.db open: {e}"),
    }

    println!(
        "accepted spec drift for '{}' (new hash {})",
        accepted.feature_id,
        &accepted.spec_content_hash[..8]
    );
    0
}

fn open_repo_mesh(repo_root: &Path) -> Result<commonwealth_state::MeshStore, String> {
    let mesh_path = repo_root.join(".sovereign").join("mesh.db");
    if let Some(parent) = mesh_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    commonwealth_state::MeshStore::open(&mesh_path)
        .map_err(|e| format!("open mesh.db at {}: {e}", mesh_path.display()))
}

fn truncate_witness(w: &str) -> String {
    w.chars().take(12).collect()
}

fn git_committer_identity(repo_root: &Path) -> Option<String> {
    let name = std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    let email = std::process::Command::new("git")
        .args(["config", "user.email"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !name.status.success() || !email.status.success() {
        return None;
    }
    Some(format!(
        "{} <{}>",
        String::from_utf8_lossy(&name.stdout).trim(),
        String::from_utf8_lossy(&email.stdout).trim(),
    ))
}

/// Shell out to `diff -u` to produce the deviation-note body. Falls
/// back to a side-by-side dump when the diff binary is unavailable —
/// the note is for human reference, not machine parse, so a degraded
/// render is acceptable.
fn unified_diff_string(old: &str, new: &str, label: &str) -> String {
    fn write_tmp(prefix: &str, body: &str) -> std::io::Result<tempfile::NamedTempFile> {
        use std::io::Write as _;
        let mut f = tempfile::Builder::new().prefix(prefix).tempfile()?;
        f.write_all(body.as_bytes())?;
        Ok(f)
    }
    if let (Ok(a), Ok(b)) = (
        write_tmp("atos-accept-old-", old),
        write_tmp("atos-accept-new-", new),
    ) {
        let approved_label = format!("approved/{label}");
        let current_label = format!("current/{label}");
        if let Ok(out) = std::process::Command::new("diff")
            .args([
                "-u",
                "--label",
                &approved_label,
                a.path().to_str().unwrap_or(""),
                "--label",
                &current_label,
                b.path().to_str().unwrap_or(""),
            ])
            .output()
        {
            return String::from_utf8_lossy(&out.stdout).into_owned();
        }
    }
    // Degraded fallback: dump both. Not a "diff" but not a lie either.
    format!("--- approved/{label}\n{old}\n+++ current/{label}\n{new}\n")
}
