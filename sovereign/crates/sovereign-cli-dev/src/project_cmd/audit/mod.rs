// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project audit` — the reviewer-ready rollup.
//!
//! A read-only scan that assembles one markdown page a reviewer would
//! otherwise piece together by hand: lifecycle state, phase progression,
//! notes grouped by kind + source priority, drift status, the feature
//! table, and red-team findings. Split out of `project_cmd` (2026-07-13)
//! for legibility; a pure move, no behaviour change. Shared helpers
//! (`find_repo_root`, `derive_project_id`, `today_iso`) resolve through
//! `use super::*`.

use super::*;

// ─── sovereign project audit (M7.3) ──────────────────────────
//
// Reviewer-ready rollup. Read-only scan of everything a reviewer
// would otherwise have to assemble by hand: the lifecycle state,
// phase progression, notes grouped by kind, drift status, feature
// summary, red-team findings. One markdown page printed to stdout
// so it can be piped to a file, a PR description, or a GitHub
// issue.

pub(crate) async fn cmd_audit(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("svrn project audit");
        println!();
        println!("Prints a reviewer-ready rollup of project state to stdout.");
        println!("Pipe it to a file or a PR description: `svrn project audit > audit.md`.");
        return 0;
    }
    let repo_root = find_repo_root()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let sov = repo_root.join(".sovereign");
    let project_toml_path = sov.join("project.toml");
    if !project_toml_path.exists() {
        eprintln!(
            "  sovereign project audit: no .sovereign/project.toml found. \
             Run `svrn project init` first."
        );
        return 1;
    }
    let project_toml = match crate::project_toml::ProjectTomlFile::read(&project_toml_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  sovereign project audit: cannot read project.toml: {e}");
            return 1;
        }
    };

    // Phase 7.3 gap E: run the LLM-backed extraction pass before
    // the report renders, so any new `source='extracted'` notes
    // land in the same audit. Best-effort — head-equality
    // short-circuit makes repeated runs cheap; backend-availability
    // failures skip cleanly without affecting the surrounding
    // audit. Output goes to stderr so the markdown report on
    // stdout isn't polluted.
    let notes_db = sov.join("notes.db");
    if notes_db.exists() {
        if let Ok(store) = corpus_engine_notes::NoteStore::open(&notes_db) {
            let summary = crate::audit_extract::run_with_default_backend(&repo_root, &store).await;
            if summary.ran && summary.written > 0 {
                eprintln!(
                    "  audit: extracted {} new decision{} from diff at HEAD {}",
                    summary.written,
                    if summary.written == 1 { "" } else { "s" },
                    summary
                        .head
                        .as_deref()
                        .map(|h| h.chars().take(8).collect::<String>())
                        .unwrap_or_default(),
                );
            } else if let Some(reason) = summary.skip_reason {
                tracing::debug!(reason, "audit_extract: skipped");
            }
        }
    }

    let report = build_audit_report(&repo_root, &project_toml).await;
    println!("{report}");
    0
}

