// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage 1 DISCOVER — deterministic and exhaustive.
//!
//! Apply the seed edit, run `cargo check --message-format=json`, read `file`,
//! `line`, `byte_start/end`, `expected`, `found` off every diagnostic, restore
//! the tree. For a type change THE COMPILER IS THE EXHAUSTIVE SITE ENUMERATOR
//! within each crate that actually gets checked; a crate whose dependency
//! failed NEVER RAN and is reported as exactly that — four verdicts, not two
//! (ARCH §18.1). SCIP/mention scans give the pre-flight estimate; rustc gives
//! the truth.
//!
//! The cargo invocation copies the lint gate's feature contract
//! (`scripts/sovereign-lint.sh`): `--all-targets` plus
//! `corpus-engine/treesitter` (when in the closure) and the leaf-crate flags —
//! one feature contract, one decider (ARCH §10.6). Diagnostics from a bare
//! default-feature check would be about a tree the gates never build.

use super::census;
use super::classify;
use super::gate;
use super::spec::RefactorSpec;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub struct PlanOptions {
    pub spec_path: PathBuf,
    pub crate_filter: Option<String>,
    pub sites_per_class: usize,
    pub run_fixture: bool,
    pub run_baseline: bool,
    pub json: bool,
}

// ── Workspace metadata ──────────────────────────────────────────────────────

pub struct PackageMeta {
    pub name: String,
    pub dir: PathBuf,
    /// Names of *workspace-member* dependencies (normal deps only).
    pub member_deps: Vec<String>,
}

pub struct WorkspaceMeta {
    pub packages: Vec<PackageMeta>,
}

