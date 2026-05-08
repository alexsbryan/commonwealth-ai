//! Brief assembler — renders a token-budgeted markdown brief for a
//! given working set.
//!
//! The brief replaces the always-on `inject-notes.sh` UserPromptSubmit
//! hook: every session prompt sees a fresh, file-scoped summary of
//! active notes, structural atoms anchored to those files, and recent
//! git activity. It is the load-bearing artifact for the situated-agent
//! property — the model arrives knowing the codebase, every prompt.
//!
//! ## Sections, in order
//!
//! 1. **Working set** — file paths the engineer is changing now.
//!    Always present.
//! 2. **Stated about this area** — `[decision]` and `[invariant]`
//!    notes from the [`NoteStore`], filtered by file overlap when
//!    note metadata names files. Replaces the generic note injection.
//! 3. **Structurally observed** — atoms from the structural atlas
//!    anchored to working-set files. Joined via the
//!    `git_archaeology.json` sidecar (per-atom `file_path` mapping)
//!    so we don't have to open the LanceDB chunk index per prompt.
//!    Skipped if archaeology hasn't been run.
//! 4. **Recent activity** — commits in the last 7 days that touched
//!    a working-set file. Uses
//!    [`corpus_engine::git_archaeology::batch_harvest_all_commits`].
//!    Skipped if `repo_root` isn't a git repo.
//!
//! All sections are token-budgeted via
//! [`crate::knowledge_view::tokens::estimate_tokens`], same pattern
//! as `digest::format_landscape` (per-bullet check, hard-cap trim).
//!
//! ## v0 trade-offs
//!
//! - No "Explicit gaps" section yet (requires cross_corpus_edges
//!   matching, which only exists for atlases that ran the
//!   cross-corpus pass). v0.5.
//! - No caching; the brief is reassembled on every call. The
//!   git-archaeology read is the dominant cost (single JSON parse,
//!   typically <50ms). v0.5 will add a `~/.cache/sovereign/brief-*.md`
//!   layer if per-prompt latency becomes painful.
//! - File-filter on notes is best-effort: a note with no `files` set
//!   is included unconditionally (it's likely globally relevant);
//!   a note with non-empty `files` is included only on overlap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use corpus_engine::archaeology_eval::{inquiries_matching_files, load_inquiries_from_dir};
use corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope};
use corpus_engine::git_archaeology::{batch_harvest_all_commits, CommitRecord};
use corpus_engine::{NoteRow, NoteStore};
use serde::Deserialize;

use crate::knowledge_view::tokens::estimate_tokens;

// ── Inputs and errors ─────────────────────────────────────────

#[derive(Debug)]
pub enum BriefError {
    Io(std::io::Error),
    Note(String),
    Json(String),
}

impl std::fmt::Display for BriefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Note(e) => write!(f, "note store: {e}"),
            Self::Json(e) => write!(f, "json: {e}"),
        }
    }
}

impl std::error::Error for BriefError {}

/// Inputs for the assembler. Most fields are optional so the brief
/// degrades gracefully on minimal data.
pub struct BriefInputs<'a> {
    /// Working-set files, repo-root-relative.
    pub working_set: &'a [PathBuf],
    /// Filesystem root of the git repo, used for archaeology lookup.
    /// `None` ⇒ skip the "Recent activity" section.
    pub repo_root: Option<&'a Path>,
    /// Atlas directory (typically
    /// `~/.sovereign/indexes/<id>-self-atlas/atlas`). `None` ⇒
    /// skip the "Structurally observed" section.
    pub atlas_dir: Option<&'a Path>,
    /// Directory of `inquiries/*.toml`. When provided and any
    /// inquiry's globs match a working-set file, a "Principles for
    /// this area" section is rendered. `None` ⇒ skip the section.
    pub inquiries_dir: Option<&'a Path>,
    /// Repo display name for the brief header.
    pub repo_name: &'a str,
    /// Branch name for the brief header.
    pub branch_name: &'a str,
    /// Token budget. Soft-capped per section, hard-capped at the end.
    pub budget_tokens: usize,
    /// Optional ATOS feature id to scope notes (mirrors
    /// `inject-notes.sh` behaviour).
    pub feature_id: Option<&'a str>,
}

