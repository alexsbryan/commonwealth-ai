// SPDX-License-Identifier: AGPL-3.0-or-later
//! A release install carries every binary its verbs exec
//! (pb-distribution-release-bins, ship gate F1).
//!
//! The source of truth is the exec sites themselves: every
//! `locate_sibling(...)` in the dispatcher, in `sovereign-cli-daemon` (which
//! `daemon run` and `setup` reach) and in `sovereign-cli-mesh` (whose
//! `mesh up` brings cw-rails up). Each named binary must be in BOTH release
//! lists — `landing/install.sh`'s `BINS` and `scripts/release-cli-local.sh`'s
//! `BINS` — and its owning package in the latter's `PKGS`, or a release
//! install answers the verb with "cannot find sibling binary".
//!
//! A monorepo-wide census (it reads three crates' sources and two shell
//! scripts), so it lives here in xtask, in no package: in sovereign-cli it
//! climbed out of the crate root, which boundary-gate rule 3c refuses of a
//! test a lifted package carries (pb-distribution-svrn-lift).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[path = "shared/repo_root.rs"]
mod repo_root;
use repo_root::repo_root;

/// The packages whose exec sites a release install reaches.
const DISPATCHING: &[&str] = &[
    "sovereign/crates/sovereign-cli/src",
    "sovereign/crates/sovereign-cli-daemon/src",
    "sovereign/crates/sovereign-cli-mesh/src",
];

/// Exec'd only behind a verb the release dispatcher refuses before dispatch.
const DEV_ONLY: &[(&str, &str)] = &[(
    "sovereign-agent-bench",
    "`agent-bench` is in DEV_VERBS (main.rs) and its arm is cfg(dev-tools)",
)];

/// Where cargo puts the packages that own the binaries.
const CRATE_ROOTS: &[&str] = &["sovereign/crates", "commonwealth/crates"];

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                rust_sources(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" {
            out.push(path);
        }
    }
}

/// The binary name each `locate_sibling(` in `src` names: a string literal,
/// or a `const NAME: &str = "..."` in the same file.
fn sibling_names(file: &Path, src: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (at, _) in src.match_indices("locate_sibling(") {
        let arg = src[at + "locate_sibling(".len()..].trim_start();
        let name = if let Some(lit) = arg.strip_prefix('"') {
            lit[..lit.find('"').unwrap()].to_string()
        } else {
            let ident: String = arg
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let decl = format!("const {ident}: &str = \"");
            let at = src.find(&decl).unwrap_or_else(|| {
                panic!(
                    "{}: locate_sibling({ident}, ..) names no `{decl}..\"` in the same file; \
                     teach release_bins.rs the new form",
                    file.display()
                )
            });
            let lit = &src[at + decl.len()..];
            lit[..lit.find('"').unwrap()].to_string()
        };
        names.push(name);
    }
    names
}

/// Every binary a release install's verbs exec, the dispatcher included.
fn exec_closure(root: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::from(["sovereign-cli".to_string()]);
    for dir in DISPATCHING {
        let mut files = Vec::new();
        rust_sources(&root.join(dir), &mut files);
        for file in files {
            let src = std::fs::read_to_string(&file).unwrap();
            found.extend(sibling_names(&file, &src));
        }
    }
    for (bin, why) in DEV_ONLY {
        assert!(
            found.remove(*bin),
            "DEV_ONLY names {bin} ({why}), but no exec site names it any more; drop the entry"
        );
    }
    found
}

/// The words of the shell list assigned on the line starting `prefix`,
/// up to `close`.
fn shell_list(path: &Path, prefix: &str, close: char) -> BTreeSet<String> {
    let text = std::fs::read_to_string(path).unwrap();
    let line = text
        .lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("{}: no line starting `{prefix}`", path.display()));
    let body = &line[prefix.len()..];
    body[..body.find(close).unwrap()]
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// The package that builds `bin`: a `[[bin]] name`, or the package's own
/// name when it has a `src/main.rs`.
fn owning_package(root: &Path, bin: &str) -> String {
    for crates in CRATE_ROOTS {
        for entry in std::fs::read_dir(root.join(crates)).unwrap() {
            let dir = entry.unwrap().path();
            let Ok(manifest) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
                continue;
            };
            let mut package = None;
            let mut section = "";
            for line in manifest.lines().map(str::trim) {
                if line.starts_with('[') {
                    section = line;
                    continue;
                }
                let Some(value) = line.strip_prefix("name = \"") else {
                    continue;
                };
                let value = &value[..value.find('"').unwrap()];
                match section {
                    "[package]" => package = Some(value.to_string()),
                    "[[bin]]" if value == bin => return package.unwrap(),
                    _ => {}
                }
            }
            if package.as_deref() == Some(bin) && dir.join("src/main.rs").is_file() {
                return bin.to_string();
            }
        }
    }
    panic!("no package under {CRATE_ROOTS:?} builds a binary named {bin}")
}

