// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench obsidian` — atlas correctness bench for the
//! author's real obsidian vault.
//!
//! Wraps `enrich eval` against
//! `sovereign/bench/obsidian/golden.toml` (a hand-authored golden
//! grounded in the vault's actual root-essay content as of the file's
//! authoring date). The vault path is supplied via the
//! `--vault <path>` flag or the `SOVEREIGN_OBSIDIAN_VAULT` env var —
//! never baked into the repo, so the artifact stays portable.
//!
//! The actual scoring is delegated to `enrich_cmd::eval::cmd_eval` so
//! the per-phase precision/recall/F1 surface stays unified across
//! literary, philosophy, and obsidian bench runs. This subcommand is
//! a thin selector for the right corpus + golden plus a pre-flight
//! sanity check.
//!
//! Workflow:
//!
//! ```sh
//!     export SOVEREIGN_OBSIDIAN_VAULT="/Users/user/Documents/Obsidian Vault"
//!
//!     # Two scoring surfaces produce a golden-compatible `atoms.json`:
//!     #
//!     # (a) LIVE tiered surface (what vault chat actually uses).
//!     #     `FolderTieredProvider::finalize_corpus` runs the
//!     #     typed-extension pass over RAPTOR summaries at the tail of
//!     #     every tiered build (spec: docs/specs/TYPED_EXTENSION_PASS.md,
//!     #     shipped 2026-05-24 with the vault tiered port). Score the
//!     #     registered vault corpus directly:
//!     #
//!     #         svrn bench obsidian --corpus obsidian-<hash> \
//!     #             --report /tmp/obsidian-bench.json
//!     #
//!     #     (re-run extraction after a prompt iteration without a
//!     #      rebuild: `svrn atlas typed-extension obsidian-<hash>`)
//!     #
//!     # (b) literary_atlas pin (legacy comparison surface) — one-time,
//!     #     per major content shift in the vault:
//!     #
//!     #         svrn enrich init obsidian-vault \
//!     #             --source "$SOVEREIGN_OBSIDIAN_VAULT" \
//!     #             --pipeline literary_atlas --force
//!     #         svrn enrich build obsidian-vault
//!
//!     # every prompt-tuning iteration
//!     svrn bench obsidian --report /tmp/obsidian-bench.json
//! ```
//!
//! See `sovereign/bench/obsidian/README.md` for the bench's scope
//! (root essays only, COMMONWEALTH/ excluded), authoring posture,
//! and known gaps.

use std::path::PathBuf;

use sovereign_cli_shared::help::{self, Help, HelpSection};

/// Default corpus id when `--corpus` is not supplied. Matches the
/// `enrich init` example in the module doc-comment.
const DEFAULT_CORPUS_ID: &str = "obsidian-vault";

/// Default golden TOML path (relative to the workspace root).
/// `enrich eval` resolves the path as-given; the workspace root is
/// the conventional cwd for the CLI, so this matches existing
/// `enrich eval` examples like `bench/literary/dubliners-3.toml`.
const DEFAULT_GOLDEN_PATH: &str = "sovereign/bench/obsidian/golden.toml";

/// Env var the user sets to point the bench at their vault. Read
/// once at parse time; `--vault <path>` always wins over the env var
/// when both are set so a user can override interactively.
const VAULT_ENV_VAR: &str = "SOVEREIGN_OBSIDIAN_VAULT";