/// The structural-atlas-side sidecar produced by `sovereign git-archaeology`.
/// We only deserialize the fields we use.
#[derive(Debug, Deserialize)]
struct ArchaeologySidecar {
    #[serde(default)]
    provenance: Vec<ArchaeologyEntry>,
}

#[derive(Debug, Deserialize)]
struct ArchaeologyEntry {
    atom_id: String,
    file_path: PathBuf,
}

// ── Public entry point ────────────────────────────────────────

/// Assemble the brief. Returns markdown sized to ≤ `budget_tokens`
/// (conservatively — see token-estimator bias).
pub async fn assemble_brief(
    inputs: BriefInputs<'_>,
    notes: &NoteStore,
) -> Result<String, BriefError> {
    let mut out = String::new();
    let mut remaining = inputs.budget_tokens;

    write_header(&mut out, inputs.repo_name, inputs.branch_name);

    // Section 1: Working set — always present.
    let s1 = render_working_set(inputs.working_set);
    push_if_fits(&mut out, &mut remaining, &s1);

    // Section 1.5: Principles for this area.
    // Slotted between working-set and notes so the model sees
    // architectural commitments before any narrative claims.
    if let Some(inquiries_dir) = inputs.inquiries_dir {
        let s_principles =
            render_principles(inquiries_dir, inputs.working_set, remaining);
        if !s_principles.is_empty() {
            push_if_fits(&mut out, &mut remaining, &s_principles);
        }
    }

    // Section 2: Active notes (decisions + invariants).
    let s2 = render_notes(notes, inputs.feature_id, inputs.working_set, remaining)
        .await
        .unwrap_or_default();
    if !s2.is_empty() {
        push_if_fits(&mut out, &mut remaining, &s2);
    }

    // Section 3: Structural atoms anchored to working-set files.
    if let Some(atlas_dir) = inputs.atlas_dir {
        if let Ok(s3) = render_structural(atlas_dir, inputs.working_set, remaining) {
            if !s3.is_empty() {
                push_if_fits(&mut out, &mut remaining, &s3);
            }
        }
    }

    // Section 4: Recent activity.
    if let Some(repo_root) = inputs.repo_root {
        if let Ok(s4) = render_recent_activity(repo_root, inputs.working_set, 7, remaining) {
            if !s4.is_empty() {
                push_if_fits(&mut out, &mut remaining, &s4);
            }
        }
    }

    // Hard guard — drop trailing lines until we're under budget.
    while estimate_tokens(&out) > inputs.budget_tokens {
        match out.rfind('\n') {
            Some(idx) if idx > 0 => out.truncate(idx),
            _ => {
                out.clear();
                break;
            }
        }
    }

    let _ = remaining; // silence unused on the last branch
    Ok(out)
}

fn push_if_fits(out: &mut String, remaining: &mut usize, section: &str) {
    let cost = estimate_tokens(section);
    if cost > *remaining {
        return;
    }
    out.push_str(section);
    *remaining = remaining.saturating_sub(cost);
}

// ── Section renderers ─────────────────────────────────────────

fn write_header(out: &mut String, repo_name: &str, branch_name: &str) {
    out.push_str(&format!(
        "# Project context: {repo_name} @ {branch_name}\n\n"
    ));
}

fn render_working_set(files: &[PathBuf]) -> String {
    let mut out = String::from("## Working set\n\n");
    if files.is_empty() {
        out.push_str("_No files in scope (clean branch or zero-diff)._\n\n");
        return out;
    }
    for f in files.iter().take(20) {
        out.push_str(&format!("- `{}`\n", f.display()));
    }
    if files.len() > 20 {
        out.push_str(&format!("- _+{} more_\n", files.len() - 20));
    }
    out.push('\n');
    out
}