/// Compose the audit markdown. Pure with respect to the filesystem
/// + DBs (reads only, no writes) so tests can drive it with a
/// seeded repo and assert the output directly.
async fn build_audit_report(
    repo_root: &Path,
    project_toml: &crate::project_toml::ProjectTomlFile,
) -> String {
    let sov = repo_root.join(".sovereign");
    let project_id = derive_project_id(repo_root);
    let now = today_iso();

    let mut out = String::new();
    out.push_str(&format!("# {project_id} — Project audit\n\n"));
    out.push_str(&format!("_Generated: {now}._\n\n"));

    // ── Lifecycle ──────────────────────────────────────────────
    out.push_str("## Lifecycle\n\n");
    out.push_str(&format!(
        "- **Founded:** {}\n- **Charter version:** {}\n- **Current phase:** {}\n",
        if project_toml.lifecycle.founded {
            "yes"
        } else {
            "no"
        },
        project_toml.lifecycle.charter_version,
        project_toml.lifecycle.current_phase,
    ));
    // Charter drift detection — recompute vs. recorded hash.
    let charter_path = sov.join("CHARTER.md");
    let drift_line = if charter_path.exists() && !project_toml.lifecycle.charter_hash.is_empty() {
        match std::fs::read_to_string(&charter_path) {
            Ok(text) => {
                let current = crate::found::hash_charter(&text);
                if current == project_toml.lifecycle.charter_hash {
                    "- **Charter drift:** none (on-disk CHARTER.md matches recorded hash)".into()
                } else {
                    format!(
                        "- **Charter drift:** ⚠ on-disk hash `{}` differs from recorded `{}`",
                        &current[..8.min(current.len())],
                        &project_toml.lifecycle.charter_hash
                            [..8.min(project_toml.lifecycle.charter_hash.len())]
                    )
                }
            }
            Err(_) => "- **Charter drift:** unknown (CHARTER.md unreadable)".into(),
        }
    } else if !charter_path.exists() {
        "- **Charter drift:** n/a (no CHARTER.md — project not founded)".into()
    } else {
        "- **Charter drift:** n/a (no recorded hash)".into()
    };
    out.push_str(&drift_line);
    out.push_str("\n\n");

    // ── Phases ─────────────────────────────────────────────────
    let phases_path = crate::phases::phases_md_path(repo_root);
    out.push_str("## Phases\n\n");
    if phases_path.exists() {
        let md = std::fs::read_to_string(&phases_path).unwrap_or_default();
        let phases = crate::phases::parse_phases(&md);
        if phases.is_empty() {
            out.push_str("_(PHASES.md exists but no phases parsed.)_\n\n");
        } else {
            out.push_str("| Phase | Status | Stop condition |\n|---|---|---|\n");
            for p in &phases {
                let status = if p.deferred {
                    "deferred".into()
                } else if p.ordinal < project_toml.lifecycle.current_phase {
                    let artifact = crate::phases::phase_report_path(repo_root, p.ordinal);
                    let verdict = read_phase_verdict(&artifact).unwrap_or("passed".into());
                    format!("{verdict} → `{}`", relative(&artifact, repo_root))
                } else if p.ordinal == project_toml.lifecycle.current_phase {
                    "current".into()
                } else {
                    "not yet".into()
                };
                let stop = if p.stop_text.is_empty() {
                    "_(none)_".into()
                } else {
                    p.stop_text.replace('|', "\\|")
                };
                out.push_str(&format!("| {} | {status} | {stop} |\n", p.heading));
            }
            out.push('\n');
        }
    } else {
        out.push_str("_(no PHASES.md — project not founded)_\n\n");
    }

    // ── Phase 7.3: multi-source audit sections ─────────────────
    //
    // The audit is the deliverable. The "non-empty floor" contract
    // says any session that did real work produces something
    // here, even if the agent never explicitly called `note(...)`
    // — that floor is held up by the four extraction streams
    // (agent / committed / extracted / inferred / observed).
    //
    // Layout (per spec §2.7):
    //
    //   ## Decisions       — kind=decision|invariant, sorted by
    //                        (source priority desc, created_at desc).
    //                        Reversal lines render under their
    //                        original via `supersedes`.
    //   ## Deviations      — kind=deviation. Source-tagged.
    //   ## Open questions  — kind=uncertainty. Source-tagged;
    //                        inferred-source rows lower-emphasised.
    //   ## Observed patterns — source=observed (any kind).
    //   ## Notes by kind   — kept for backward compatibility with
    //                        readers used to the old layout.
    let notes_db = sov.join("notes.db");
    let audit_notes: AuditNotes = if notes_db.exists() {
        match corpus_engine_notes::NoteStore::open(&notes_db) {
            Ok(store) => gather_audit_notes(&store).await,
            Err(_) => AuditNotes::default(),
        }
    } else {
        AuditNotes::default()
    };

    out.push_str(&render_decisions(&audit_notes));
    out.push_str(&render_deviations(&audit_notes));
    out.push_str(&render_open_questions(&audit_notes));
    out.push_str(&render_observed_patterns(&audit_notes));

    // Legacy "Notes by kind" count table — kept so reviewers used
    // to it don't notice a regression. Empty case still renders
    // the placeholder so empty audits stay consistent.
    out.push_str("## Notes by kind\n\n");
    if audit_notes.counts.is_empty() {
        out.push_str("_(no notes recorded)_\n\n");
    } else {
        out.push_str("| Kind | Count |\n|---|---|\n");
        for (kind, count) in &audit_notes.counts {
            out.push_str(&format!("| {kind} | {count} |\n"));
        }
        out.push('\n');
    }

    // ── Features ───────────────────────────────────────────────
    //
    // Phase 6: enumerate features from BOTH sources and merge by
    // id, so a feature with just a committed `spec.md` (no
    // `provision` step) shows up alongside features that were
    // explicitly seeded into `features.db`.
    //
    //   - `.sovereign/features/<id>/` directories on disk → "spec
    //     present" / "no spec yet" depending on whether `spec.md`
    //     exists. Source of truth for the new flat-namespace flow.
    //   - `features.db` rows → state machine (active/archived) and
    //     auto-redteam preference. Still useful for projects that
    //     ran `svrn atos provision`, but no longer required.
    //
    // Both sources are merged on `id`. A directory-only feature
    // shows `state = "(directory only)"`; a db-only feature
    // (provisioned but never had its spec written) shows
    // `state = <db state>` + a missing-spec note.
    out.push_str("## Features\n\n");
    let feature_rows = collect_feature_rows(&sov).await;
    if feature_rows.is_empty() {
        out.push_str(
            "_(no features yet — write a spec at \
            `.sovereign/features/<id>/spec.md` and commit it)_\n\n",
        );
    } else {
        out.push_str("| Feature | State | Spec | Auto red-team |\n|---|---|---|---|\n");
        for row in &feature_rows {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                row.id,
                row.state,
                if row.spec_present { "✓" } else { "missing" },
                if row.auto_redteam { "yes" } else { "no" },
            ));
        }
        out.push('\n');
    }

    // ── Artifact inventory ─────────────────────────────────────
    out.push_str("## Artifact inventory\n\n");
    let artifacts = collect_artifact_inventory(&sov);
    if artifacts.is_empty() {
        out.push_str("_(no ATOS artifacts found)_\n\n");
    } else {
        for a in &artifacts {
            out.push_str(&format!("- `{a}`\n"));
        }
        out.push('\n');
    }

    // ── Footer ────────────────────────────────────────────────
    out.push_str("---\n\n");
    out.push_str(
        "_Generated by `svrn project audit`. Re-run to refresh; this document is not committed automatically._\n",
    );

    // ── Publish-recipe nudge (Phase 7) ─────────────────────────
    // Fires once when a user-authored recipe has been driven to
    // findings via `svrn enrich investigation build`. The
    // condition gate suppresses the nudge for already-published
    // and explicitly-dismissed entries; it costs ~one filesystem
    // read per investigation corpus and never fires for recipes
    // sourced from the upstream registry.
    if let Some(nudge) = compose_publish_recipe_nudge() {
        out.push('\n');
        out.push_str(&nudge);
    }
    out
}

