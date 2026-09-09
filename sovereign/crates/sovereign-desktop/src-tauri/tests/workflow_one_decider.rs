// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface rung 5's workflow family census: workflow EXECUTION has one
//! home — the daemon's `/internal/workflows/*` job surface
//! (`sovereign-workflow-host::workflow_http`). The desktop was one of two
//! in-process runtimes (`run_workflow_with_provider` fed `AppState.inference`,
//! plus the client-side catalog: `Workflow::parse` + `SHIPPED_WORKFLOWS` +
//! `resolve_workflow_source`, plus the corpus-param derivation now in
//! `derive_run_params`).
//!
//! # The state this makes unrepresentable
//!
//! A second in-process workflow runtime, or a client-side re-derivation of
//! the catalog, in the desktop's workflow surfaces. Both `workflow_commands`
//! (the Run view) and `local_corpus_commands`' runner-ingest path must reach
//! the daemon's routes and nothing else.
//!
//! Watched to fail: re-introduce any of the forbidden spellings in either
//! file's CODE and this goes red naming the rule. Sabotage-verified at
//! landing (a planted `run_workflow_with_provider` call, watched red,
//! reverted, green).
//!
//! Out of scope by measurement: `recipe_author_commands.rs` keeps
//! `Workflow::parse` — authoring-time validation of a draft in the editor is
//! a different question from catalog/run (the same distinction that struck
//! the health-report row in rung 4), and `state.rs` mounts
//! `WorkflowAuthoringTools` in the embedded daemon's MCP bundle plus the
//! workflow router itself — serving-side, not a client twin.

use std::path::Path;

fn source(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The code view of a file: comment lines stripped, so a needle names a
/// CODE spelling. (Rung 4's lesson: needles over raw text false-fire on
/// prose — this module's own docs mention the retired spellings.)
fn code_lines(src: &str) -> Vec<String> {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(String::from)
        .collect()
}

#[test]
fn workflow_execution_has_one_home_the_daemon_job_surface() {
    for rel in ["src/workflow_commands.rs", "src/local_corpus_commands.rs"] {
        let code = code_lines(&source(rel));
        // Each needle names the exact retreat spelling it forbids.
        for needle in [
            "run_workflow_with_provider(",
            "run_workflow_in_process(",
            "resolve_workflow_source(",
            "Workflow::parse(",
            "SHIPPED_WORKFLOWS",
            "workflow_corpus_tools",
            "HttpCorpusInstaller",
        ] {
            assert!(
                !code.iter().any(|l| l.contains(needle)),
                "sv-surface rung 5: `{needle}` is back in {rel}'s code — a client-side \
                 workflow runtime/catalog spelling. Execution, the catalog, and the \
                 corpus-param derivation have one home: the daemon's \
                 /internal/workflows/* routes (sovereign-workflow-host::workflow_http). \
                 Submit a job and poll; do not re-derive the runner client-side."
            );
        }
    }
}

#[test]
fn the_workflow_commands_reach_the_daemon_routes() {
    let commands = code_lines(&source("src/workflow_commands.rs"));
    for needle in [
        "/internal/workflows/list",
        "/internal/workflows/run",
        "/internal/workflows/jobs/",
    ] {
        assert!(
            commands.iter().any(|l| l.contains(needle)),
            "workflow_commands.rs must drive the daemon's `{needle}` route — the \
             rung-5 conversion makes it a job-submission client"
        );
    }
    let runner_ingest = code_lines(&source("src/local_corpus_commands.rs"));
    assert!(
        runner_ingest
            .iter()
            .any(|l| l.contains("/internal/workflows/run")),
        "the SOVEREIGN_RUNNER_INGEST path must submit the notebook job to the \
         daemon route, not run in-process"
    );
}
