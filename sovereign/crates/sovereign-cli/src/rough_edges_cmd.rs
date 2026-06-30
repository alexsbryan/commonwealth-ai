// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn rough-edges <code-corpus> [--source-path <path>] [--output <md>]`
//!
//! Scans a code corpus' source tree for `TODO/FIXME/HACK/XXX`
//! markers and renders a one-page digest. JSON sidecar carries the
//! full detail for downstream tools (drift orchestrator, IDEs).
//!
//! Tier 0 of the internal-contradiction work (task #36/37). Pairs
//! with a future tier-1 rustdoc-vs-signature drift detector (#39)
//! that will share the same finding shape and renderer.

use std::path::{Path, PathBuf};

use corpus_engine_archaeology::rough_edges::{
    scan_all, DocDriftKind, FindingKind, MarkerKind, RoughEdgeFinding, Severity, SmellKind,
};

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let source_path = match resolve_source_path(&parsed) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    println!("=== sovereign rough-edges ===");
    println!("  corpus       = {}", parsed.corpus_id);
    println!("  source       = {}", source_path.display());
    if let Some(o) = &parsed.output {
        println!("  output       = {}", o.display());
    } else {
        println!("  output       = <stdout>");
    }
    println!();

    let findings = scan_all(&source_path);

    let summary = summary_line(&findings);
    println!("  {summary}");
    println!();

    let md = render_markdown(&parsed.corpus_id, &source_path, &findings);

    if let Some(out_path) = &parsed.output {
        if let Err(e) = std::fs::write(out_path, &md) {
            eprintln!("✗ failed to write {}: {e}", out_path.display());
            return 1;
        }
        let json_path = sidecar_json_path(out_path);
        let json_body = serde_json::to_string_pretty(&JsonReport {
            corpus_id: parsed.corpus_id.clone(),
            source_path: source_path.clone(),
            findings: findings.clone(),
        })
        .unwrap_or_else(|_| "{}".into());
        if let Err(e) = std::fs::write(&json_path, json_body) {
            eprintln!("✗ failed to write {}: {e}", json_path.display());
            return 1;
        }
        println!("  ✓ wrote {}", out_path.display());
        println!("  ✓ wrote {}", json_path.display());
    } else {
        print!("{md}");
    }

    0
}

// ── Argument parsing ─────────────────────────────────────────

#[derive(Default)]
struct Args {
    corpus_id: String,
    source_path: Option<PathBuf>,
    output: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source-path" => {
                let v = args.get(i + 1).ok_or("--source-path requires a value")?;
                out.source_path = Some(PathBuf::from(v));
                i += 2;
            }
            "--output" => {
                let v = args.get(i + 1).ok_or("--output requires a value")?;
                out.output = Some(PathBuf::from(v));
                i += 2;
            }
            s if !s.starts_with("--") && out.corpus_id.is_empty() => {
                out.corpus_id = s.to_string();
                i += 1;
            }
            other => return Err(format!("unrecognised argument: {other}")),
        }
    }
    if out.corpus_id.is_empty() {
        return Err(
            "missing positional <corpus-id>. usage: sovereign rough-edges \
             <corpus-id> [--source-path <path>] [--output <md>]"
                .into(),
        );
    }
    Ok(out)
}

