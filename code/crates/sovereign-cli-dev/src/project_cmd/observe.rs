// SPDX-License-Identifier: AGPL-3.0-or-later
//! `project init`'s project-model step, served by the code program
//! (pb-code-cli-base). The project model (`crate::observation`,
//! `crate::project_toml`) is code's, so the dispatcher's `svrn init` links
//! none of it and execs two hidden arms instead:
//!
//! - `project-lifecycle <repo_root>` prints the lifecycle flags init reads
//!   (`git_declined_at_init`, `founded`) from `.sovereign/project.toml` as one
//!   JSON object;
//! - `project-observe <repo_root> --has-git <true|false> [--design-exists]
//!   [--git-declined]` observes the repo, prints the observation report and
//!   read-modify-writes `.sovereign/project.toml`, preserving its lifecycle.

use std::path::{Path, PathBuf};

use crate::observation::{DepKind, DetectedDependency, ProjectObservation, ScipTooling};
use crate::project_toml::ProjectTomlFile;

fn project_toml_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sovereign").join("project.toml")
}

/// `sovereign-cli-dev project-lifecycle <repo_root>`. A missing or
/// unreadable project.toml reads as a first init (both flags false), as
/// init's own read always did; the trace names which it was.
pub(crate) fn cmd_lifecycle(args: &[String]) -> i32 {
    let Some(repo_root) = args.first() else {
        eprintln!("usage: sovereign-cli-dev project-lifecycle <repo_root>");
        return 2;
    };
    let path = project_toml_path(Path::new(repo_root));
    let (git_declined_at_init, founded) = match ProjectTomlFile::read(&path) {
        Ok(t) => (t.lifecycle.git_declined_at_init, t.lifecycle.founded),
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "project-lifecycle: no readable project.toml, a first init");
            (false, false)
        }
    };
    println!(
        "{}",
        serde_json::json!({
            "git_declined_at_init": git_declined_at_init,
            "founded": founded,
        })
    );
    0
}

/// `sovereign-cli-dev project-observe <repo_root> --has-git <true|false>
/// [--design-exists] [--git-declined]`. Exit 1 when `.sovereign/` cannot be
/// created; a failed write is reported and init goes on, as before.
pub(crate) fn cmd_observe(args: &[String]) -> i32 {
    const USAGE: &str = "usage: sovereign-cli-dev project-observe <repo_root> --has-git <true|false> [--design-exists] [--git-declined]";
    let mut repo_root: Option<PathBuf> = None;
    let mut has_git: Option<bool> = None;
    let mut design_exists = false;
    let mut git_declined = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--has-git" => has_git = it.next().and_then(|v| v.parse().ok()),
            "--design-exists" => design_exists = true,
            "--git-declined" => git_declined = true,
            other if repo_root.is_none() && !other.starts_with("--") => {
                repo_root = Some(PathBuf::from(other))
            }
            other => {
                eprintln!(
                    "sovereign-cli-dev project-observe: unexpected argument '{other}'\n{USAGE}"
                );
                return 2;
            }
        }
    }
    let (Some(repo_root), Some(has_git)) = (repo_root, has_git) else {
        eprintln!("{USAGE}");
        return 2;
    };
    tracing::debug!(repo_root = %repo_root.display(), has_git, design_exists, git_declined, "project-observe");

    let mut observation = crate::observation::observe(&repo_root);
    // The caller may have just run `git init`, after this repo's git state
    // was last seen; its answer is the current one.
    observation.has_git = has_git;
    let report_ctx = ObservationReportContext { design_exists };
    print_observation_report(&observation, &report_ctx);

    // Read-modify-write: preserve any existing lifecycle fields so
    // re-running init after `svrn project found` doesn't reset
    // `founded`, `charter_version`, or `current_phase`.
    let project_toml_path = project_toml_path(&repo_root);
    if let Err(e) =
        std::fs::create_dir_all(project_toml_path.parent().unwrap_or_else(|| Path::new(".")))
    {
        eprintln!("    \u{2717} Cannot create .sovereign/: {e}");
        return 1;
    }
    let mut project_toml = ProjectTomlFile::read(&project_toml_path)
        .unwrap_or_else(|_| ProjectTomlFile::from_observation(&observation));
    project_toml.update_observation(&observation, &project_toml_path);
    // Persist a fresh git declination if the user just said "no" —
    // but preserve a prior declination (user already said no before).
    // Never un-set: once they've opted out, that stays opted out
    // until they run `git init` themselves.
    if git_declined {
        project_toml.lifecycle.git_declined_at_init = true;
    }
    if let Err(e) = project_toml.write(&project_toml_path) {
        eprintln!("    \u{2717} Cannot write project.toml: {e}");
    }
    0
}
/// Contextual flags the report uses to decide whether a missing
/// toolchain / git is ACTIONABLE (fix it now) or DEFERRED (we know
/// why it's fine). Keeps `print_observation_report` side-effect free
/// while letting `cmd_init` pass in what it knows.
struct ObservationReportContext {
    /// True when `<repo>/DESIGN.md` exists. A pre-code project with a
    /// design doc is a legitimate state — "no languages detected"
    /// becomes "indexing deferred" rather than an actionable error.
    design_exists: bool,
}