const HELP: Help = Help {
    command: "svrn bench obsidian",
    summary: "Score the resolved atlas of an obsidian-vault corpus against the in-repo golden.",
    sections: &[
        HelpSection::Usage(
            "svrn bench obsidian [--vault <path>] [--corpus <id>] [--golden <path>] [--report <json-path>]",
        ),
        HelpSection::Flags(&[
            (
                "--vault <path>",
                "Absolute path to the obsidian vault to score. Overrides the SOVEREIGN_OBSIDIAN_VAULT \
                 environment variable. Informational at this layer — used by the pre-flight hint that \
                 prints the `enrich init` line if the corpus has not been built yet.",
            ),
            (
                "--corpus <id>",
                "Corpus to score. Default: obsidian-vault. The corpus must already be \
                 `svrn enrich init`'d and `svrn enrich build`'d with --pipeline obsidian_atlas.",
            ),
            (
                "--golden <path>",
                "Path to the golden TOML. Default: sovereign/bench/obsidian/golden.toml. \
                 Override when scoring against a custom golden.",
            ),
            (
                "--report <json-path>",
                "Forward to `enrich eval --report` so a JSON scoreboard is written for diffing across \
                 prompt iterations. Recommended for any run you will come back to.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn bench obsidian --report /tmp/obsidian-bench.json",
                "Score the default corpus (obsidian-vault) against the in-repo golden.",
            ),
            (
                "SOVEREIGN_OBSIDIAN_VAULT=\"$HOME/Documents/Obsidian Vault\" \\\n  svrn bench obsidian --report /tmp/r.json",
                "Set the vault path via env var so the pre-flight hint suggests the right `enrich init` line.",
            ),
        ]),
        HelpSection::Notes(
            "v1 wraps `svrn enrich eval` with sensible obsidian defaults. You must run \
             `enrich init` + `enrich build` against the corpus once before scoring; this \
             subcommand does NOT build the atlas itself. The golden at \
             sovereign/bench/obsidian/golden.toml is grounded in real vault content as of \
             its authoring date — see the README in that directory for the drift policy.",
        ),
    ],
};

struct Args {
    corpus: String,
    golden: PathBuf,
    /// Where the user says their vault lives. `--vault` wins over the
    /// env var; both surface at the pre-flight `enrich init` hint, not
    /// in the call to `enrich eval` (the corpus path is owned by the
    /// init step, not by eval).
    vault_path: Option<PathBuf>,
    report: Option<PathBuf>,
}

impl Args {
    fn defaults() -> Self {
        let vault_from_env = std::env::var(VAULT_ENV_VAR).ok().map(PathBuf::from);
        Self {
            corpus: DEFAULT_CORPUS_ID.to_string(),
            golden: PathBuf::from(DEFAULT_GOLDEN_PATH),
            vault_path: vault_from_env,
            report: None,
        }
    }
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::defaults();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--corpus" => {
                i += 1;
                out.corpus = args
                    .get(i)
                    .ok_or_else(|| "--corpus needs a value".to_string())?
                    .clone();
            }
            "--golden" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--golden needs a value".to_string())?;
                out.golden = PathBuf::from(v);
            }
            "--vault" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--vault needs a value".to_string())?;
                out.vault_path = Some(PathBuf::from(v));
            }
            "--report" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--report needs a value".to_string())?;
                out.report = Some(PathBuf::from(v));
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(out)
}

