// SPDX-License-Identifier: AGPL-3.0-or-later
//! DESIGN.md onboarding — import an existing doc or drop a minimal
//! template.
//!
//! Two entry points:
//!
//! - [`import_design`] — copy an existing file into `<repo>/DESIGN.md`,
//!   diff-confirming with the user if a DESIGN.md already exists so we
//!   never silently overwrite hand-authored content.
//! - [`ensure_template`] — write the minimal "Anchors + free-form"
//!   template into `<repo>/DESIGN.md` if nothing is there yet, so the
//!   agent has a scaffold to iterate against.
//!
//! Both operations are intentionally small. They don't invoke the
//! agent, don't touch git, don't index — those are the caller's job.
//! The module's contract is "make sure `<repo>/DESIGN.md` exists and
//! reflects the user's intent." The session driver
//! ([`crate::design_session`]) composes these with preflight and
//! transport selection.

use std::fs;
use std::path::{Path, PathBuf};

/// Outcome of onboarding — tells the caller whether a fresh file
/// landed on disk and what state the doc is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardOutcome {
    /// `--import <path>` copied a file to `<repo>/DESIGN.md`.
    Imported {
        /// Absolute path to the newly-written DESIGN.md.
        written: PathBuf,
    },
    /// The template skeleton was written at `<repo>/DESIGN.md`.
    TemplateDropped { written: PathBuf },
    /// A DESIGN.md was already in place and the user declined to
    /// overwrite — the existing file is preserved unchanged.
    PreservedExisting { path: PathBuf },
    /// User cancelled the import (source file unreadable, or they
    /// said "no" on the diff-confirm). No file was changed.
    Cancelled,
}

/// The minimal DESIGN.md template. Intentionally short — the plan
/// calls this "the little signal" — a structured Anchors block up top
/// so the agent has something concrete to extract, then free-form
/// sections the user fills in however they want.
///
/// Every line in the template is either (a) an HTML comment the
/// agent treats as scaffolding (not content) or (b) a heading that
/// structures future edits. `DesignSignals::extract` on this exact
/// template yields zero anchors, empty-section gaps for each body,
/// and zero keyword hits — so `svrn project design --solo` on a
/// fresh template will walk through every gap, which is exactly the
/// intended UX for "start from nothing."
pub const DESIGN_TEMPLATE: &str = r#"# <project> — Design

<!-- Replace or delete every line of this file. The only requirement
     is that this document reflects what you're actually building. -->

## Anchors

<!-- 3–7 lines, each a stable fact about this system that will NOT
     change without an amendment. Keep each anchor concrete — a vague
     anchor is worse than no anchor. Examples:
       · Primary persistence: <technology + why>
       · Primary interface: <HTTP / CLI / library / …>
       · Language + runtime: <…>
       · Deployment surface: <one process | N services | edge> -->

-

## What we're building



## Data & interfaces



## Open questions

<!-- Anything you know you don't know. Prefix with "TBD:" or leave
     short stubs. `svrn project plan` treats these as
     load-bearing; they become OPEN_QUESTIONS.md entries. -->

-
"#;

/// Repo-root DESIGN.md path. Centralized so every caller agrees on
/// the location — the plan commits to repo-root (not `.sovereign/`)
/// for GitHub / editor discoverability.
pub fn design_path(repo_root: &Path) -> PathBuf {
    repo_root.join("DESIGN.md")
}

/// Copy `source` into `<repo>/DESIGN.md`. When a DESIGN.md already
/// exists, prompt the user to confirm before overwriting.
///
/// Returns the outcome. On `Cancelled`, the existing file (if any)
/// is untouched.
pub fn import_design(repo_root: &Path, source: &Path) -> OnboardOutcome {
    let target = design_path(repo_root);

    let source_text = match fs::read_to_string(source) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "    \u{2717} Could not read source: {} ({e})",
                source.display()
            );
            return OnboardOutcome::Cancelled;
        }
    };

    if source_text.trim().is_empty() {
        eprintln!(
            "    \u{2717} Source file is empty: {}. Nothing to import.",
            source.display()
        );
        return OnboardOutcome::Cancelled;
    }

    if target.exists() {
        // Read the existing file so we can show a concrete summary
        // of what would be overwritten. Falling back to "can't read"
        // is a rare edge (file exists but unreadable); treat it
        // conservatively — prompt before clobbering, don't silently
        // overwrite.
        let existing = fs::read_to_string(&target).unwrap_or_default();
        eprintln!();
        eprintln!(
            "  \u{26a0} {} already exists ({} line{}).",
            target.display(),
            existing.lines().count(),
            if existing.lines().count() == 1 {
                ""
            } else {
                "s"
            }
        );
        eprintln!(
            "    Importing from {} ({} lines).",
            source.display(),
            source_text.lines().count()
        );

        let accept = sovereign_cli_shared::prompts::confirm(
            "    Overwrite the existing DESIGN.md?",
            /* default_yes */ false,
        );
        if !accept {
            eprintln!("    \u{2026} Import cancelled. Existing DESIGN.md is unchanged.");
            return OnboardOutcome::PreservedExisting { path: target };
        }
    }

    if let Err(e) = fs::write(&target, source_text) {
        eprintln!("    \u{2717} Could not write {}: {e}", target.display());
        return OnboardOutcome::Cancelled;
    }

    eprintln!("    \u{2713} DESIGN.md imported from {}.", source.display());
    OnboardOutcome::Imported { written: target }
}