/// Surface architectural commitments (inquiries) that target a file
/// in the working set. Reuses the eval framework's inquiry loader +
/// glob matcher so principles-as-inquiries stays one source of truth.
fn render_principles(
    inquiries_dir: &Path,
    working_set: &[PathBuf],
    remaining: usize,
) -> String {
    let inquiries = match load_inquiries_from_dir(inquiries_dir) {
        Ok(i) => i,
        Err(_) => return String::new(),
    };
    if inquiries.is_empty() {
        return String::new();
    }
    let matching = inquiries_matching_files(&inquiries, working_set);
    if matching.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Principles for this area\n\n");
    let mut spent = estimate_tokens(&out);
    for inq in matching.iter().take(8) {
        let line = format!("- **{}** (`{}`)\n", inq.title, inq.id);
        let cost = estimate_tokens(&line);
        if spent + cost > remaining {
            break;
        }
        out.push_str(&line);
        spent += cost;
    }
    out.push('\n');
    out
}

async fn render_notes(
    notes: &NoteStore,
    feature_id: Option<&str>,
    working_set: &[PathBuf],
    remaining: usize,
) -> Result<String, BriefError> {
    use corpus_engine::{NoteScope, ScopeFilter};
    // `reflection` joins decision + invariant so session-end captures
    // (written by `sovereign code reflect`) surface in the next
    // session's brief automatically — closing the feedback loop.
    let kinds: Vec<String> = vec![
        "decision".into(),
        "invariant".into(),
        "reflection".into(),
    ];
    let scope_filter = match feature_id {
        Some(f) => ScopeFilter {
            scopes: vec![NoteScope::Global, NoteScope::Feature],
            feature_id: Some(f.to_string()),
        },
        None => ScopeFilter {
            scopes: vec![NoteScope::Global],
            feature_id: None,
        },
    };
    let rows = notes
        .read_notes_scoped(None, &[], &[], &kinds, 30, false, &scope_filter)
        .await
        .map_err(|e| BriefError::Note(format!("{e}")))?;

    if rows.is_empty() {
        return Ok(String::new());
    }

    // File overlap filter — keep notes whose `files` overlap the
    // working set, OR whose `files` is empty (likely global note).
    let ws_set: std::collections::HashSet<String> = working_set
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let mut kept: Vec<&NoteRow> = Vec::new();
    for row in &rows {
        if row.files.is_empty()
            || row
                .files
                .iter()
                .any(|f| ws_set.contains(f))
        {
            kept.push(row);
        }
    }
    if kept.is_empty() {
        return Ok(String::new());
    }

    let mut out = String::from("## Stated about this area\n\n");
    let mut spent = estimate_tokens(&out);
    for row in kept.iter().take(15) {
        let line = format!(
            "- **[{}]** {}\n",
            row.kind,
            truncate_to_chars(&row.content, 220)
        );
        let cost = estimate_tokens(&line);
        if spent + cost > remaining {
            break;
        }
        out.push_str(&line);
        spent += cost;
    }
    out.push('\n');
    Ok(out)
}

fn render_structural(
    atlas_dir: &Path,
    working_set: &[PathBuf],
    remaining: usize,
) -> Result<String, BriefError> {
    // Load the archaeology sidecar (atom_id → file_path).
    let arch_path = atlas_dir.join("git_archaeology.json");
    let arch_raw = match std::fs::read_to_string(&arch_path) {
        Ok(s) => s,
        Err(_) => return Ok(String::new()), // archaeology not built — skip
    };
    let arch: ArchaeologySidecar =
        serde_json::from_str(&arch_raw).map_err(|e| BriefError::Json(format!("{e}")))?;

    // Build atom_id → file_path map from sidecar.
    let mut atom_path: HashMap<String, PathBuf> = HashMap::with_capacity(arch.provenance.len());
    for entry in arch.provenance {
        atom_path.insert(entry.atom_id, entry.file_path);
    }

    // Filter atoms by working-set membership.
    let ws_set: std::collections::HashSet<&Path> = working_set.iter().map(|p| p.as_path()).collect();
    let atoms_file = match read_atlas_atoms(atlas_dir) {
        Ok(a) => a,
        Err(_) => return Ok(String::new()),
    };
    let mut matches: Vec<(String, &AtomEnvelope, PathBuf)> = Vec::new();
    for atom in &atoms_file.atoms {
        let id = atom.id().as_str().to_string();
        let Some(path) = atom_path.get(&id) else {
            continue;
        };
        if !ws_set.contains(path.as_path()) {
            continue;
        }
        matches.push((id, atom, path.clone()));
    }
    if matches.is_empty() {
        return Ok(String::new());
    }
    matches.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)));

    let mut out = String::from("## Structurally observed\n\n");
    let mut spent = estimate_tokens(&out);
    for (_id, atom, path) in matches.iter().take(20) {
        let (kind, name) = atom_label(atom);
        let line = format!("- `{}` — {kind} `{name}`\n", path.display());
        let cost = estimate_tokens(&line);
        if spent + cost > remaining {
            break;
        }
        out.push_str(&line);
        spent += cost;
    }
    out.push('\n');
    Ok(out)
}