fn print_observation_report(obs: &ProjectObservation, ctx: &ObservationReportContext) {
    let mut ready: Vec<String> = Vec::new();
    let mut actionable: Vec<(String, &'static str)> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    // Languages & SCIP tooling. On a pre-code project with a design
    // doc present, "no languages" is expected — soft-path the
    // warning into the deferred bucket instead of treating it as a
    // gap the user must close right now.
    if obs.languages.is_empty() {
        if ctx.design_exists {
            deferred.push(
                "Pre-code project (DESIGN.md present, no source yet). Language detection runs on the next init."
                    .into(),
            );
        } else {
            actionable.push((
                "No supported languages detected (Rust, TypeScript, JavaScript, Go, Python, Java)."
                    .into(),
                "",
            ));
        }
    } else {
        for lang in &obs.languages {
            match &lang.scip_tooling {
                ScipTooling::Available { binary } => {
                    ready.push(format!("{} ({binary} on PATH)", lang.display));
                }
                ScipTooling::NotRequired => {
                    ready.push(lang.display.clone());
                }
                ScipTooling::Missing {
                    binary,
                    install_cmd,
                } => {
                    actionable.push((
                        format!(
                            "{} detected. Call-graph navigation requires `{binary}`:",
                            lang.display
                        ),
                        *install_cmd,
                    ));
                }
            }
        }
    }

    if obs.has_git {
        ready.push("Git repository".into());
    }

    if obs.embed_model_available {
        ready.push("Embed model".into());
    } else {
        actionable.push((
            "Embed model not found (documentation search will be degraded).".into(),
            "svrn setup",
        ));
    }

    // External dependencies — noted for `project found` (Stage 2
    // fault lines draws on this list). Not resolved at init time.
    let direct_deps: Vec<&DetectedDependency> = obs
        .deps
        .iter()
        .filter(|d| d.kind == DepKind::Direct)
        .collect();
    if !direct_deps.is_empty() {
        let n = direct_deps.len();
        deferred.push(format!(
            "{n} direct external dependenc{y} detected — surfaced to `svrn project found`.",
            y = if n == 1 { "y" } else { "ies" }
        ));
    }

    // Render.
    if !ready.is_empty() {
        println!();
        for r in &ready {
            println!("    \u{2713} {r}");
        }
    }

    if !actionable.is_empty() {
        println!();
        for (desc, cmd) in &actionable {
            println!("    \u{26a0} {desc}");
            if !cmd.is_empty() {
                println!();
                println!("{cmd}");
                println!();
            }
        }
    }

    if !deferred.is_empty() {
        println!();
        for d in &deferred {
            println!("    \u{2026} {d}");
        }
    }
}