/// Write the minimal template into `<repo>/DESIGN.md` if and only if
/// no file is present. Existing files are never overwritten here —
/// callers should use [`import_design`] (which handles the confirm
/// flow) when the user explicitly wants to replace content.
pub fn ensure_template(repo_root: &Path) -> OnboardOutcome {
    let target = design_path(repo_root);
    if target.exists() {
        return OnboardOutcome::PreservedExisting { path: target };
    }
    if let Err(e) = fs::write(&target, DESIGN_TEMPLATE) {
        eprintln!("    \u{2717} Could not write {}: {e}", target.display());
        return OnboardOutcome::Cancelled;
    }
    eprintln!(
        "    \u{2713} DESIGN.md template created at {}.",
        target.display()
    );
    OnboardOutcome::TemplateDropped { written: target }
}

/// Substitute `<project>` in the template with a concrete project id,
/// first-time-only. Leaves every other character byte-identical, so
/// the `DesignSignals` extractor sees the same structure whether the
/// user ran substitution or not. No-op when the template has already
/// been edited past recognition.
pub fn personalize_template_in_place(path: &Path, project_id: &str) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    // Only substitute if the canonical placeholder is still present.
    // This keeps `personalize_template_in_place` safe to call
    // repeatedly — idempotent on edited docs.
    if !text.contains("# <project> — Design") {
        return;
    }
    let replaced = text.replacen(
        "# <project> — Design",
        &format!("# {project_id} — Design"),
        1,
    );
    let _ = fs::write(path, replaced);
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn template_has_required_sections() {
        // If the template shape drifts, downstream code (DesignSignals
        // tests, solo flow, extract_anchors) breaks silently. Pin the
        // contract.
        for required in [
            "# <project> — Design",
            "## Anchors",
            "## What we're building",
            "## Data & interfaces",
            "## Open questions",
        ] {
            assert!(
                DESIGN_TEMPLATE.contains(required),
                "template missing required heading: {required}"
            );
        }
    }

    #[test]
    fn ensure_template_writes_to_empty_repo() {
        let tmp = tmpdir();
        let out = ensure_template(tmp.path());
        let target = design_path(tmp.path());
        assert!(matches!(out, OnboardOutcome::TemplateDropped { .. }));
        assert!(target.exists());
        let contents = fs::read_to_string(target).unwrap();
        assert!(contents.contains("## Anchors"));
    }

    #[test]
    fn ensure_template_preserves_existing_design_md() {
        let tmp = tmpdir();
        let target = design_path(tmp.path());
        fs::write(&target, "# mine\n").unwrap();
        let out = ensure_template(tmp.path());
        assert!(matches!(out, OnboardOutcome::PreservedExisting { .. }));
        let contents = fs::read_to_string(target).unwrap();
        assert_eq!(
            contents, "# mine\n",
            "existing content must not be clobbered"
        );
    }

    #[test]
    fn personalize_template_replaces_placeholder() {
        let tmp = tmpdir();
        let target = design_path(tmp.path());
        fs::write(&target, DESIGN_TEMPLATE).unwrap();
        personalize_template_in_place(&target, "ingest-svc");
        let contents = fs::read_to_string(&target).unwrap();
        assert!(contents.contains("# ingest-svc — Design"));
        assert!(!contents.contains("# <project> — Design"));
    }

    #[test]
    fn personalize_is_idempotent_on_edited_docs() {
        // A user who's already replaced the H1 manually should not
        // see their title silently rewritten on a second init run.
        let tmp = tmpdir();
        let target = design_path(tmp.path());
        fs::write(&target, "# my-custom-title\n\n## Anchors\n\n- one\n").unwrap();
        personalize_template_in_place(&target, "would-be-overwritten");
        let contents = fs::read_to_string(&target).unwrap();
        assert!(contents.contains("# my-custom-title"));
        assert!(!contents.contains("would-be-overwritten"));
    }

    #[test]
    fn import_writes_source_content_to_design_md() {
        let tmp = tmpdir();
        let src = tmp.path().join("my-design.md");
        let mut f = fs::File::create(&src).unwrap();
        writeln!(f, "# Imported — Design").unwrap();
        writeln!(f, "\n## Anchors\n\n- Primary persistence: sqlite").unwrap();
        drop(f);

        let out = import_design(tmp.path(), &src);
        match out {
            OnboardOutcome::Imported { written } => {
                let contents = fs::read_to_string(&written).unwrap();
                assert!(contents.contains("# Imported — Design"));
                assert!(contents.contains("Primary persistence: sqlite"));
            }
            other => panic!("expected Imported, got {other:?}"),
        }
    }

    #[test]
    fn import_rejects_empty_source() {
        let tmp = tmpdir();
        let src = tmp.path().join("empty.md");
        fs::write(&src, "   \n").unwrap();
        let out = import_design(tmp.path(), &src);
        assert!(matches!(out, OnboardOutcome::Cancelled));
        assert!(!design_path(tmp.path()).exists());
    }

    #[test]
    fn import_rejects_missing_source() {
        let tmp = tmpdir();
        let src = tmp.path().join("nope.md");
        let out = import_design(tmp.path(), &src);
        assert!(matches!(out, OnboardOutcome::Cancelled));
    }
}