impl WorkspaceMeta {
    pub fn load(root: &Path) -> Result<Self, String> {
        let out = std::process::Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(root)
            .output()
            .map_err(|e| format!("running cargo metadata: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("parsing cargo metadata: {e}"))?;
        let raw = v["packages"].as_array().cloned().unwrap_or_default();
        let member_names: BTreeSet<String> = raw
            .iter()
            .filter_map(|p| p["name"].as_str().map(String::from))
            .collect();
        let packages = raw
            .iter()
            .filter_map(|p| {
                let name = p["name"].as_str()?.to_string();
                let dir = PathBuf::from(p["manifest_path"].as_str()?)
                    .parent()?
                    .to_path_buf();
                let member_deps = p["dependencies"]
                    .as_array()
                    .map(|deps| {
                        deps.iter()
                            .filter(|d| d["kind"].is_null()) // normal deps only
                            .filter_map(|d| d["name"].as_str())
                            .filter(|n| member_names.contains(*n))
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                Some(PackageMeta {
                    name,
                    dir,
                    member_deps,
                })
            })
            .collect();
        Ok(WorkspaceMeta { packages })
    }

    pub fn get(&self, name: &str) -> Option<&PackageMeta> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// Owning workspace package of a file, by longest manifest-dir prefix.
    pub fn package_for_file(&self, path: &Path) -> Option<&PackageMeta> {
        self.packages
            .iter()
            .filter(|p| path.starts_with(&p.dir))
            .max_by_key(|p| p.dir.components().count())
    }

    /// Transitive workspace-member dependency closure of `name`.
    pub fn transitive_deps(&self, name: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![name.to_string()];
        while let Some(n) = stack.pop() {
            if let Some(p) = self.get(&n) {
                for d in &p.member_deps {
                    if seen.insert(d.clone()) {
                        stack.push(d.clone());
                    }
                }
            }
        }
        seen
    }
}

// ── The seed edit + restore guard ───────────────────────────────────────────

struct SeededFile {
    path: PathBuf,
    original: String,
    seeded: String,
    sites: usize,
}

/// Holds every seeded file's original content and restores on drop. If a file
/// changed under us mid-run (a peer edit), the guard REFUSES to clobber it:
/// the original is parked next to it as `<name>.rf-orig` and the conflict is
/// named loudly. Absence of a clean restore is reported, never defaulted
/// (ARCH §18.3).
struct TreeGuard {
    files: Vec<SeededFile>,
    restored: bool,
}

impl TreeGuard {
    /// Files that could NOT be restored cleanly.
    fn restore(&mut self) -> Vec<PathBuf> {
        if self.restored {
            return Vec::new();
        }
        self.restored = true;
        let mut conflicts = Vec::new();
        for f in &self.files {
            match std::fs::read_to_string(&f.path) {
                Ok(current) if current == f.seeded => {
                    if std::fs::write(&f.path, &f.original).is_err() {
                        conflicts.push(f.path.clone());
                    }
                }
                // The guard enrolls a file BEFORE writing it, so a seed run
                // that failed before this file's write leaves it untouched —
                // nothing to restore, and NOT a conflict.
                Ok(current) if current == f.original => {}
                _ => {
                    // Peer edit during our window: park the original, keep theirs.
                    let park = f.path.with_extension("rs.rf-orig");
                    let _ = std::fs::write(&park, &f.original);
                    eprintln!(
                        "!! {} changed while seeded — NOT restored; pre-seed content parked at {}",
                        f.path.display(),
                        park.display()
                    );
                    conflicts.push(f.path.clone());
                }
            }
        }
        conflicts
    }
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        let conflicts = self.restore();
        if !conflicts.is_empty() {
            eprintln!(
                "!! seed restore left {} conflicted file(s) — resolve by hand",
                conflicts.len()
            );
        }
    }
}

fn apply_seed(root: &Path, spec: &RefactorSpec, files: &[PathBuf]) -> Result<TreeGuard, String> {
    let line_re = Regex::new(&format!(
        r"^(\s*(?:pub(?:\s*\([^)]*\))?\s+)?{}:\s*){}(\s*,?\s*)$",
        regex::escape(&spec.discover.seed.field),
        regex::escape(&spec.discover.seed.from)
    ))
    .expect("escaped dynamic parts");
    let replacement = format!("${{1}}{}${{2}}", spec.target);

    // The guard exists BEFORE the first write and every file is enrolled in
    // it before its own write. A mid-seed failure (disk full is the proven
    // case) therefore drops a guard that already owns every touched file:
    // fully-written files restore, the untouched tail no-ops, and a truncated
    // partial write parks its original as a sidecar and is named loudly.
    let mut guard = TreeGuard {
        files: Vec::new(),
        restored: false,
    };
    for path in files {
        let original = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let mut sites = 0usize;
        let new_lines: Vec<String> = original
            .lines()
            .map(|l| {
                if line_re.is_match(l) {
                    sites += 1;
                    line_re.replace(l, replacement.as_str()).into_owned()
                } else {
                    l.to_string()
                }
            })
            .collect();
        if sites == 0 {
            continue;
        }
        let mut content = new_lines.join("\n");
        if original.ends_with('\n') {
            content.push('\n');
        }
        guard.files.push(SeededFile {
            path: path.clone(),
            original,
            seeded: content,
            sites,
        });
        let f = guard.files.last().expect("just pushed");
        // On Err the guard (which already owns this file) drops and restores
        // everything written so far.
        std::fs::write(path, &f.seeded).map_err(|e| format!("seeding {}: {e}", path.display()))?;
        tracing::debug!(
            target: "refactor",
            file = %path.strip_prefix(root).unwrap_or(path).display(),
            sites,
            "seeded"
        );
    }
    Ok(guard)
}

// ── cargo check → diagnostics ───────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct RawDiagnostic {
    pub code: String,
    pub message: String,
    pub file: String,
    pub line: u64,
    pub column: u64,
    pub byte_start: u64,
    pub byte_end: u64,
    pub expected: Option<String>,
    pub found: Option<String>,
    /// Concatenated label/children text the context tag was derived from.
    pub context_text: String,
    pub package: String,
}

/// The lint gate's feature contract (`scripts/sovereign-lint.sh` §5), scoped
/// to the current selection. `pkg/feature` is an error for a package outside
/// the selection's closure, so each flag is gated on the closure.
fn feature_contract(meta: &WorkspaceMeta, crate_filter: Option<&str>) -> String {
    let mut features: Vec<String> = Vec::new();
    let in_closure = |pkg: &str| -> bool {
        match crate_filter {
            None => meta.get(pkg).is_some(),
            Some(sel) => sel == pkg || meta.transitive_deps(sel).contains(pkg),
        }
    };
    if in_closure("corpus-engine") {
        features.push("corpus-engine/treesitter".into());
    }
    if (crate_filter.is_none() || crate_filter == Some("sovereign-cli"))
        && meta.get("sovereign-cli").is_some()
    {
        features.push("sovereign-cli/dev-tools".into());
        features.push("sovereign-cli/code-intel".into());
        features.push("sovereign-cli/awareness".into());
    }
    if (crate_filter.is_none() || crate_filter == Some("sovereign-mesh"))
        && meta.get("sovereign-mesh").is_some()
    {
        features.push("sovereign-mesh/mesh-sim".into());
    }
    features.join(",")
}

pub struct CheckRun {
    pub diagnostics: Vec<RawDiagnostic>,
    pub command: String,
    pub exit_code: i32,
}

fn run_cargo_check(
    root: &Path,
    meta: &WorkspaceMeta,
    crate_filter: Option<&str>,
) -> Result<CheckRun, String> {
    let mut args: Vec<String> = vec!["check".into()];
    match crate_filter {
        Some(c) => {
            args.push("-p".into());
            args.push(c.into());
        }
        None => args.push("--workspace".into()),
    }
    args.push("--all-targets".into());
    args.push("--keep-going".into());
    args.push("--message-format=json".into());
    let features = feature_contract(meta, crate_filter);
    if !features.is_empty() {
        args.push("--features".into());
        args.push(features);
    }
    let command = format!("cargo {}", args.join(" "));
    tracing::info!(target: "refactor", %command, "discover: running");
    let out = std::process::Command::new("cargo")
        .args(&args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("running cargo check: {e}"))?;

    let mut diagnostics = Vec::new();
    let mut seen: BTreeSet<(String, u64, u64, String, String)> = BTreeSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v["reason"].as_str() != Some("compiler-message") {
            continue;
        }
        let msg = &v["message"];
        if msg["level"].as_str() != Some("error") {
            continue;
        }
        let text = msg["message"].as_str().unwrap_or_default().to_string();
        if text.starts_with("aborting due to") {
            continue;
        }
        let code = msg["code"]["code"].as_str().unwrap_or("none").to_string();
        let package = package_name(v["package_id"].as_str().unwrap_or_default())
            .unwrap_or_else(|| v["target"]["name"].as_str().unwrap_or("?").to_string());
        let spans = msg["spans"].as_array().cloned().unwrap_or_default();
        let primary = spans
            .iter()
            .find(|s| s["is_primary"].as_bool() == Some(true))
            .or_else(|| spans.first());
        let (file, line_no, column, byte_start, byte_end) = match primary {
            Some(s) => (
                s["file_name"].as_str().unwrap_or("?").to_string(),
                s["line_start"].as_u64().unwrap_or(0),
                s["column_start"].as_u64().unwrap_or(0),
                s["byte_start"].as_u64().unwrap_or(0),
                s["byte_end"].as_u64().unwrap_or(0),
            ),
            None => ("?".to_string(), 0, 0, 0, 0),
        };
        // `--all-targets` compiles a lib for itself and for its test harness:
        // the same source error is reported once per target. One site, one
        // diagnostic.
        if !seen.insert((file.clone(), line_no, column, code.clone(), text.clone())) {
            continue;
        }
        let mut context_text = String::new();
        for s in &spans {
            if let Some(l) = s["label"].as_str() {
                context_text.push_str(l);
                context_text.push('\n');
            }
        }
        for c in msg["children"].as_array().unwrap_or(&Vec::new()) {
            if let Some(m) = c["message"].as_str() {
                context_text.push_str(m);
                context_text.push('\n');
            }
        }
        let (expected, found) = classify::extract_expected_found(&text, &context_text);
        diagnostics.push(RawDiagnostic {
            code,
            message: text,
            file,
            line: line_no,
            column,
            byte_start,
            byte_end,
            expected,
            found,
            context_text,
            package,
        });
    }
    Ok(CheckRun {
        diagnostics,
        command,
        exit_code: out.status.code().unwrap_or(-1),
    })
}

/// `path+file:///…#kernel-types@0.5.0` → `kernel-types`;
/// `path+file:///…/kernel-types#0.5.0` → `kernel-types`.
fn package_name(package_id: &str) -> Option<String> {
    let frag = package_id.rsplit('#').next()?;
    if let Some((name, _ver)) = frag.split_once('@') {
        return Some(name.to_string());
    }
    // Fragment is a bare version: the name is the last path segment.
    let head = package_id.split('#').next()?;
    head.rsplit('/').next().map(String::from)
}

// ── The plan orchestration ──────────────────────────────────────────────────

/// Per-crate discover verdict — four verdicts, not two (ARCH §18.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrateVerdict {
    /// The check ran and reported errors — enumeration is complete here.
    Errors,
    /// The check ran; no diagnostics landed in this crate.
    Clean,
    /// A workspace dependency failed, so this crate NEVER RAN this pass.
    /// (Files outside the cargo workspace are reported separately — no check
    /// can see them at all.)
    NeverRan,
}