/// Compose a publish-recipe nudge if any investigation pipeline has
/// produced findings on a user-authored (local) recipe that hasn't
/// been published yet AND hasn't been explicitly dismissed.
/// Returns `None` when nothing to nudge about.
///
/// The nudge text is formatted as a horizontal-rule-delimited
/// blockquote so it survives copy-paste into a PR description or
/// gets ignored when piped to a tool that strips trailing
/// whitespace.
fn compose_publish_recipe_nudge() -> Option<String> {
    use corpus_engine::RecipeRegistry;

    // Resolve the local recipes dir + the indexes dir. Bail
    // silently when HOME isn't set — the nudge is best-effort.
    let local_recipes_dir = RecipeRegistry::default_local_recipes_dir()?;
    let indexes_dir = sovereign_cli_shared::dirs::sovereign_indexes();
    if !indexes_dir.is_dir() {
        return None;
    }

    // Build the registry once so `is_local_entry` is cheap to call.
    let mut registry = RecipeRegistry::from_bundled(Some(local_recipes_dir.clone()));
    registry = registry.with_local_registry(&local_recipes_dir.join("registry.toml"));

    // Read the publish + dismissal markers.
    let sovereign_root = sovereign_cli_shared::dirs::sovereign_root();
    let published: std::collections::BTreeMap<String, serde_json::Value> =
        std::fs::read_to_string(sovereign_root.join("published_recipes.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
    let dismissed: Vec<String> =
        std::fs::read_to_string(sovereign_root.join("dismissed_nudges.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
    if dismissed.iter().any(|d| d == "recipe-publish") {
        return None;
    }

    // Walk every installed corpus and look for an
    // `investigation/pattern_findings.json` with at least one row.
    let mut candidates: Vec<(String, usize)> = Vec::new();
    let entries = std::fs::read_dir(&indexes_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let corpus_id = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let findings_path = path.join("investigation").join("pattern_findings.json");
        if !findings_path.is_file() {
            continue;
        }
        let raw = match std::fs::read_to_string(&findings_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let arr: Vec<serde_json::Value> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if arr.is_empty() {
            continue;
        }
        // Filters: local recipe, not published, not dismissed-by-id.
        if !registry.is_local_entry(&corpus_id) {
            continue;
        }
        if published.contains_key(&corpus_id) {
            continue;
        }
        if dismissed
            .iter()
            .any(|d| d == &format!("recipe-publish:{corpus_id}"))
        {
            continue;
        }
        candidates.push((corpus_id, arr.len()));
    }

    if candidates.is_empty() {
        return None;
    }

    let mut nudge = String::new();
    nudge.push_str("\n---\n\n");
    nudge.push_str("## 💡 Share your recipe\n\n");
    if candidates.len() == 1 {
        let (id, n) = &candidates[0];
        nudge.push_str(&format!(
            "Your locally-authored recipe `{id}` produced {n} pattern finding(s) \
             during this audit. Publishing it would let others run the same \
             investigation without rebuilding the recipe from scratch.\n\n"
        ));
        nudge.push_str(&format!(
            "  sovereign recipe publish ~/.sovereign/recipes/{id}/recipe.toml\n\n"
        ));
    } else {
        nudge.push_str(
            "These locally-authored recipes produced findings during this audit. \
             Publishing each lets others run the same investigations.\n\n",
        );
        for (id, n) in &candidates {
            nudge.push_str(&format!("- `{id}` ({n} finding(s))\n"));
        }
        nudge.push('\n');
        nudge.push_str("  sovereign recipe publish ~/.sovereign/recipes/<id>/recipe.toml\n\n");
    }
    nudge.push_str(
        "_Shown once. Dismiss forever: `svrn nudge dismiss recipe-publish` \
         — or per-recipe: `svrn nudge dismiss recipe-publish:<id>`._\n",
    );
    Some(nudge)
}

/// Scan a phase-N.md artifact for its verdict line. Defensive —
/// if the header line doesn't match the expected shape, returns
/// `None` and the caller falls back to a generic label.
fn read_phase_verdict(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let first = text.lines().next()?;
    if first.contains("PASSED") {
        Some("✓ passed".into())
    } else if first.contains("FAILED") {
        Some("✗ failed".into())
    } else {
        None
    }
}

/// Count notes by kind and pull up to 10 open-question + 10
/// deviation notes. Uses the broad Global + Feature scope
/// combined — auditor wants the whole picture.
/// Phase 7.3: everything the audit's note-derived sections need.
/// Built once per audit run by [`gather_audit_notes`]; the
/// section renderers are pure with respect to this struct.
#[derive(Debug, Default)]
struct AuditNotes {
    /// `kind=decision` or `kind=invariant`, sorted by
    /// (source priority desc, created_at desc). Reversal lines
    /// follow their originals — see [`render_decisions`].
    decisions: Vec<corpus_engine_notes::NoteRow>,
    /// `kind=deviation`. Source-tagged in the renderer.
    deviations: Vec<corpus_engine_notes::NoteRow>,
    /// `kind=uncertainty`. The renderer de-emphasises
    /// `source=inferred` rows since they're the lowest-confidence
    /// stream.
    open_questions: Vec<corpus_engine_notes::NoteRow>,
    /// `source=observed` (any kind). These are the audit's
    /// "the agent did X but didn't say so" stream from the Phase
    /// 7.1 ToolPatternMatcher.
    observed: Vec<corpus_engine_notes::NoteRow>,
    /// All notes, indexed by id. Used by the renderer to look up
    /// the row a `supersedes` link points at.
    by_id: std::collections::HashMap<String, corpus_engine_notes::NoteRow>,
    /// Total count per kind across all sources. Powers the
    /// legacy "Notes by kind" table.
    counts: std::collections::BTreeMap<String, u32>,
}

/// Read every active note in the store and bucket it by the
/// audit's section needs. Decisions are sorted by source priority
/// (agent > committed > extracted > inferred > observed) then
/// reverse chronological. Read-once, render-many — the renderers
/// don't touch the DB.
async fn gather_audit_notes(store: &corpus_engine_notes::NoteStore) -> AuditNotes {
    let filter = corpus_engine_notes::ScopeFilter {
        scopes: vec![
            corpus_engine_notes::NoteScope::Global,
            corpus_engine_notes::NoteScope::Feature,
        ],
        feature_id: None,
    };
    let rows = store
        .read_notes_scoped(None, &[], &[], &[], 1000, false, &filter)
        .await
        .unwrap_or_default();

    let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
    let mut decisions = Vec::new();
    let mut deviations = Vec::new();
    let mut open_questions = Vec::new();
    let mut observed = Vec::new();
    let mut by_id = std::collections::HashMap::new();

    for n in &rows {
        *counts.entry(n.kind.clone()).or_insert(0) += 1;
        if n.kind == "decision" || n.kind == "invariant" {
            decisions.push(n.clone());
        }
        if n.kind == "deviation" {
            deviations.push(n.clone());
        }
        if n.kind == "uncertainty" {
            open_questions.push(n.clone());
        }
        if n.source == corpus_engine_notes::NoteSource::Observed.as_str() {
            observed.push(n.clone());
        }
        by_id.insert(n.id.clone(), n.clone());
    }

    // Decisions get the multi-source priority sort. Higher
    // priority first; tie-broken by recency. The audit reader
    // sees agent-written decisions above extracted/inferred ones
    // — the trust ordering matches the eye flow.
    decisions.sort_by(|a, b| {
        let pa = source_priority(&a.source);
        let pb = source_priority(&b.source);
        pb.cmp(&pa).then_with(|| b.created_at.cmp(&a.created_at))
    });
    deviations.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    open_questions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    observed.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    AuditNotes {
        decisions,
        deviations,
        open_questions,
        observed,
        by_id,
        counts,
    }
}

/// Lookup helper for the audit sort. Maps the string-form source
/// (which is what's in the DB row) back to the priority number.
/// Unknown / pre-v6 strings are treated as lowest priority so a
/// stale row never accidentally floats above the agent's own.
fn source_priority(s: &str) -> u8 {
    match corpus_engine_notes::NoteSource::parse(s) {
        Some(src) => src.priority(),
        None => 0,
    }
}

/// Render a one-line summary of `note` suitable for the audit's
/// bullet lists. First line of the body, truncated to a
/// reasonable cap, with a `[<source>]` suffix the reviewer can
/// scan to gauge confidence at a glance.
fn render_audit_line(note: &corpus_engine_notes::NoteRow) -> String {
    let first_line: String = note
        .content
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(160)
        .collect();
    format!(
        "- `[note:{}]` {} _[{}]_",
        note.id,
        first_line.trim_end(),
        note.source
    )
}

/// Phase 7.3 Decisions section. Walks `notes.decisions` once;
/// for each top-level note (one without a `supersedes` link) it
/// renders the line and any subsequent reversal. The reverse
/// shows up indented under the original so the audit reader sees
/// the chain.
fn render_decisions(notes: &AuditNotes) -> String {
    let mut out = String::new();
    out.push_str("## Decisions\n\n");
    if notes.decisions.is_empty() {
        out.push_str("_(no decisions recorded yet)_\n\n");
        return out;
    }
    // Build the reverse map: original.id → list of supersedes rows.
    let mut supers_of: std::collections::HashMap<String, Vec<&corpus_engine_notes::NoteRow>> =
        std::collections::HashMap::new();
    for n in &notes.decisions {
        if let Some(orig_id) = n.supersedes.as_ref() {
            supers_of.entry(orig_id.clone()).or_default().push(n);
        }
    }
    // Sort each reversal chain by created_at ascending so the
    // earliest reversal renders directly under the original.
    for chain in supers_of.values_mut() {
        chain.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    }

    let mut already_rendered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for n in &notes.decisions {
        // Skip rows that are themselves reversals — they'll be
        // rendered under the originals below.
        if n.supersedes.is_some() {
            // ...unless the original isn't in our visible set
            // (e.g. retired separately). In that case render it
            // standalone so the reviewer doesn't lose the entry.
            let orphan = n
                .supersedes
                .as_ref()
                .map(|id| !notes.by_id.contains_key(id))
                .unwrap_or(true);
            if !orphan {
                continue;
            }
        }
        if already_rendered.contains(&n.id) {
            continue;
        }
        out.push_str(&render_audit_line(n));
        out.push('\n');
        already_rendered.insert(n.id.clone());
        if let Some(reversals) = supers_of.get(&n.id) {
            for r in reversals {
                out.push_str(&format!(
                    "  ↳ REVERSED {}: {} _[{}]_\n",
                    short_date(&r.created_at),
                    r.content
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(140)
                        .collect::<String>()
                        .trim_end(),
                    r.source
                ));
                already_rendered.insert(r.id.clone());
            }
        }
    }
    out.push('\n');
    out
}

/// Phase 7.3 Deviations section. Sorted reverse-chronological;
/// each row carries a `[<source>]` suffix so the reviewer can
/// distinguish drift the agent acknowledged (`agent`) from drift
/// surfaced via spec-hash comparison (`extracted` / `inferred`).
fn render_deviations(notes: &AuditNotes) -> String {
    let mut out = String::new();
    if notes.deviations.is_empty() {
        return out;
    }
    out.push_str("## Deviations\n\n");
    for n in &notes.deviations {
        out.push_str(&render_audit_line(n));
        out.push('\n');
    }
    out.push('\n');
    out
}

/// Phase 7.3 Open questions section. `kind=uncertainty`. The
/// renderer marks `source=inferred` rows with a "(low confidence)"
/// suffix because regex-mining over assistant prose is noisier
/// than the agent's own `note(uncertainty, …)`.
fn render_open_questions(notes: &AuditNotes) -> String {
    let mut out = String::new();
    if notes.open_questions.is_empty() {
        return out;
    }
    out.push_str("## Open questions\n\n");
    for n in &notes.open_questions {
        let confidence_suffix = if n.source == corpus_engine_notes::NoteSource::Inferred.as_str() {
            " _(low confidence)_"
        } else {
            ""
        };
        out.push_str(&render_audit_line(n));
        out.push_str(confidence_suffix);
        out.push('\n');
    }
    out.push('\n');
    out
}

/// Phase 7.3 Observed patterns section. Pulls notes tagged
/// `source=observed` from any kind. These are the
/// `ToolPatternMatcher`'s output — workflow-shape signals like
/// "investigated impact before editing" that no human or agent
/// recorded explicitly.
fn render_observed_patterns(notes: &AuditNotes) -> String {
    let mut out = String::new();
    if notes.observed.is_empty() {
        return out;
    }
    out.push_str("## Observed patterns\n\n");
    for n in &notes.observed {
        out.push_str(&render_audit_line(n));
        out.push('\n');
    }
    out.push('\n');
    out
}

/// Format a NoteRow's RFC-3339 created_at as `YYYY-MM-DD`. If the
/// string isn't parseable as RFC-3339, returns the raw column
/// truncated to 10 chars (the date prefix). Audit lines are
/// terse — we don't render the full timestamp.
fn short_date(rfc3339: &str) -> String {
    // First 10 chars of an RFC-3339 timestamp is "YYYY-MM-DD".
    rfc3339.chars().take(10).collect()
}

/// One audit row in the Features table. Merges what we know from
/// `features.db` (lifecycle state, redteam preference) with what we
/// see on disk (`.sovereign/features/<id>/spec.md`). A row exists
/// if either source has the feature.
struct FeatureRow {
    id: String,
    /// "(directory only)" when the feature is on disk but absent
    /// from features.db; the db state ("active", "archived", …)
    /// otherwise. Phase 6: directory-only is the new default —
    /// users no longer need to run `svrn atos provision` to
    /// have a feature exist for the audit.
    state: String,
    /// True iff `<id>/spec.md` is present at the canonical path.
    spec_present: bool,
    /// Mirrors `FeatureRow.auto_redteam` from the db. Defaults to
    /// false for directory-only entries.
    auto_redteam: bool,
}

/// Enumerate features from both sources (db + on-disk directories)
/// and return one merged row per id, sorted alphabetically. Result
/// is empty when neither source has any features — the audit
/// renders that as a one-line "_(no features yet)_" note rather
/// than a header-less table.
async fn collect_feature_rows(sov: &Path) -> Vec<FeatureRow> {
    use std::collections::BTreeMap;

    let mut by_id: BTreeMap<String, FeatureRow> = BTreeMap::new();

    // Source A: `.sovereign/features/<id>/` directories.
    let features_dir = sov.join("features");
    if let Ok(entries) = std::fs::read_dir(&features_dir) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(String::from) else {
                continue;
            };
            let spec_present = entry.path().join("spec.md").is_file();
            by_id.insert(
                id.clone(),
                FeatureRow {
                    id,
                    state: "(directory only)".into(),
                    spec_present,
                    auto_redteam: false,
                },
            );
        }
    }

    // Source B: `features.db` rows. Where ids overlap, the db row
    // "wins" for state + auto_redteam; spec_present is taken from
    // the directory walk (we don't trust the db to know whether
    // spec.md was actually written).
    let features_db = sov.join("features.db");
    if features_db.exists() {
        if let Ok(store) = corpus_engine_atos::FeatureStore::open(&features_db) {
            if let Ok(features) = store.list(true).await {
                for f in features {
                    let spec_present = features_dir.join(&f.id).join("spec.md").is_file();
                    by_id.insert(
                        f.id.clone(),
                        FeatureRow {
                            id: f.id,
                            state: f.state.to_string(),
                            spec_present,
                            auto_redteam: f.auto_redteam,
                        },
                    );
                }
            }
        }
    }

    by_id.into_values().collect()
}

fn collect_artifact_inventory(sov: &Path) -> Vec<String> {
    let mut out = Vec::new();
    // Top-level markdown + toml artifacts.
    for name in ["CHARTER.md", "PHASES.md", "project.toml"] {
        if sov.join(name).exists() {
            out.push(format!(".sovereign/{name}"));
        }
    }
    // Phase reports.
    if let Ok(entries) = std::fs::read_dir(sov) {
        let mut phase_names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                (n.starts_with("phase-") && n.ends_with(".md")).then_some(n)
            })
            .collect();
        phase_names.sort();
        for n in phase_names {
            out.push(format!(".sovereign/{n}"));
        }
    }
    // Feature artifacts.
    let feats = sov.join("features");
    if feats.exists() {
        if let Ok(entries) = std::fs::read_dir(&feats) {
            let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            dirs.sort();
            for dir in dirs {
                let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
                let mut items = Vec::new();
                for f in ["spec.md", "red-team.md", "epistemic-report.md"] {
                    if dir.join(f).exists() {
                        items.push(f.to_string());
                    }
                }
                if let Ok(ents) = std::fs::read_dir(&dir) {
                    let mut ms: Vec<String> = ents
                        .filter_map(|e| e.ok())
                        .filter_map(|e| {
                            let n = e.file_name().to_string_lossy().into_owned();
                            (n.starts_with("milestone-") && n.ends_with(".md")).then_some(n)
                        })
                        .collect();
                    ms.sort();
                    items.extend(ms);
                }
                for i in items {
                    out.push(format!(".sovereign/features/{name}/{i}"));
                }
            }
        }
    }
    // Fetched docs.
    let docs = sov.join("docs");
    if docs.exists() {
        if let Ok(entries) = std::fs::read_dir(&docs) {
            let mut fnames: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            fnames.sort();
            for n in fnames {
                out.push(format!(".sovereign/docs/{n}"));
            }
        }
    }
    out
}

fn relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests;
