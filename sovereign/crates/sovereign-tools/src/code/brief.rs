// SPDX-License-Identifier: AGPL-3.0-or-later
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
//!    [`corpus_engine_archaeology::git_archaeology::batch_harvest_all_commits`].
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

use corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope};
#[cfg(feature = "dev-tools")]
use corpus_engine_archaeology::archaeology_eval::{
    inquiries_matching_files, load_inquiries_from_dir,
};
#[cfg(feature = "dev-tools")]
use corpus_engine_archaeology::git_archaeology::{batch_harvest_all_commits, CommitRecord};
use corpus_engine_notes::{NodeRoster, NoteRow, NoteStore};
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
    /// `~/.svrnmesh/indexes/<id>-self-atlas/atlas`). `None` ⇒
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
    /// Directory holding the architectural-drift fingerprint +
    /// report (typically `~/.svrnmesh/drift/`). When provided and
    /// `repo_root` is set, a "Drift posture" section is rendered.
    /// `None` ⇒ skip the section.
    pub drift_dir: Option<&'a Path>,
    /// Live work-atlas signals overlapping the working set (peer
    /// claims + edit observations), precomputed by the caller via
    /// `sovereign_work_atlas::tools::collect_in_flight`. Empty ⇒
    /// skip the "Work in flight" section. The brief stays a pure
    /// renderer — it never opens the atlas store itself.
    pub work_in_flight: &'a [WorkInFlightEntry],
}

/// One live peer signal for the "Work in flight" section — a claim
/// or an edit observation, already reduced to display fields by the
/// caller (the daemon `briefing` tool or `svrn code brief`).
pub struct WorkInFlightEntry {
    /// What the signal overlaps: a working-set file path or a
    /// claimed scope (symbol id / path prefix).
    pub scope: String,
    /// Confidence grade: `declared` (explicit claim), `active`
    /// (edited ≤5 min ago), or `recent` (edited ≤30 min ago).
    pub grade: String,
    /// Claim intent, or a short edit summary ("edited 3m ago").
    pub detail: String,
    /// Peer node id when known.
    pub node: Option<String>,
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

    // Section 1.2: Work in flight.
    // Directly after the working set because it qualifies those same
    // files: a peer actively editing one of them changes what the
    // agent should do FIRST (coordinate), before any principle or
    // note below matters. Empty input ⇒ no section — quiet is the
    // common case and shouldn't burn brief tokens.
    let s_wif = render_work_in_flight(inputs.work_in_flight, notes.node_roster());
    if !s_wif.is_empty() {
        push_if_fits(&mut out, &mut remaining, &s_wif);
    }

    // Section 1.4: Drift posture.
    // Slots between working-set and principles because drift status
    // ("are the narrative docs still anchored to the code?") frames
    // *every* principle the next section would cite — a stale doc
    // means cited principles may not match the current code.
    //
    // Gated on `treesitter` because `render_drift_posture` calls into
    // `crate::code::drift_posture`, which is itself a treesitter-only
    // module (the posture computation reads SCIP-derived state).
    // Without this cfg the brief still assembles; the drift section is
    // simply omitted on non-treesitter builds.
    #[cfg(feature = "treesitter")]
    if let (Some(drift_dir), Some(repo_root)) = (inputs.drift_dir, inputs.repo_root) {
        let s_drift = render_drift_posture(drift_dir, repo_root);
        if !s_drift.is_empty() {
            push_if_fits(&mut out, &mut remaining, &s_drift);
        }
    }