pub async fn run_plan(opts: &PlanOptions) -> i32 {
    let root = match census::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let spec = match RefactorSpec::load(&opts.spec_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let meta = match WorkspaceMeta::load(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return 3;
        }
    };
    if let Some(c) = &opts.crate_filter {
        if meta.get(c).is_none() {
            eprintln!("error: '{c}' is not a workspace package");
            return 1;
        }
    }

    if !opts.json {
        println!(
            "refactor plan — spec '{}' (kind {}, target {})",
            spec.id,
            spec.kind.as_str(),
            spec.target
        );
        println!();
    }

    // ── Entry gate first: an item failing it is NOT discovered ──────────
    let gate_report = gate::gate_spec(&root, &meta, &spec, opts.run_fixture);
    if !opts.json {
        print!("{}", gate_report.render());
        println!();
    }
    if !gate_report.passed() {
        if opts.json {
            let out = serde_json::json!({
                "spec": spec.id,
                "gate": gate_report,
                "verdict": "refused",
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        } else {
            println!(
                "ENTRY GATE REFUSED '{}' — not discovered, not scheduled. File it as a finding.",
                spec.id
            );
        }
        return 2;
    }

    // ── Baseline: the instrument is only valid over a green tree ────────
    if opts.run_baseline {
        match run_cargo_check(&root, &meta, opts.crate_filter.as_deref()) {
            // Green needs BOTH: no diagnostics AND exit 0. A build-script or
            // link failure emits no rustc diagnostics but exits non-zero —
            // reading that as green is the exact failure the lint gate fixed
            // on 2026-07-28 (note 73bb9404).
            Ok(run) if run.diagnostics.is_empty() && run.exit_code == 0 => {
                if !opts.json {
                    println!("baseline: green ({})", run.command);
                }
            }
            Ok(run) if run.diagnostics.is_empty() => {
                eprintln!(
                    "error: baseline check exited {} with no rustc diagnostics — a build-script \
                     or link failure; could not judge. Run `{}` by hand.",
                    run.exit_code, run.command
                );
                return 3;
            }
            Ok(run) => {
                eprintln!(
                    "error: baseline check has {} pre-existing error(s) — discover would \
                     misattribute them to the seed. First: {} ({} {}:{}). Fix the tree or \
                     pass --skip-baseline.",
                    run.diagnostics.len(),
                    run.diagnostics[0].message,
                    run.diagnostics[0].code,
                    run.diagnostics[0].file,
                    run.diagnostics[0].line,
                );
                return 3;
            }
            Err(e) => {
                eprintln!("error: baseline check: {e}");
                return 3;
            }
        }
    } else if !opts.json {
        println!("baseline: SKIPPED on request — errors below may predate the seed");
    }

    // ── Seed ────────────────────────────────────────────────────────────
    let mut files = census::walk_rs_files(&root, census::EXCLUDE_DIRS_DECL);
    if let Some(c) = &opts.crate_filter {
        let dir = meta.get(c).map(|p| p.dir.clone()).unwrap_or_default();
        files.retain(|f| f.starts_with(&dir));
    }
    let mut guard = match apply_seed(&root, &spec, &files) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return 3;
        }
    };
    let seeded_sites: usize = guard.files.iter().map(|f| f.sites).sum();
    let seeded_files = guard.files.len();
    if seeded_sites == 0 {
        eprintln!(
            "error: seed `{}: {}` matched no declaration in scope — nothing to discover",
            spec.discover.seed.field, spec.discover.seed.from
        );
        return 3;
    }
    if !opts.json {
        println!(
            "seed: retyped {seeded_sites} `{}: {}` declaration(s) across {seeded_files} file(s) -> {}",
            spec.discover.seed.field, spec.discover.seed.from, spec.target
        );
    }

    // ── Check, then restore BEFORE reporting ────────────────────────────
    let check = run_cargo_check(&root, &meta, opts.crate_filter.as_deref());
    let seeded_packages: BTreeSet<String> = guard
        .files
        .iter()
        .filter_map(|f| meta.package_for_file(&f.path).map(|p| p.name.clone()))
        .collect();
    let outside: Vec<PathBuf> = guard
        .files
        .iter()
        .filter(|f| meta.package_for_file(&f.path).is_none())
        .map(|f| f.path.clone())
        .collect();
    let conflicts = guard.restore();
    drop(guard);
    if !conflicts.is_empty() {
        eprintln!(
            "error: restore left {} conflicted file(s) (named above) — resolve before trusting \
             the tree",
            conflicts.len()
        );
        // Keep going: the diagnostics are still valid; the tree problem is loud.
    }

    let check = match check {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: seeded check: {e}");
            return 3;
        }
    };
    if check.diagnostics.is_empty() && check.exit_code != 0 {
        eprintln!(
            "error: seeded check exited {} with no rustc diagnostics — a build-script or link \
             failure, not an enumeration; could not judge",
            check.exit_code
        );
        return 3;
    }

    // ── Per-crate verdicts ──────────────────────────────────────────────
    let errored: BTreeSet<String> = check
        .diagnostics
        .iter()
        .map(|d| d.package.clone())
        .collect();
    let mut crate_verdicts: BTreeMap<String, CrateVerdict> = BTreeMap::new();
    for pkg in &seeded_packages {
        let v = if errored.contains(pkg) {
            CrateVerdict::Errors
        } else if meta
            .transitive_deps(pkg)
            .iter()
            .any(|d| errored.contains(d))
        {
            CrateVerdict::NeverRan
        } else {
            CrateVerdict::Clean
        };
        crate_verdicts.insert(pkg.clone(), v);
    }
    // Errors can land in crates we never seeded (cross-crate constructors).
    for pkg in &errored {
        crate_verdicts
            .entry(pkg.clone())
            .or_insert(CrateVerdict::Errors);
    }

    // ── Classify (stage 2) and render ───────────────────────────────────
    let classification = classify::classify(&check.diagnostics, &spec);
    if opts.json {
        let out = serde_json::json!({
            "spec": spec.id,
            "target": spec.target,
            "gate": gate_report,
            "discover": {
                "command": check.command,
                "seeded_sites": seeded_sites,
                "seeded_files": seeded_files,
                "outside_workspace": outside,
                "crate_verdicts": crate_verdicts,
                "restore_conflicts": conflicts,
            },
            "classes": classification.classes,
            "totals": classification.totals(),
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        println!();
        print!(
            "{}",
            classification.render(opts.sites_per_class, &crate_verdicts, &outside)
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_file(
        dir: &Path,
        name: &str,
        on_disk: &str,
        original: &str,
        seeded: &str,
    ) -> SeededFile {
        let path = dir.join(name);
        std::fs::write(&path, on_disk).unwrap();
        SeededFile {
            path,
            original: original.to_string(),
            seeded: seeded.to_string(),
            sites: 1,
        }
    }

    #[test]
    fn restore_rolls_back_seeded_and_noops_the_unwritten_tail() {
        // The mid-seed failure shape: file A was written, file B was enrolled
        // but the crash came first. A restores, B is not a conflict.
        let dir = tempfile::tempdir().unwrap();
        let a = seeded_file(dir.path(), "a.rs", "SEEDED", "ORIG", "SEEDED");
        let b = seeded_file(dir.path(), "b.rs", "ORIG", "ORIG", "SEEDED");
        let (pa, pb) = (a.path.clone(), b.path.clone());
        let mut guard = TreeGuard {
            files: vec![a, b],
            restored: false,
        };
        let conflicts = guard.restore();
        assert!(conflicts.is_empty(), "no conflicts expected: {conflicts:?}");
        assert_eq!(std::fs::read_to_string(&pa).unwrap(), "ORIG");
        assert_eq!(std::fs::read_to_string(&pb).unwrap(), "ORIG");
    }

    #[test]
    fn restore_refuses_to_clobber_a_peer_edit_and_parks_the_original() {
        // A peer (or a truncating failed write) changed the file while it was
        // seeded: the guard must NOT overwrite their content. The pre-seed
        // original is parked as a sidecar so nothing is lost.
        let dir = tempfile::tempdir().unwrap();
        let c = seeded_file(dir.path(), "c.rs", "PEER EDIT", "ORIG", "SEEDED");
        let pc = c.path.clone();
        let mut guard = TreeGuard {
            files: vec![c],
            restored: false,
        };
        let conflicts = guard.restore();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(std::fs::read_to_string(&pc).unwrap(), "PEER EDIT");
        let park = pc.with_extension("rs.rf-orig");
        assert_eq!(std::fs::read_to_string(&park).unwrap(), "ORIG");
    }

    #[test]
    fn restore_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let a = seeded_file(dir.path(), "a.rs", "SEEDED", "ORIG", "SEEDED");
        let mut guard = TreeGuard {
            files: vec![a],
            restored: false,
        };
        assert!(guard.restore().is_empty());
        assert!(guard.restore().is_empty(), "second restore is a no-op");
    }
}