pub async fn cmd_obsidian(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let mut parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    // Resolve a friendly corpus argument (display name / unique
    // fragment) to the real id — ids carry a hash suffix nobody
    // should have to type. Failing here beats forwarding an unknown
    // id to `enrich eval`, whose error doesn't list the candidates.
    let indexes_dir = Some(sovereign_contracts::rebrand::data_dir().join("indexes"));
    if let Some(indexes_dir) = indexes_dir {
        match crate::corpus_resolve::resolve_corpus_id(&indexes_dir, &parsed.corpus) {
            Ok(id) => {
                if id != parsed.corpus {
                    println!("Corpus '{}' resolved to '{id}'", parsed.corpus);
                }
                parsed.corpus = id;
            }
            Err(msg) => {
                eprintln!("error: {msg}");
                return 2;
            }
        }
    }

    // Pre-flight: golden must exist. `enrich eval` would surface
    // the same error with a less obvious provenance — better to fail
    // early here with the workspace-root hint.
    if !parsed.golden.exists() {
        eprintln!(
            "error: golden TOML not found at {}",
            parsed.golden.display()
        );
        eprintln!("hint: invoke from the workspace root, or pass --golden <absolute-path>");
        return 2;
    }

    // If the user supplied a vault path (via --vault or env), validate
    // it exists. The path isn't forwarded to `enrich eval` (eval reads
    // a previously-built corpus, not the vault), but a wrong path is
    // a clear sign the user is about to confuse themselves about
    // which corpus the report describes — surface it now.
    if let Some(vault) = parsed.vault_path.as_ref() {
        if !vault.exists() {
            eprintln!(
                "warning: --vault path does not exist on disk: {}",
                vault.display()
            );
            eprintln!(
                "hint: this run will score whatever corpus '{}' currently holds in \
                 ~/.svrnmesh/indexes/. If that corpus was built from a different vault \
                 than the one you intended, the report will not reflect today's vault state.",
                parsed.corpus
            );
        } else {
            println!(
                "Vault at: {} (informational; scoring against built corpus '{}')",
                vault.display(),
                parsed.corpus
            );
        }
    }

    // Forward to `enrich eval` with the corpus + golden. The eval
    // command already supports --report; we pass it through verbatim
    // so the JSON shape stays consistent across literary, philosophy,
    // and obsidian runs (one downstream diff tool, not three).
    let mut forward: Vec<String> = vec![
        parsed.corpus.clone(),
        parsed.golden.to_string_lossy().to_string(),
    ];
    if let Some(report) = parsed.report.as_ref() {
        forward.push("--report".to_string());
        forward.push(report.to_string_lossy().to_string());
    }

    println!(
        "Running `enrich eval {} {}` …",
        parsed.corpus,
        parsed.golden.display()
    );
    crate::enrich_cmd::eval::cmd_eval(&forward).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // The default cargo-test runner executes tests in parallel
    // threads sharing one process — and one env table. Any test that
    // touches VAULT_ENV_VAR has to serialise against every other
    // such test or the env state races. A Mutex is the simplest
    // serialisation primitive that doesn't pull in a new dep.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_clean_env<F: FnOnce()>(f: F) {
        // Hold the lock for the test's entire body, including the
        // restore step, so a panicking test still releases the lock
        // (PoisonError is fine — subsequent tests recover the guard
        // and proceed; the env table itself stays cleanly restored
        // by the panic-unwind path's drop of the guard).
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var(VAULT_ENV_VAR).ok();
        std::env::remove_var(VAULT_ENV_VAR);
        f();
        match prior {
            Some(v) => std::env::set_var(VAULT_ENV_VAR, v),
            None => std::env::remove_var(VAULT_ENV_VAR),
        }
    }

    #[test]
    fn defaults_are_obsidian_vault_with_repo_golden() {
        with_clean_env(|| {
            let a = Args::defaults();
            assert_eq!(a.corpus, "obsidian-vault");
            assert!(a.golden.ends_with("golden.toml"));
            assert!(a.vault_path.is_none(), "no env → no vault path default");
            assert!(a.report.is_none());
        });
    }

    #[test]
    fn env_var_populates_vault_path() {
        with_clean_env(|| {
            std::env::set_var(VAULT_ENV_VAR, "/tmp/my-vault");
            let a = Args::defaults();
            assert_eq!(
                a.vault_path.as_deref(),
                Some(std::path::Path::new("/tmp/my-vault"))
            );
        });
    }

    #[test]
    fn parse_overrides_corpus_and_report_and_vault() {
        with_clean_env(|| {
            let args = vec![
                "--corpus".into(),
                "my-vault".into(),
                "--report".into(),
                "/tmp/r.json".into(),
                "--vault".into(),
                "/tmp/v".into(),
            ];
            let a = parse_args(&args).expect("parse");
            assert_eq!(a.corpus, "my-vault");
            assert_eq!(
                a.report.as_deref(),
                Some(std::path::Path::new("/tmp/r.json"))
            );
            assert_eq!(
                a.vault_path.as_deref(),
                Some(std::path::Path::new("/tmp/v"))
            );
        });
    }

    #[test]
    fn cli_vault_flag_overrides_env_var() {
        with_clean_env(|| {
            std::env::set_var(VAULT_ENV_VAR, "/from/env");
            let args = vec!["--vault".into(), "/from/flag".into()];
            let a = parse_args(&args).expect("parse");
            assert_eq!(
                a.vault_path.as_deref(),
                Some(std::path::Path::new("/from/flag")),
                "--vault must beat the env var"
            );
        });
    }

    #[test]
    fn unknown_flag_is_rejected() {
        with_clean_env(|| {
            let args = vec!["--bogus".into()];
            assert!(parse_args(&args).is_err());
        });
    }
}
