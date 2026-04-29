//! `sovereign atos provision <id>` and `sovereign atos archive <id>`.
//!
//! Provision has two shapes:
//!
//! 1. **Structured** — operator passes `--charter <path>`; the
//!    orchestrator parses the markdown, extracts the title and
//!    milestones, and seeds `feature_milestones` rows. Yara's path.
//! 2. **Parts-based (legacy M1/M2)** — caller passes `--title` +
//!    `--charter` + `--stop-cmd` explicitly. Kept for scripts that
//!    were written before the charter parser landed.
//!
//! Archive is a one-shot marker: `archive --reason <text>` flips the
//! feature to archived and the reason is surfaced in `atos status`.

use sovereign_atos::AtosOrchestrator;

use super::args::{get_flag, split_args};
use super::stores::open_orchestrator;

pub(crate) async fn cmd_provision(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let id_flag = positional.first().cloned();
    let title_flag = get_flag(&flags, "--title");
    let charter_path = match get_flag(&flags, "--charter") {
        Some(p) => p,
        None => {
            eprintln!("provision: --charter <path> is required");
            return 2;
        }
    };
    let charter_md = match std::fs::read_to_string(&charter_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("provision: read {charter_path}: {e}");
            return 1;
        }
    };
    let sovereign_md = match get_flag(&flags, "--sovereign-md") {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("provision: read {p}: {e}");
                return 1;
            }
        },
        None => String::new(),
    };
    let stop_condition = get_flag(&flags, "--stop-cmd").unwrap_or_default();

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("provision: {e}");
            return 1;
        }
    };

    // Structured path: when `--title` is NOT given, parse the charter.
    // This preserves the M1/M2 parts-based flow for callers that want
    // imperative control AND gives Yara's "write a charter, provision
    // it" flow a first-class path.
    if title_flag.is_none() {
        match orc.provision_feature(&charter_md).await {
            Ok(f) => {
                let milestones = orc.list_milestones(&f.id).await.unwrap_or_default();
                println!(
                    "parsed {} milestone{} from charter",
                    milestones.len(),
                    if milestones.len() == 1 { "" } else { "s" }
                );
                println!("provisioned feature '{}': {}", f.id, f.title);
                return 0;
            }
            Err(e) => {
                eprintln!("provision: {e}");
                return 1;
            }
        }
    }

    // Parts-based fallback. Requires <id> positional.
    let Some(id) = id_flag else {
        eprintln!("provision: missing <id> (required unless charter drives it)");
        return 2;
    };
    let title = title_flag.unwrap_or_default();

    match orc
        .provision_feature_parts(&id, &title, &charter_md, &sovereign_md, &stop_condition)
        .await
    {
        Ok(f) => {
            println!("provisioned feature '{}': {}", f.id, f.title);
            0
        }
        Err(e) => {
            eprintln!("provision: {e}");
            1
        }
    }
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
