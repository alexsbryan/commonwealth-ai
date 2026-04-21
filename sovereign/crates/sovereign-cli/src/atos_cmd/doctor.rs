//! `sovereign atos doctor` — one-pass health check.
//!
//! Prints a per-check ✓ / ✗ / ⚠ line and exits 0 iff every check is ✓
//! or ⚠. The job is to tell the operator — or someone landing on a
//! new machine — exactly what in the ATOS surface is configured and
//! what isn't, without making them read the code.
//!
//! The checks are deliberately fast and non-mutating: open DBs
//! read-only-ish (SQLite may upgrade journal mode but that's
//! harmless), shell one HEAD at localhost:9741 with a 2s timeout.
//! No long-running probes, no network beyond localhost.

use std::path::{Path, PathBuf};

use corpus_engine::FeatureStore;

pub(super) async fn cmd_doctor(_args: &[String]) -> i32 {
    let mut report = DoctorReport::default();
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // 1. Repo root resolvable (prefer git; fall back to cwd).
    let git_root = resolve_git_root(&repo_root);
    match git_root.as_ref() {
        Some(p) => report.pass("repo root", p.display().to_string()),
        None => report.warn(
            "repo root",
            "git rev-parse failed; falling back to CWD (still works, just no git approvals)".into(),
        ),
    }
    let anchor = git_root.clone().unwrap_or_else(|| repo_root.clone());

    // 2. .sovereign/ directory present.
    let sov = anchor.join(".sovereign");
    if sov.is_dir() {
        report.pass(".sovereign directory", sov.display().to_string());
    } else {
        report.fail(
            ".sovereign directory",
            format!(
                "missing at {} — run `sovereign atos provision <id>` first",
                sov.display()
            ),
        );
    }

    // 3. notes.db reachable (opening applies the v4 migration as a
    //    side-effect, so success implies schema is caught up).
    let notes_db = sov.join("notes.db");
    match corpus_engine::NoteStore::open(&notes_db) {
        Ok(_) => report.pass("notes.db", "open + migrations OK".into()),
        Err(e) => report.fail("notes.db", format!("{e}")),
    }

    // 4. features.db reachable + feature count.
    let features_db = sov.join("features.db");
    match FeatureStore::open(&features_db) {
        Ok(store) => {
            let count = store.list(true).await.map(|v| v.len()).unwrap_or(0);
            report.pass(
                "features.db",
                format!("{count} feature{}", if count == 1 { "" } else { "s" }),
            );
            // 7. Per-feature checks.
            let features = store.list(false).await.unwrap_or_default();
            for f in features {
                check_feature(&mut report, &anchor, &f).await;
            }
        }
        Err(e) => report.fail("features.db", format!("{e}")),
    }

    // 5. Default pipelines loadable.
    let pipelines_ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        commonwealth_core::pipeline_aliases::PipelineAliasTable::default_table()
            .resolve("sovereign-coder")
            .is_some()
    }))
    .unwrap_or(false);
    if pipelines_ok {
        report.pass("default pipelines", "sovereign-coder resolves".into());
    } else {
        report.fail(
            "default pipelines",
            "default_pipelines.toml parse or resolve failed".into(),
        );
    }

    // 6. Opencode plugin present + version freshness.
    let plugin = anchor.join(crate::atos_plugin::plugin_rel_path());
    if !plugin.exists() {
        report.warn(
            "opencode plugin",
            format!(
                "{} not found — run `sovereign atos install-plugin`",
                plugin.display()
            ),
        );
    } else {
        match std::fs::read_to_string(&plugin) {
            Ok(contents) => {
                let installed = crate::atos_plugin::parse_installed_version(&contents);
                let expected = crate::atos_plugin::PLUGIN_VERSION;
                match installed.as_deref() {
                    Some(v) if v == expected => {
                        report.pass("opencode plugin", format!("v{v} (current)"));
                    }
                    Some(v) => {
                        report.warn(
                            "opencode plugin",
                            format!(
                                "v{v} on disk, v{expected} in this CLI — run `sovereign atos install-plugin`"
                            ),
                        );
                    }
                    None => {
                        report.warn(
                            "opencode plugin",
                            "present but unversioned (hand-authored or pre-embedded) — run `sovereign atos install-plugin`".into(),
                        );
                    }
                }
            }
            Err(e) => report.warn("opencode plugin", format!("unreadable: {e}")),
        }
    }

    // 8. Commonwealth daemon reachable (warn, not fail — dev mode
    //    valid).
    match probe_daemon_head().await {
        Ok(()) => report.pass("commonwealth daemon", "localhost:9741 responding".into()),
        Err(e) => report.warn(
            "commonwealth daemon",
            format!("localhost:9741: {e} (dev mode is fine)"),
        ),
    }

    // 9. Fast inference slot — inspect /oicp/v1/capabilities.
    match probe_fast_slot().await {
        Ok(true) => report.pass("fast inference slot", "capability present".into()),
        Ok(false) => report.warn("fast inference slot", "no fast-capable model registered".into()),
        Err(e) => report.warn("fast inference slot", format!("probe failed: {e}")),
    }

    report.print();
    if report.any_failed() {
        1
    } else {
        0
    }
}