fn render_recent_activity(
    repo_root: &Path,
    working_set: &[PathBuf],
    days: i64,
    remaining: usize,
) -> Result<String, BriefError> {
    let history = batch_harvest_all_commits(repo_root)
        .map_err(|e| BriefError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
    let cutoff = chrono::Utc::now().timestamp() - days * 86_400;

    // Collect (timestamp, hash, subject, file) triples for working-set
    // files within the cutoff window. Dedup by hash so a single commit
    // touching N working-set files only renders once.
    let ws_set: std::collections::HashSet<&Path> = working_set.iter().map(|p| p.as_path()).collect();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entries: Vec<(i64, &CommitRecord)> = Vec::new();
    for path in working_set {
        let Some(commits) = history.get(path) else {
            continue;
        };
        for c in commits {
            if c.timestamp < cutoff {
                continue;
            }
            if seen.insert(c.hash.clone()) {
                entries.push((c.timestamp, c));
            }
        }
    }
    let _ = ws_set;
    if entries.is_empty() {
        return Ok(String::new());
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0)); // newest first

    let mut out = format!("## Recent activity (last {days} days)\n\n");
    let mut spent = estimate_tokens(&out);
    for (_ts, c) in entries.iter().take(10) {
        let line = format!(
            "- `{}` ({}) — \"{}\"\n",
            short_hash(&c.hash),
            c.author_email,
            truncate_to_chars(&c.subject, 80)
        );
        let cost = estimate_tokens(&line);
        if spent + cost > remaining {
            break;
        }
        out.push_str(&line);
        spent += cost;
    }
    out.push('\n');
    Ok(out)
}

// ── Helpers ───────────────────────────────────────────────────

fn atom_label(atom: &AtomEnvelope) -> (&'static str, String) {
    match atom {
        AtomEnvelope::Entity(e) => ("entity", e.canonical_name.clone()),
        AtomEnvelope::Event(e) => ("event", truncate_to_chars(&e.description, 60)),
        AtomEnvelope::State(s) => ("state", s.label.clone()),
        AtomEnvelope::Relation(r) => ("relation", r.label.clone()),
        AtomEnvelope::Claim(c) => ("claim", truncate_to_chars(&c.content, 60)),
        AtomEnvelope::Question(q) => ("question", truncate_to_chars(&q.content, 60)),
        AtomEnvelope::Configuration(c) => ("configuration", c.label.clone()),
        AtomEnvelope::ArgumentReconstruction(a) => ("argument", a.name.clone()),
    }
}

fn truncate_to_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

