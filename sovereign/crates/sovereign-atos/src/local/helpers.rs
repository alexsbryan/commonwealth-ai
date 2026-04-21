//! Pure helpers used by [`super::orchestrator::LocalAtosOrchestrator`].
//!
//! Everything here is a deterministic function of its inputs — no
//! clocks, no subprocess spawning, no store mutation. Extracted so
//! each helper can be covered by a unit test that doesn't need a
//! tempdir or a tokio runtime.
//!
//! The helpers group into two themes:
//!
//! 1. **Charter / brief text ops** — parsing the charter heading,
//!    composing per-milestone briefs, extracting the stop-condition
//!    marker that the provisioner embeds, deriving a display title.
//! 2. **Notes digest composition** — the two functions that walk a
//!    [`NoteStore`] to produce preamble blocks. They take an
//!    `&NoteStore` rather than an `&LocalAtosOrchestrator` so they
//!    stay testable without a full orchestrator.
//!
//! The module is `pub(super)` because the helpers are implementation
//! detail of the orchestrator; `feature_dir` and
//! `extract_milestone_stop_condition` are re-exported from
//! [`super`] for external callers that already consume them.
//!
//! Reference: ARCH_PRINCIPLES.md §3 (split a file by concern).

use corpus_engine::{NoteRow, NoteScope, NoteStore, ScopeFilter};

use crate::{Error, Result};

/// Compute the per-feature artifact directory. The renderer writes
/// `milestone-<n>.md` / `red-team.md` / `epistemic-report.md` here.
/// Rooted at `cwd/.sovereign/features/<id>` — matches where
/// `features.db` itself lives so operators find reports next to the
/// features they came from.
pub fn feature_dir(feature_id: &str) -> std::path::PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".sovereign")
        .join("features")
        .join(feature_id)
}

/// Extract the feature id + human title from the charter's first
/// level-1 heading. Accepted forms:
///
/// ```markdown
/// # atos-version-flag — Add `--version` to atos
/// # atos-version-flag -- Add `--version` to atos
/// # atos-version-flag: Add `--version` to atos
/// # atos-version-flag
/// ```
///
/// The id is the slug up to the first separator; the title is
/// whatever follows, or the id itself when the heading is slug-only.
pub(super) fn extract_id_and_title(charter_md: &str) -> Result<(String, String)> {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
    let parser = Parser::new(charter_md);
    let mut in_h1 = false;
    let mut buf = String::new();
    for event in parser {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) => {
                in_h1 = true;
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) => {
                if in_h1 {
                    break;
                }
            }
            Event::Text(t) if in_h1 => buf.push_str(&t),
            Event::Code(c) if in_h1 => buf.push_str(&c),
            _ => {}
        }
    }
    let raw = buf.trim();
    if raw.is_empty() {
        return Err(Error::CharterParse(
            "charter must open with a level-1 heading like `# <id> — <title>`".into(),
        ));
    }
    // Split on the first recognized separator.
    for sep in ["—", "--", ":"] {
        if let Some(idx) = raw.find(sep) {
            let id = raw[..idx].trim().to_string();
            let title = raw[idx + sep.len()..].trim().to_string();
            if !id.is_empty() && !title.is_empty() {
                return Ok((id, title));
            }
        }
    }
    // No separator — treat the whole line as both id and title.
    Ok((raw.to_string(), raw.to_string()))
}

/// Build the per-milestone brief that gets stored in
/// `feature_milestones.brief_md`. Keeps Yara's `### N. Title` header
/// + body intact so the brief reads like a self-contained document
/// when piped into a driver session.
pub(super) fn compose_milestone_brief(spec: &crate::charter::MilestoneSpec) -> String {
    format!(
        "### {}. {}\n\n{}",
        spec.ordinal,
        spec.title,
        spec.brief_md.trim_end()
    )
}

/// Extract the `<!-- atos:stop_condition:... -->` marker the charter
/// provisioner writes, so `next_milestone` can read the per-milestone
/// stop command without a schema change. Returns empty string when
/// absent (manual-review milestone).
pub fn extract_milestone_stop_condition(brief_md: &str) -> String {
    const OPEN: &str = "<!-- atos:stop_condition:";
    const CLOSE: &str = "-->";
    let Some(start) = brief_md.find(OPEN) else {
        return String::new();
    };
    let after = &brief_md[start + OPEN.len()..];
    let Some(end) = after.find(CLOSE) else {
        return String::new();
    };
    after[..end].trim().to_string()
}

/// Remove the stop-condition marker from a brief so it doesn't leak
/// into the driver's view of the milestone. The marker is provisioner
/// metadata — the agent sees the stop command via the brief header
/// composed by [`crate::PreparedBrief::render`].
pub(super) fn strip_stop_condition_marker(brief_md: &str) -> String {
    const OPEN: &str = "<!-- atos:stop_condition:";
    const CLOSE: &str = "-->";
    let Some(start) = brief_md.find(OPEN) else {
        return brief_md.trim().to_string();
    };
    let after = &brief_md[start..];
    let Some(end_rel) = after.find(CLOSE) else {
        return brief_md.trim().to_string();
    };
    let mut out = String::with_capacity(brief_md.len());
    out.push_str(&brief_md[..start]);
    out.push_str(&brief_md[start + end_rel + CLOSE.len()..]);
    out.trim().to_string()
}

