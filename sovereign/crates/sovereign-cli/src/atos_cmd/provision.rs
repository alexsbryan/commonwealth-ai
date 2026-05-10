//! `sovereign atos provision <id>` (retired Phase 6) and
//! `sovereign atos archive <id>`.
//!
//! ## Phase 6 retirement
//!
//! `provision` is no longer required. The default user flow now is:
//!
//!   `sovereign init` → write `.sovereign/features/<id>/spec.md`
//!                    → `git commit` (= approval — see approval_gate)
//!                    → work
//!
//! Spec-existence on disk is sufficient for `tools/list` to surface
//! the spec/drift/note/notes tools (Phase 5b gate), and a committed
//! spec is what the approval_gate middleware now reads (no
//! features.db row required). The auditor lists features by walking
//! `.sovereign/features/*/` so a directory-only feature shows up
//! immediately.
//!
//! `cmd_provision` is therefore a no-op + deprecation banner. The
//! programmatic path through `AtosOrchestrator::provision_feature`
//! remains intact for tests and any operator who genuinely wants a
//! seeded `features.db` row. Archive is unchanged — flipping a
//! feature row to "archived" still has a meaningful surface.

use sovereign_atos::AtosOrchestrator;

use super::args::{get_flag, split_args};
use super::stores::open_orchestrator;

pub(crate) async fn cmd_provision(_args: &[String]) -> i32 {
    crate::util::deprecation::announce_retired(
        "sovereign atos provision",
        "Provisioning is implicit now: an existing \
         `.sovereign/features/<id>/spec.md` (committed for write \
         tools to be approved) is sufficient. The auditor and the \
         MCP gate read directories directly; no features.db row is \
         required up-front.",
    );
    0
}

pub(crate) async fn cmd_archive(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("archive: missing <id>");
        return 2;
    };
    let reason = get_flag(&flags, "--reason").unwrap_or_else(|| "(no reason given)".into());

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("archive: {e}");
            return 1;
        }
    };

    match orc.archive_feature(&id, &reason).await {
        Ok(true) => {
            println!("archived feature '{id}'");
            0
        }
        Ok(false) => {
            eprintln!("archive: feature '{id}' not found");
            1
        }
        Err(e) => {
            eprintln!("archive: {e}");
            1
        }
    }
}