    // Section 1.5: Principles for this area.
    // Slotted between drift posture and notes so the model sees
    // architectural commitments before any narrative claims.
    if let Some(inquiries_dir) = inputs.inquiries_dir {
        let s_principles = render_principles(inquiries_dir, inputs.working_set, remaining);
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

/// Render the "Work in flight" section — live peer signals on the
/// working set. Empty input renders nothing: quiet is the common
/// case, and an always-present empty section would train readers to
/// skip it. Entries are capped at 8; peer collision is rare enough
/// that more than that means something is systemically wrong (stale
/// TTLs) rather than eight genuine concurrent edits.
///
/// `roster` names the nodes. Without it these lines read `(node
/// b88252e4…)`, which is the exact friction this section exists to
/// remove: a reader cannot tell whether "holding the GPU" is a claim on
/// their own box or on someone else's, so they route around a
/// constraint that was never theirs. With a roster the same entry reads
/// `(BeefyMac ⟵ peer)`.
fn render_work_in_flight(entries: &[WorkInFlightEntry], roster: Option<&NodeRoster>) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Work in flight (peers)\n\n");
    for e in entries.iter().take(8) {
        let node = e
            .node
            .as_deref()
            .map(|n| match roster {
                // Same resolver the notes use, so "peer" cannot mean one
                // thing here and another two sections down.
                Some(r) => format!(" ({})", r.resolve(n).label_compact()),
                // No roster wired: fall back to the raw id rather than
                // inventing a name. The first 8 chars are enough to
                // cross-reference against `sovereign mesh status`.
                None => {
                    let short: String = n.chars().take(8).collect();
                    format!(" (node {short}…)")
                }
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "- [{}] `{}` — {}{}\n",
            e.grade, e.scope, e.detail, node
        ));
    }
    if entries.len() > 8 {
        out.push_str(&format!("- _+{} more_\n", entries.len() - 8));
    }
    out.push_str(
        "\n_Coordinate before editing these — `work_in_flight(scope, match_mode)` for detail._\n\n",
    );
    out
}

/// Render the drift posture section using the same freshness logic
/// the `drift_posture` MCP tool exposes. Cheap — reads the fingerprint
/// sidecar + SHA-256 hashes a couple of small markdown files.
///
/// Skipped (empty string) when the posture has nothing actionable to
/// say (`fresh` with no Act-on findings) — a clean drift state is
/// the default and shouldn't burn brief tokens.
///
/// Treesitter-only: depends on the `crate::code::drift_posture` module
/// which is itself feature-gated. The caller in `assemble()` is
/// cfg-gated to match so non-treesitter builds compile cleanly without
/// pulling this section.
#[cfg(feature = "treesitter")]
fn render_drift_posture(drift_dir: &Path, repo_root: &Path) -> String {
    use crate::code::drift_posture::{compute_posture, PostureStatus, DEFAULT_NARRATIVES};

    let narrative_paths: Vec<PathBuf> = DEFAULT_NARRATIVES
        .iter()
        .map(|p| {
            let joined = repo_root.join(p);
            std::fs::canonicalize(&joined).unwrap_or(joined)
        })
        .collect();
    let posture = compute_posture(drift_dir, &narrative_paths);

    // Skip the section entirely when there's nothing the operator
    // needs to act on. `fresh` + zero Act-on = noise.
    let act_on = posture.act_on_count.unwrap_or(0);
    if matches!(posture.status, PostureStatus::Fresh) && act_on == 0 {
        return String::new();
    }

    let mut out = String::from("## Drift posture\n\n");
    let status_line = match posture.status {
        PostureStatus::Fresh => format!(
            "fresh · {} Act-on finding{}",
            act_on,
            if act_on == 1 { "" } else { "s" }
        ),
        PostureStatus::Stale => {
            let age = posture
                .age_seconds
                .map(|s| format!(" · last run {}", humanize_age(s)))
                .unwrap_or_default();
            let touched = if posture.stale_paths.len() == 1 {
                " · 1 narrative doc changed".to_string()
            } else {
                format!(" · {} narrative docs changed", posture.stale_paths.len())
            };
            format!("**stale**{age}{touched}")
        }
        PostureStatus::Partial => "**partial** · new narrative doc(s) since last run".into(),
        PostureStatus::NeverRun => {
            "**never run** — `sovereign drift detect` to seed the report".into()
        }
    };
    out.push_str(&status_line);
    out.push_str("\n\n");

    // Surface the top 3 critical findings when we have them — those
    // are the deviations the next session is most likely to need
    // to act on.
    for c in posture.top_critical.iter().take(3) {
        let section = c
            .section
            .as_deref()
            .map(|s| format!(" {}", s))
            .unwrap_or_default();
        let doc = if c.doc.is_empty() {
            String::new()
        } else {
            c.doc.to_string()
        };
        out.push_str(&format!(
            "- {doc}{section} — {}\n",
            truncate_to_chars(&c.claim, 140)
        ));
    }
    if act_on > 3 {
        out.push_str(&format!(
            "- _+{} more (see `~/.svrnmesh/drift/latest.md`)_\n",
            act_on - 3
        ));
    }

    // Hint the operator at the remediation when stale or never-run.
    if matches!(
        posture.status,
        PostureStatus::Stale | PostureStatus::NeverRun
    ) {
        out.push_str("\nRun `sovereign drift detect` to refresh.\n");
    }

    out.push('\n');
    out
}

/// Render an age delta as a short human string. Used by the drift
/// posture section header so "stale · last run 2d ago" beats
/// "stale · last run 172800s ago" for readability.
fn humanize_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

/// Surface architectural commitments (inquiries) that target a file
/// in the working set. Reuses the eval framework's inquiry loader +
/// glob matcher so principles-as-inquiries stays one source of truth.
#[cfg(not(feature = "dev-tools"))]
fn render_principles(_inquiries_dir: &Path, _working_set: &[PathBuf], _remaining: usize) -> String {
    String::new()
}
#[cfg(feature = "dev-tools")]
fn render_principles(inquiries_dir: &Path, working_set: &[PathBuf], remaining: usize) -> String {
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
    use corpus_engine_notes::{NoteScope, ScopeFilter};
    // `reflection` joins decision + invariant so session-end captures
    // (written by `sovereign code reflect`) surface in the next
    // session's brief automatically — closing the feedback loop.
    let kinds: Vec<String> = vec!["decision".into(), "invariant".into(), "reflection".into()];
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
        if row.files.is_empty() || row.files.iter().any(|f| ws_set.contains(f)) {
            kept.push(row);
        }
    }
    // Dedup by content — mesh gossip can hold the same global note many
    // times over; a boot brief that shows one decision 12× buries every
    // other note under the budget (observed live 2026-07-23).
    let mut seen_content = std::collections::HashSet::new();
    kept.retain(|row| seen_content.insert(row.content.trim().to_string()));
    if kept.is_empty() {
        return Ok(String::new());
    }