/// Derive a short human title from a milestone's brief. The
/// provisioner prepends `### N. Title` so the first line is
/// authoritative.
pub(super) fn derive_milestone_title(brief_md: &str) -> String {
    let first = brief_md.lines().next().unwrap_or("").trim();
    let stripped = first
        .trim_start_matches('#')
        .trim()
        .trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | ')' | ':' | ' '))
        .trim();
    if stripped.is_empty() {
        first.to_string()
    } else {
        stripped.to_string()
    }
}

/// Fetch global-scope invariant notes. The trait method
/// `active_global_invariants` on [`super::orchestrator::LocalAtosOrchestrator`]
/// is a thin wrapper around this; the split lets the inference helper
/// below reuse the same query without going through the trait.
pub(super) async fn global_invariants_rows(notes: &NoteStore) -> Result<Vec<NoteRow>> {
    let filter = ScopeFilter {
        scopes: vec![NoteScope::Global],
        feature_id: None,
    };
    Ok(notes
        .read_notes_scoped(
            None,
            &[],
            &[],
            &["invariant".to_string()],
            50,
            false,
            &filter,
        )
        .await?)
}

/// Compose the prior-milestone digest — feature-scoped notes, most
/// recent first, bounded at 30 entries. Deterministic rendering so
/// the handoff flow is testable without an inference provider; a
/// future Fast-slot summarizer can swap in here without touching the
/// caller.
pub(super) async fn compose_prior_digest(
    notes: &NoteStore,
    feature_id: &str,
) -> Result<String> {
    let filter = ScopeFilter {
        scopes: vec![NoteScope::Feature],
        feature_id: Some(feature_id.to_string()),
    };
    let rows = notes
        .read_notes_scoped(
            None,
            &[],
            &[],
            &[
                "decision".to_string(),
                "invariant".to_string(),
                "attempt".to_string(),
                "uncertainty".to_string(),
                "postmortem_pointer".to_string(),
            ],
            30,
            false,
            &filter,
        )
        .await?;
    if rows.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::new();
    for n in rows {
        let first_line: String =
            n.content.lines().next().unwrap_or("").chars().take(160).collect();
        out.push_str(&format!("- `[note:{}]` [{}] {}\n", n.id, n.kind, first_line));
    }
    Ok(out)
}

/// Gather active global invariants for every fresh session's
/// preamble. Capped at 20 so large corpora don't blow the brief up.
pub(super) async fn compose_global_invariants(notes: &NoteStore) -> Result<String> {
    let rows = global_invariants_rows(notes).await?;
    if rows.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::new();
    for n in rows.iter().take(20) {
        let first_line: String =
            n.content.lines().next().unwrap_or("").chars().take(200).collect();
        out.push_str(&format!("- `[note:{}]` {}\n", n.id, first_line));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_stop_condition_from_brief() {
        let brief = "### 1. Title\n\nBody.\n\n<!-- atos:stop_condition:cargo test -->\n";
        assert_eq!(
            extract_milestone_stop_condition(brief),
            "cargo test".to_string()
        );
    }

    #[test]
    fn extract_stop_condition_absent_returns_empty() {
        assert_eq!(extract_milestone_stop_condition("### 1. Title\n\nBody.\n"), "");
    }

    #[test]
    fn strip_marker_removes_only_the_marker() {
        let brief = "### 1. Title\n\nBody text.\n\n<!-- atos:stop_condition:cargo test -->\n";
        let stripped = strip_stop_condition_marker(brief);
        assert!(stripped.contains("Body text"));
        assert!(!stripped.contains("<!-- atos:stop_condition"));
    }

    #[test]
    fn derive_title_strips_header_prefix() {
        assert_eq!(
            derive_milestone_title("### 1. Wire the flag\n\nbody"),
            "Wire the flag"
        );
    }

    #[test]
    fn derive_title_handles_plain_text_fallback() {
        assert_eq!(derive_milestone_title("Plain text brief"), "Plain text brief");
    }

    #[test]
    fn extract_id_and_title_em_dash_separator() {
        let md = "# atos-foo — Human Title\n\nbody";
        let (id, title) = extract_id_and_title(md).unwrap();
        assert_eq!(id, "atos-foo");
        assert_eq!(title, "Human Title");
    }

    #[test]
    fn extract_id_and_title_rejects_missing_heading() {
        let md = "body without heading";
        let err = extract_id_and_title(md).unwrap_err();
        assert!(matches!(err, Error::CharterParse(_)));
    }
}