/// The text of the CI workflow step named `name`, up to the next step.
fn workflow_step<'a>(workflow: &'a str, name: &str) -> &'a str {
    let head = format!("- name: {name}\n");
    let at = workflow
        .find(&head)
        .unwrap_or_else(|| panic!("cli-release.yml: no step `{name}`"))
        + head.len();
    let rest = &workflow[at..];
    let end = ["- name: ", "- uses: "]
        .iter()
        .filter_map(|s| rest.find(s))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

/// The words of `step` from `start` to `stop`, with `"${BINS[@]}"` expanded
/// to the release script's BINS and `"${PKGS[@]}"` to `-p <pkg>` per PKGS,
/// when the step reads them from it — the one list both release paths build
/// from (pb-distribution-f5-release-ci).
fn workflow_words(
    step: &str,
    start: &str,
    stop: &str,
    bins: &BTreeSet<String>,
    pkgs: &BTreeSet<String>,
) -> Vec<String> {
    let reads_script = step.contains("scripts/release-cli-local.sh");
    let at = step
        .find(start)
        .unwrap_or_else(|| panic!("cli-release.yml: no `{start}` in step:\n{step}"))
        + start.len();
    let body = &step[at..];
    let body = &body[..body.find(stop).unwrap_or(body.len())];
    let mut words = Vec::new();
    for word in body.split_whitespace() {
        let list: Option<Vec<String>> = if word.contains("${BINS[@]}") {
            Some(bins.iter().cloned().collect())
        } else if word.contains("${PKGS[@]}") {
            Some(
                pkgs.iter()
                    .flat_map(|p| ["-p".to_string(), p.clone()])
                    .collect(),
            )
        } else {
            None
        };
        match list {
            Some(list) => {
                assert!(
                    reads_script,
                    "cli-release.yml expands `{word}` without reading scripts/release-cli-local.sh"
                );
                words.extend(list);
            }
            None => words.push(word.trim_matches('"').to_string()),
        }
    }
    words
}

#[test]
fn release_lists_carry_every_exec_d_binary() {
    let root = repo_root();
    let needed = exec_closure(&root);
    let install = shell_list(&root.join("landing/install.sh"), "BINS=\"", '"');
    let release = root.join("scripts/release-cli-local.sh");
    let release_bins = shell_list(&release, "BINS=(", ')');
    let release_pkgs = shell_list(&release, "PKGS=(", ')');
    let workflow = std::fs::read_to_string(root.join(".github/workflows/cli-release.yml")).unwrap();
    let build = workflow_words(
        workflow_step(&workflow, "Build CLI binaries"),
        "cargo build",
        "\n\n",
        &release_bins,
        &release_pkgs,
    );
    let ci_pkgs: BTreeSet<String> = build
        .iter()
        .zip(build.iter().skip(1))
        .filter(|(flag, _)| *flag == "-p")
        .map(|(_, pkg)| pkg.clone())
        .collect();
    let ci_bins: BTreeSet<String> = workflow_words(
        workflow_step(&workflow, "Package tarball"),
        "for b in ",
        "; do",
        &release_bins,
        &release_pkgs,
    )
    .into_iter()
    .collect();

    let mut missing = Vec::new();
    for bin in &needed {
        if !install.contains(bin) {
            missing.push(format!("{bin}: not in landing/install.sh BINS"));
        }
        if !release_bins.contains(bin) {
            missing.push(format!("{bin}: not in scripts/release-cli-local.sh BINS"));
        }
        let package = owning_package(&root, bin);
        if !release_pkgs.contains(&package) {
            missing.push(format!(
                "{bin}: its package {package} is not in scripts/release-cli-local.sh PKGS"
            ));
        }
        if !ci_bins.contains(bin) {
            missing.push(format!("{bin}: not packaged by cli-release.yml"));
        }
        if !ci_pkgs.contains(&package) {
            missing.push(format!(
                "{bin}: its package {package} is not built by cli-release.yml"
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "a release install cannot exec what it does not carry (exec'd: {needed:?}):\n  {}",
        missing.join("\n  ")
    );
}
