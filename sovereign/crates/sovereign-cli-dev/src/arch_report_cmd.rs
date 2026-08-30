// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code arch-report` — the WRITER of the persisted architecture
//! posture (quality program).
//!
//! Builds the full report (SCIP metrics + declared deps + layer-map check +
//! filesystem + git temporal coupling) and persists
//! `~/.svrnmesh/arch/<corpus>/` so the cheap `arch_posture` tool answers
//! from disk. The MCP `arch_report` tool computes the same report on demand
//! but never writes. Own module (not a `code_cmd.rs` arm) per ARCH §3.1 —
//! that file is already on the oversized baseline; the arch-gate correctly
//! rejected growing it further.

use std::path::PathBuf;

use sovereign_tools::code::arch_report::{
    build_arch_report, persist_arch_report, render_report, ArchReportInputs,
};

pub(crate) async fn run(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut include_git = true;
    let mut root_override: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-git" => include_git = false,
            "--root" => {
                i += 1;
                root_override = args.get(i).map(PathBuf::from);
                if root_override.is_none() {
                    eprintln!("error: --root requires a path");
                    return 1;
                }
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return 1;
            }
            positional => {
                if corpus_id.is_none() {
                    corpus_id = Some(positional.to_string());
                }
            }
        }
        i += 1;
    }

    // Resolve the corpus: explicit, or the sole indexed code corpus.
    let indexes_dir = sovereign_cli_shared::dirs::sovereign_root().join("indexes");
    let corpus_id = match crate::converge_cmd::resolve_corpus(corpus_id, &indexes_dir) {
        Ok(c) => c,
        Err(code) => return code,
    };

    let db_path = indexes_dir.join(&corpus_id).join("scip_graph.db");
    if !db_path.exists() {
        eprintln!(
            "error: no SCIP graph at {} — run `svrn project init` first",
            db_path.display()
        );
        return 1;
    }

    // Workspace root: explicit --root, else the cwd when it looks like a
    // workspace. Without one the report is SCIP-only (a warning, not an
    // error — the daemon surface has the same degradation).
    let project_root = root_override.or_else(|| {
        std::env::current_dir()
            .ok()
            .filter(|d| d.join("Cargo.toml").exists())
    });
    if project_root.is_none() {
        eprintln!(
            "warning: no workspace root (run from the repo or pass --root) — \
             declared-deps, layer-map, filesystem and git sections will be skipped"
        );
    }

    let data = match build_arch_report(ArchReportInputs {
        db_path: &db_path,
        corpus_id: &corpus_id,
        project_root: project_root.as_deref(),
        include_git: include_git && project_root.is_some(),
    })
    .await
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let markdown = render_report(&data);
    match persist_arch_report(&data, &markdown) {
        Ok(dir) => {
            println!("{markdown}");
            eprintln!(
                "wrote {} (arch_report.md / .json / .fingerprint — `arch_posture` reads these)",
                dir.display()
            );
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