/// Source path comes from one of:
/// 1. Explicit `--source-path <path>` (highest priority).
/// 2. The corpus's `_corpus_meta.json` `source_path` field (set by
///    `svrn code index` for code corpora).
/// 3. Fall back to error if neither is present.
fn resolve_source_path(args: &Args) -> Result<PathBuf, String> {
    if let Some(p) = &args.source_path {
        if !p.exists() {
            return Err(format!("--source-path {} does not exist", p.display()));
        }
        return Ok(p.clone());
    }
    let canonical_meta = home_dir()
        .join(".sovereign/indexes")
        .join(&args.corpus_id)
        .join("_corpus_meta.json");
    let partition_meta = home_dir()
        .join(".sovereign/indexes")
        .join(format!("{}-partition-local", args.corpus_id))
        .join("_corpus_meta.json");
    let meta_path = if canonical_meta.exists() {
        canonical_meta
    } else if partition_meta.exists() {
        // Sharded-install artifact: chunks live under -partition-local
        // even though the corpus_id inside is the bare name.
        partition_meta
    } else {
        return Err(format!(
            "corpus '{}' not found at {} (or {}) — run `svrn code index` \
             first or pass --source-path",
            args.corpus_id,
            canonical_meta.display(),
            partition_meta.display()
        ));
    };
    let raw = std::fs::read_to_string(&meta_path)
        .map_err(|e| format!("read {}: {e}", meta_path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", meta_path.display()))?;
    let s = v
        .get("source_path")
        .and_then(|x| x.as_str())
        .ok_or_else(|| {
            format!(
                "corpus '{}' has no source_path stamped — pass --source-path \
                 explicitly. (Only code-corpus installs from `svrn code \
                 index` stamp a source_path.)",
                args.corpus_id
            )
        })?;
    let p = PathBuf::from(s);
    if !p.exists() {
        return Err(format!(
            "stamped source_path {} no longer exists — pass --source-path",
            p.display()
        ));
    }
    Ok(p)
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

fn sidecar_json_path(md: &Path) -> PathBuf {
    let mut p = md.to_path_buf();
    p.set_extension("json");
    p
}

// ── Rendering ────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct JsonReport {
    corpus_id: String,
    source_path: PathBuf,
    findings: Vec<RoughEdgeFinding>,
}

fn summary_line(findings: &[RoughEdgeFinding]) -> String {
    let mut markers = 0usize;
    let mut doc_drift = 0usize;
    let mut smells = 0usize;
    let mut critical = 0usize;
    let mut likely = 0usize;
    let mut note = 0usize;
    for f in findings {
        match f.kind {
            FindingKind::Marker(_) => markers += 1,
            FindingKind::DocDrift(_) => doc_drift += 1,
            FindingKind::Smell(_) => smells += 1,
        }
        match f.severity {
            Severity::Critical => critical += 1,
            Severity::Likely => likely += 1,
            Severity::Note => note += 1,
        }
    }
    format!(
        "{} findings ({markers} markers · {doc_drift} doc-drift · {smells} smells · {critical} critical · {likely} likely · {note} note)",
        findings.len()
    )
}

fn render_markdown(corpus_id: &str, source_path: &Path, findings: &[RoughEdgeFinding]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Rough Edges — `{corpus_id}`\n\n*{}*\n\n",
        summary_line(findings)
    ));
    out.push_str(&format!("Source: `{}`\n\n", source_path.display()));

    if findings.is_empty() {
        out.push_str("No rough edges found. ✓\n");
        return out;
    }

    // Group by marker kind in a stable order.
    let order = [
        MarkerKind::Xxx,
        MarkerKind::Fixme,
        MarkerKind::Hack,
        MarkerKind::Todo,
    ];
    for marker in order {
        let group: Vec<&RoughEdgeFinding> = findings
            .iter()
            .filter(|f| matches!(f.kind, FindingKind::Marker(m) if m == marker))
            .collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "## {} ({})\n\n",
            marker_heading(marker),
            group.len()
        ));
        // Cap per-section render at 50 entries to keep the digest
        // scannable; the full list lives in the JSON sidecar.
        let cap = 50;
        for f in group.iter().take(cap) {
            let rel = relativize(&f.file, source_path);
            out.push_str(&format!(
                "- `{}:{}` — {}\n",
                rel.display(),
                f.line,
                f.message
            ));
        }
        if group.len() > cap {
            out.push_str(&format!(
                "- *…and {} more (see JSON sidecar)*\n",
                group.len() - cap
            ));
        }
        out.push('\n');
    }

    // Doc-drift section.
    let doc_drift: Vec<&RoughEdgeFinding> = findings
        .iter()
        .filter(|f| matches!(f.kind, FindingKind::DocDrift(_)))
        .collect();
    if !doc_drift.is_empty() {
        out.push_str(&format!(
            "## Doc-vs-signature drift ({})\n\n_Rustdoc claims that the body or signature contradicts. \
             Likely-severity by default; review and either remove the doc claim or restore the \
             behaviour it describes._\n\n",
            doc_drift.len()
        ));
        let cap = 50;
        for f in doc_drift.iter().take(cap) {
            let rel = relativize(&f.file, source_path);
            let kind_label = match f.kind {
                FindingKind::DocDrift(DocDriftKind::SectionMismatch) => "section-mismatch",
                FindingKind::DocDrift(DocDriftKind::MissingParam) => "missing-param",
                FindingKind::DocDrift(DocDriftKind::UnknownIdent) => "unknown-ident",
                _ => "other",
            };
            out.push_str(&format!(
                "- `{}:{}` ({}) — {}\n",
                rel.display(),
                f.line,
                kind_label,
                f.message
            ));
        }
        if doc_drift.len() > cap {
            out.push_str(&format!(
                "- *…and {} more (see JSON sidecar)*\n",
                doc_drift.len() - cap
            ));
        }
        out.push('\n');
    }

    // Smells (tier-2) section.
    let smells: Vec<&RoughEdgeFinding> = findings
        .iter()
        .filter(|f| matches!(f.kind, FindingKind::Smell(_)))
        .collect();
    if !smells.is_empty() {
        out.push_str(&format!(
            "## Smells ({})\n\n_Code smells the structural-correctness layer flags: \
             absolute developer paths in source (portability), large files with zero \
             tracing events (§9.1 glassbox)._\n\n",
            smells.len()
        ));
        let cap = 50;
        for f in smells.iter().take(cap) {
            let rel = relativize(&f.file, source_path);
            let kind_label = match f.kind {
                FindingKind::Smell(SmellKind::AbsoluteUserPath) => "absolute-user-path",
                FindingKind::Smell(SmellKind::ZeroTracing) => "zero-tracing",
                _ => "other",
            };
            out.push_str(&format!(
                "- `{}:{}` ({}) — {}\n",
                rel.display(),
                f.line,
                kind_label,
                f.message
            ));
        }
        if smells.len() > cap {
            out.push_str(&format!(
                "- *…and {} more (see JSON sidecar)*\n",
                smells.len() - cap
            ));
        }
        out.push('\n');
    }
    out
}

fn marker_heading(m: MarkerKind) -> &'static str {
    match m {
        MarkerKind::Xxx => "XXX (alarm)",
        MarkerKind::Fixme => "FIXME (known broken)",
        MarkerKind::Hack => "HACK (known-bad fix)",
        MarkerKind::Todo => "TODO (intent)",
    }
}

fn relativize(path: &Path, root: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf())
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn rough-edges",
    summary: "Scan a code corpus for FIXME/TODO/HACK/XXX markers (rough-edge inventory).",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn rough-edges <corpus-id> [--source-path <dir>] [--output <md>]",
        ),
        crate::util::help::HelpSection::Notes(
            "Reads source path from the corpus's _corpus_meta.json by default. \
             Writes a markdown digest plus a .json sidecar (full per-finding detail \
             for downstream tools). Standalone surface; also called from \
             `svrn drift detect` to enrich the unified drift digest.",
        ),
    ],
};