fn short_hash(s: &str) -> String {
    s.chars().take(8).collect()
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Cmd;

    fn init_repo(dir: &Path) {
        assert!(Cmd::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        for (k, v) in [("user.email", "alice@example.com"), ("user.name", "A")] {
            Cmd::new("git").args(["config", k, v]).current_dir(dir).status().unwrap();
        }
    }

    fn write_and_commit(dir: &Path, rel: &str, body: &str, msg: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        Cmd::new("git").args(["add", rel]).current_dir(dir).status().unwrap();
        Cmd::new("git")
            .args(["commit", "-m", msg])
            .current_dir(dir)
            .status()
            .unwrap();
    }

    #[tokio::test]
    async fn empty_inputs_renders_just_the_header_and_empty_working_set() {
        let tmp = tempfile::tempdir().unwrap();
        let notes = NoteStore::open(&tmp.path().join("notes.db")).unwrap();
        let working_set: Vec<PathBuf> = vec![];
        let inputs = BriefInputs {
            working_set: &working_set,
            repo_root: None,
            atlas_dir: None,
            inquiries_dir: None,
            repo_name: "test",
            branch_name: "main",
            budget_tokens: 1500,
            feature_id: None,
        };
        let brief = assemble_brief(inputs, &notes).await.unwrap();
        assert!(brief.contains("# Project context: test @ main"));
        assert!(brief.contains("## Working set"));
        assert!(brief.contains("No files in scope"));
        // Not present: structural / recent (no atlas + no repo).
        assert!(!brief.contains("## Structurally observed"));
        assert!(!brief.contains("## Recent activity"));
    }

    #[tokio::test]
    async fn working_set_lists_files_capped_at_20() {
        let tmp = tempfile::tempdir().unwrap();
        let notes = NoteStore::open(&tmp.path().join("notes.db")).unwrap();
        let working_set: Vec<PathBuf> = (0..25).map(|i| PathBuf::from(format!("f{i}.rs"))).collect();
        let inputs = BriefInputs {
            working_set: &working_set,
            repo_root: None,
            atlas_dir: None,
            inquiries_dir: None,
            repo_name: "test",
            branch_name: "main",
            budget_tokens: 4000,
            feature_id: None,
        };
        let brief = assemble_brief(inputs, &notes).await.unwrap();
        assert!(brief.contains("`f0.rs`"));
        assert!(brief.contains("+5 more"));
    }

    #[tokio::test]
    async fn recent_activity_shows_branch_commits_and_skips_old_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let notes = NoteStore::open(&tmp.path().join("notes.db")).unwrap();
        // One backdated commit + one fresh.
        let p = repo.join("old.rs");
        std::fs::write(&p, "fn old() {}\n").unwrap();
        Cmd::new("git").args(["add", "old.rs"]).current_dir(repo).status().unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "old commit body"])
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00 +0000")
            .current_dir(repo)
            .status()
            .unwrap();
        write_and_commit(repo, "fresh.rs", "fn fresh() {}\n", "fresh commit subject");

        let working_set = vec![PathBuf::from("fresh.rs"), PathBuf::from("old.rs")];
        let inputs = BriefInputs {
            working_set: &working_set,
            repo_root: Some(repo),
            atlas_dir: None,
            inquiries_dir: None,
            repo_name: "test",
            branch_name: "main",
            budget_tokens: 4000,
            feature_id: None,
        };
        let brief = assemble_brief(inputs, &notes).await.unwrap();
        assert!(brief.contains("Recent activity"));
        assert!(brief.contains("fresh commit subject"));
        // Old commit (2020) is outside the 7-day window.
        assert!(!brief.contains("old commit body"));
    }

    #[tokio::test]
    async fn budget_is_honored_under_pressure() {
        let tmp = tempfile::tempdir().unwrap();
        let notes = NoteStore::open(&tmp.path().join("notes.db")).unwrap();
        // 100 files; with a tiny budget many should be dropped.
        let working_set: Vec<PathBuf> =
            (0..100).map(|i| PathBuf::from(format!("file{i}.rs"))).collect();
        let inputs = BriefInputs {
            working_set: &working_set,
            repo_root: None,
            atlas_dir: None,
            inquiries_dir: None,
            repo_name: "test",
            branch_name: "main",
            budget_tokens: 50,
            feature_id: None,
        };
        let brief = assemble_brief(inputs, &notes).await.unwrap();
        let cost = estimate_tokens(&brief);
        assert!(cost <= 50, "budget overrun: {cost} > 50");
    }
}