async fn check_feature(
    report: &mut DoctorReport,
    repo_root: &Path,
    f: &corpus_engine::FeatureRow,
) {
    let label = format!("feature `{}`", f.id);

    // spec.md exists
    let spec_path = sovereign_atos::approval::spec_path(repo_root, &f.id);
    if !spec_path.exists() {
        report.fail(&label, format!("spec.md missing at {}", spec_path.display()));
        return;
    }

    // approval resolvable
    let mesh_path = repo_root.join(".sovereign").join("mesh.db");
    let mesh = commonwealth_state::MeshStore::open(&mesh_path).ok();
    let approval = sovereign_atos::approval::find_approval(repo_root, &f.id, mesh.as_ref());
    let Some(appr) = approval else {
        report.warn(
            &label,
            "unapproved — `git commit` the spec OR run `atos feature approve`".into(),
        );
        // Still report auto_redteam + other flags.
        report.info(
            &label,
            format!("auto_redteam={}  state={}", f.auto_redteam, f.state),
        );
        return;
    };

    // drift check
    if sovereign_atos::approval::detect_drift(&appr, repo_root) {
        report.warn(
            &label,
            format!(
                "spec.md drifted since approval — run `atos spec diff {}` or `atos spec accept {}`",
                f.id, f.id
            ),
        );
    } else {
        report.pass(
            &label,
            format!(
                "approved via {:?} (by {})",
                appr.source,
                short_identity(&appr.approved_by)
            ),
        );
    }

    report.info(
        &label,
        format!("auto_redteam={}  state={}", f.auto_redteam, f.state),
    );
}

fn resolve_git_root(cwd: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(PathBuf::from(s.trim()))
}

async fn probe_daemon_head() -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|e| format!("reqwest client: {e}"))?;
    let res = client
        .head("http://localhost:9741/v1/models")
        .send()
        .await
        .map_err(|e: reqwest::Error| e.to_string())?;
    if res.status().is_success() || res.status().is_redirection() {
        Ok(())
    } else {
        Err(format!("HTTP {}", res.status()))
    }
}

async fn probe_fast_slot() -> Result<bool, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|e| format!("reqwest client: {e}"))?;
    let res = client
        .get("http://localhost:9741/oicp/v1/capabilities")
        .send()
        .await
        .map_err(|e: reqwest::Error| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status()));
    }
    let body = res
        .text()
        .await
        .map_err(|e: reqwest::Error| e.to_string())?;
    Ok(body.to_lowercase().contains("fast"))
}

fn short_identity(s: &str) -> String {
    // "Name <email>" — keep just the Name.
    s.split('<').next().unwrap_or(s).trim().to_string()
}

#[derive(Default)]
struct DoctorReport {
    lines: Vec<DoctorLine>,
}

struct DoctorLine {
    kind: DoctorKind,
    check: String,
    detail: String,
}

#[derive(Clone, Copy, PartialEq)]
enum DoctorKind {
    Pass,
    Fail,
    Warn,
    Info,
}

impl DoctorReport {
    fn pass(&mut self, check: &str, detail: String) {
        self.lines.push(DoctorLine {
            kind: DoctorKind::Pass,
            check: check.into(),
            detail,
        });
    }
    fn fail(&mut self, check: &str, detail: String) {
        self.lines.push(DoctorLine {
            kind: DoctorKind::Fail,
            check: check.into(),
            detail,
        });
    }
    fn warn(&mut self, check: &str, detail: String) {
        self.lines.push(DoctorLine {
            kind: DoctorKind::Warn,
            check: check.into(),
            detail,
        });
    }
    fn info(&mut self, check: &str, detail: String) {
        self.lines.push(DoctorLine {
            kind: DoctorKind::Info,
            check: check.into(),
            detail,
        });
    }
    fn any_failed(&self) -> bool {
        self.lines.iter().any(|l| l.kind == DoctorKind::Fail)
    }
    fn print(&self) {
        let width = self
            .lines
            .iter()
            .map(|l| l.check.len())
            .max()
            .unwrap_or(0)
            .min(36);
        for l in &self.lines {
            let sym = match l.kind {
                DoctorKind::Pass => "✓",
                DoctorKind::Fail => "✗",
                DoctorKind::Warn => "⚠",
                DoctorKind::Info => " ",
            };
            println!("{sym} {:width$}  {}", l.check, l.detail, width = width);
        }
    }
}