    let mut out = String::from("## Stated about this area\n\n");
    let mut spent = estimate_tokens(&out);
    for row in kept.iter().take(15) {
        // Author is compact here (bare name, `⟵peer` marker) because the
        // brief is token-budgeted; the full "(this machine)" phrasing
        // rides the `notes` tool. Both come off the same resolved
        // attribution, so they cannot disagree about self-vs-peer.
        // `marker_compact`, NOT `label_compact`: the marker is suppressed
        // unless it would change what the reader does. Rendering it
        // unconditionally is what shipped in a8e10be6, and because
        // `attribution()` returns `Unknown` for every row when no roster
        // is wired, the roster-less CLI brief path printed `_?_` on 100%
        // of lines — the feature inverted. See NodeAttribution::marker_compact.
        let author = notes
            .attribution(row.origin_node_id.as_deref())
            .marker_compact()
            .map(|m| format!(" _{m}_"))
            .unwrap_or_default();
        let line = format!(
            "- **[{}]**{} {}\n",
            row.kind,
            author,
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
    let ws_set: std::collections::HashSet<&Path> =
        working_set.iter().map(|p| p.as_path()).collect();
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

#[cfg(not(feature = "dev-tools"))]
fn render_recent_activity(
    _repo_root: &Path,
    _working_set: &[PathBuf],
    _days: i64,
    _remaining: usize,
) -> Result<String, BriefError> {
    Ok(String::new())
}
#[cfg(feature = "dev-tools")]
fn render_recent_activity(
    repo_root: &Path,
    working_set: &[PathBuf],
    days: i64,
    remaining: usize,
) -> Result<String, BriefError> {
    let history = batch_harvest_all_commits(repo_root)
        .map_err(|e| BriefError::Io(std::io::Error::other(e.to_string())))?;
    let cutoff = chrono::Utc::now().timestamp() - days * 86_400;

    // Collect (timestamp, hash, subject, file) triples for working-set
    // files within the cutoff window. Dedup by hash so a single commit
    // touching N working-set files only renders once.
    let ws_set: std::collections::HashSet<&Path> =
        working_set.iter().map(|p| p.as_path()).collect();
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
    // Was an eleventh copy of the kind-label + display-name fan-out. The Asset
    // arm previously read `asset:<sha12>`; it now matches every other surface
    // (`<kind> asset <sha12>`), which is the point of having one decider.
    (atom.atom_type().label(), atom.display_name(Some(60)))
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
    use corpus_engine_notes::RosterEntry;
    use std::process::Command as Cmd;

    fn init_repo(dir: &Path) {
        assert!(Cmd::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        for (k, v) in [("user.email", "alice@example.com"), ("user.name", "A")] {
            Cmd::new("git")
                .args(["config", k, v])
                .current_dir(dir)
                .status()
                .unwrap();
        }
    }

    fn write_and_commit(dir: &Path, rel: &str, body: &str, msg: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        Cmd::new("git")
            .args(["add", rel])
            .current_dir(dir)
            .status()
            .unwrap();
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
            drift_dir: None,
            work_in_flight: &[],
        };
        let brief = assemble_brief(inputs, &notes).await.unwrap();
        assert!(brief.contains("# Project context: test @ main"));
        assert!(brief.contains("## Working set"));
        assert!(brief.contains("No files in scope"));
        // Not present: structural / recent (no atlas + no repo).
        assert!(!brief.contains("## Structurally observed"));
        assert!(!brief.contains("## Recent activity"));
    }

    #[test]
    fn work_in_flight_empty_renders_nothing() {
        assert_eq!(render_work_in_flight(&[], None), "");
    }

    /// The reported failure this whole change exists to fix: an agent
    /// read "holding the GPU" off a peer's claim, could not tell whose
    /// box it was about, and routed around a constraint that was never
    /// its own. With a roster the two claims are visibly different.
    #[test]
    fn work_in_flight_names_nodes_and_marks_peers_when_roster_is_wired() {
        let roster = NodeRoster::new(
            Some(RosterEntry {
                id_hex: "44ae76142b0c3c723051ff98f043104a".to_string(),
                name: "RuggedFox".to_string(),
            }),
            vec![RosterEntry {
                id_hex: "b88252e4325bc377465f5109ab3d7712".to_string(),
                name: "BeefyMac".to_string(),
            }],
        );
        let entries = vec![
            WorkInFlightEntry {
                scope: "daemon-runtime:9741-primary-slot".to_string(),
                grade: "declared".to_string(),
                detail: "claim: heavy GPU load, holding the slot".to_string(),
                // The truncated Display form notes and claims actually carry.
                node: Some("node-b88252e4325bc377".to_string()),
            },
            WorkInFlightEntry {
                scope: "src/mine.rs".to_string(),
                grade: "active".to_string(),
                detail: "edited 2m ago".to_string(),
                node: Some("node-44ae76142b0c3c72".to_string()),
            },
        ];
        let out = render_work_in_flight(&entries, Some(&roster));
        // Someone else's GPU claim is marked as someone else's.
        assert!(
            out.contains("(BeefyMac⟵peer)"),
            "peer claim must name the peer and mark it: {out}"
        );
        // Our own edit is named without the peer marker.
        assert!(
            out.contains("(RuggedFox)") && !out.contains("(RuggedFox⟵peer)"),
            "self entry must not be marked as a peer: {out}"
        );
        // And the opaque hash form is gone entirely.
        assert!(!out.contains("(node "), "no raw node ids remain: {out}");
    }

    /// Build a note store whose writes are stamped with `origin`, and
    /// whose roster calls `44ae…` this machine and `b882…` BeefyMac.
    async fn store_writing_as(origin: &str) -> (tempfile::TempDir, NoteStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();
        store.set_origin_node_id(origin).unwrap();
        store
            .set_node_roster(NodeRoster::new(
                Some(RosterEntry {
                    id_hex: "44ae76142b0c3c723051ff98f043104a".to_string(),
                    name: "RuggedFox".to_string(),
                }),
                vec![RosterEntry {
                    id_hex: "b88252e4325bc377465f5109ab3d7712".to_string(),
                    name: "BeefyMac".to_string(),
                }],
            ))
            .unwrap();
        // The TempDir must outlive the store — returning it keeps the db
        // file alive for the caller's assertions.
        (dir, store)
    }

    /// The suppression added after a8e10be6 must not swallow the case the
    /// whole feature exists for. With a roster wired, a peer's note still
    /// announces itself — otherwise "quiet" would have been bought by
    /// deleting the signal rather than the noise.
    #[tokio::test]
    async fn peer_authored_note_is_still_marked_when_a_roster_is_wired() {
        let (_dir, store) = store_writing_as("node-b88252e4325bc377").await;
        store
            .write_note(
                "decision",
                "Holding the primary slot for a long enrichment run.",
                vec![],
                vec![],
                "s1",
            )
            .await
            .unwrap();

        let out = render_notes(&store, None, &[], 4000).await.unwrap();
        assert!(
            out.contains("_BeefyMac⟵peer_"),
            "a peer's note must name the peer: {out}"
        );
    }

    /// The other side of the same rule: a note written HERE carries no
    /// marker. The boot hook already told the session which machine it is,
    /// so a per-line marker on the common case is pure token cost.
    #[tokio::test]
    async fn locally_authored_note_carries_no_marker() {
        let (_dir, store) = store_writing_as("node-44ae76142b0c3c72").await;
        store
            .write_note("decision", "A local decision.", vec![], vec![], "s1")
            .await
            .unwrap();

        let out = render_notes(&store, None, &[], 4000).await.unwrap();
        assert!(out.contains("A local decision."), "note missing: {out}");
        assert!(
            !out.contains("_RuggedFox_") && !out.contains('?'),
            "a local note must render unmarked: {out}"
        );
    }

    /// THE SHIPPED REGRESSION, pinned at the surface that showed it.
    /// `sovereign code brief` opens a bare store: `sovereign-cli` cannot
    /// depend on `sovereign-mesh`, so no roster is ever wired there and
    /// `attribution()` returns `Unknown` for every row — including
    /// peer-written ones whose origin is populated and correct. a8e10be6
    /// rendered that as `_?_` on 100% of note lines (measured 2026-08-07,
    /// 15 of 15). A marker on every line names nothing.
    #[tokio::test]
    async fn roster_less_brief_renders_no_markers_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();
        // Origin IS stamped — this is not a missing-provenance case. The
        // store simply has no roster to name it with.
        store.set_origin_node_id("node-b88252e4325bc377").unwrap();
        for i in 0..3 {
            store
                .write_note("decision", &format!("Decision {i}."), vec![], vec![], "s1")
                .await
                .unwrap();
        }

        let out = render_notes(&store, None, &[], 4000).await.unwrap();
        assert_eq!(
            out.matches("- **[").count(),
            3,
            "expected all three notes to render: {out}"
        );
        assert!(
            !out.contains('?'),
            "an unnameable origin must render nothing, not `?`: {out}"
        );
    }

    /// A truncated id that matches two members is reported, not guessed.
    /// Guessing here would attribute a peer's machine-state claim to the
    /// local box, which is the failure mode inverted.
    #[test]
    fn ambiguous_node_prefix_is_reported_rather_than_resolved() {
        let roster = NodeRoster::new(
            None,
            vec![
                RosterEntry {
                    id_hex: "aaaaaaaaaaaaaaaa1111111111111111".to_string(),
                    name: "One".to_string(),
                },
                RosterEntry {
                    id_hex: "aaaaaaaaaaaaaaaa2222222222222222".to_string(),
                    name: "Two".to_string(),
                },
            ],
        );
        let attribution = roster.resolve("node-aaaaaaaaaaaaaaaa");
        assert_eq!(attribution.as_str(), "ambiguous");
        assert!(!attribution.is_this_machine());
        assert!(attribution.label().contains("One, Two"), "{attribution:?}");
    }

    #[test]
    fn work_in_flight_lists_entries_and_caps_at_8() {
        let mk = |i: usize, grade: &str| WorkInFlightEntry {
            scope: format!("src/file_{i}.rs"),
            grade: grade.to_string(),
            detail: if grade == "declared" {
                "claim: refactor the ingest path".to_string()
            } else {
                format!("edited {i}m ago · 2 event(s)")
            },
            node: Some("abcdef1234567890".to_string()),
        };
        let entries: Vec<WorkInFlightEntry> = (0..10)
            .map(|i| mk(i, if i == 9 { "active" } else { "declared" }))
            .collect();
        let out = render_work_in_flight(&entries, None);
        assert!(out.contains("## Work in flight (peers)"));
        // No roster wired ⇒ raw id, shortened to 8 chars. We fall back to
        // the unhelpful-but-true form rather than inventing a name.
        assert!(out.contains("(node abcdef12…)"));
        assert!(out.contains("[declared] `src/file_0.rs` — claim: refactor the ingest path"));
        // 10 entries → 8 rendered + overflow marker. The renderer
        // trusts the caller's ordering (OverlapAccumulator sorts).
        assert_eq!(out.matches("\n- [").count(), 8);
        assert!(out.contains("_+2 more_"));
        assert!(out.contains("work_in_flight(scope, match_mode)"));
    }

    #[tokio::test]
    async fn working_set_lists_files_capped_at_20() {
        let tmp = tempfile::tempdir().unwrap();
        let notes = NoteStore::open(&tmp.path().join("notes.db")).unwrap();
        let working_set: Vec<PathBuf> =
            (0..25).map(|i| PathBuf::from(format!("f{i}.rs"))).collect();
        let inputs = BriefInputs {
            working_set: &working_set,
            repo_root: None,
            atlas_dir: None,
            inquiries_dir: None,
            repo_name: "test",
            branch_name: "main",
            budget_tokens: 4000,
            feature_id: None,
            drift_dir: None,
            work_in_flight: &[],
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
        Cmd::new("git")
            .args(["add", "old.rs"])
            .current_dir(repo)
            .status()
            .unwrap();
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
            drift_dir: None,
            work_in_flight: &[],
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
        let working_set: Vec<PathBuf> = (0..100)
            .map(|i| PathBuf::from(format!("file{i}.rs")))
            .collect();
        let inputs = BriefInputs {
            working_set: &working_set,
            repo_root: None,
            atlas_dir: None,
            inquiries_dir: None,
            repo_name: "test",
            branch_name: "main",
            budget_tokens: 50,
            feature_id: None,
            drift_dir: None,
            work_in_flight: &[],
        };
        let brief = assemble_brief(inputs, &notes).await.unwrap();
        let cost = estimate_tokens(&brief);
        assert!(cost <= 50, "budget overrun: {cost} > 50");
    }
}
