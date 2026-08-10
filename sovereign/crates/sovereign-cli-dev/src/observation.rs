// SPDX-License-Identifier: AGPL-3.0-or-later
//! Project observation — everything `svrn project init` can
//! learn about a directory without asking the user a single question.
//!
//! The output is a pure-data [`ProjectObservation`] that
//! downstream code (init's printer, the project.toml writer,
//! `project found`'s Stage-1 "what can I derive" pass) can consume
//! without re-running detection.
//!
//! ## Invariants
//!
//! - **Non-interactive.** This module never reads stdin, never
//!   prompts, never writes. It returns facts.
//! - **Fast.** Under 200ms on a clean repo. No recursion-heavy
//!   filesystem walks; dep files are parsed with a cap.
//! - **Language-agnostic where possible.** Dep detection uses
//!   per-language parsers but produces the same
//!   [`DetectedDependency`] shape so downstream code doesn't
//!   branch on language at the consumption site.
//! - **No network.** Embed-model availability is a local check; if
//!   we ever need to hit a registry that's a separate seam (and
//!   probably a `Gap` that flows through the honesty protocol).

use std::path::Path;

use corpus_engine_scip::scip_export;

// ─── Data ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProjectObservation {
    pub has_git: bool,
    pub languages: Vec<LanguageObservation>,
    pub deps: Vec<DetectedDependency>,
    pub embed_model_available: bool,
}

#[derive(Debug, Clone)]
pub struct LanguageObservation {
    /// Stable id used throughout sovereign: `rust`, `go`, `typescript`,
    /// `javascript`, `python`, `java`.
    pub id: String,
    /// Human-readable tag — may include workspace detail
    /// ("Rust workspace (12 crates)").
    pub display: String,
    pub scip_tooling: ScipTooling,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScipTooling {
    /// The tool is on PATH and ready to index.
    Available { binary: &'static str },
    /// The language is present but the indexer binary is missing.
    /// `install_cmd` is the copy-pasteable one-liner.
    Missing {
        binary: &'static str,
        install_cmd: &'static str,
    },
    /// Rust's indexer is built into the tree-sitter path (no external
    /// binary), or the language is a degenerate case. No action needed.
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedDependency {
    pub name: String,
    pub version: Option<String>,
    pub source_file: String,
    pub kind: DepKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Direct,
    Dev,
}

// ─── Gather ──────────────────────────────────────────────────────────────────

/// Observe `repo_root`. Pure with respect to the filesystem (reads
/// only — no writes, no network).
pub fn observe(repo_root: &Path) -> ProjectObservation {
    let has_git = repo_root.join(".git").exists();
    let languages = detect_languages(repo_root);
    let deps = gather_deps(repo_root);
    let embed_model_available = embed_model_available();
    ProjectObservation {
        has_git,
        languages,
        deps,
        embed_model_available,
    }
}

// ─── Language + SCIP tooling ─────────────────────────────────────────────────

fn detect_languages(root: &Path) -> Vec<LanguageObservation> {
    let mut out = Vec::new();

    if root.join("Cargo.toml").exists() {
        out.push(LanguageObservation {
            id: "rust".into(),
            display: describe_rust(root),
            // Rust indexing uses the built-in tree-sitter pipeline;
            // the SCIP exporter's config for Rust points at a
            // rust-analyzer path that's documentation-only in our
            // stack today. Report NotRequired so we don't nag.
            scip_tooling: ScipTooling::NotRequired,
        });
    }

    // TypeScript vs JavaScript: any `tsconfig*.json` → TS; otherwise
    // `package.json` + any `.ts` under the workspace → TS; bare
    // package.json without .ts → JS.
    if root.join("tsconfig.json").exists() || root.join("tsconfig.base.json").exists() {
        out.push(language_with_scip("typescript", "TypeScript"));
    } else if root.join("package.json").exists() {
        if has_file_with_ext(root, "ts", 2) {
            out.push(language_with_scip("typescript", "TypeScript"));
        } else {
            out.push(language_with_scip("javascript", "JavaScript"));
        }
    }

    if root.join("go.mod").exists() {
        out.push(language_with_scip("go", "Go"));
    }

    if root.join("pyproject.toml").exists()
        || root.join("setup.py").exists()
        || root.join("requirements.txt").exists()
    {
        out.push(language_with_scip("python", "Python"));
    }

    // Java detection is minimal: a top-level pom.xml or build.gradle.
    // We support it because `scip-java` is in the exporter table; if
    // the project is a polyglot JVM repo we'll probably miss some
    // nuance — fine, init is not trying to be authoritative about
    // Java build systems.
    if root.join("pom.xml").exists() || root.join("build.gradle").exists() {
        out.push(language_with_scip("java", "Java"));
    }

    out
}

fn language_with_scip(id: &str, display: &str) -> LanguageObservation {
    LanguageObservation {
        id: id.into(),
        display: display.into(),
        scip_tooling: scip_tooling_for(id),
    }
}

/// Map a language id to the corpus-engine exporter config and check
/// whether its binary is on PATH. Returns `NotRequired` when the
/// language has no external indexer in our table.
pub fn scip_tooling_for(language_id: &str) -> ScipTooling {
    let exporter = scip_export::all_exporters()
        .iter()
        .find(|e| e.language_id == language_id);
    let Some(exporter) = exporter else {
        return ScipTooling::NotRequired;
    };
    // Rust's entry points at rust-analyzer which we don't actually
    // invoke — the built-in tree-sitter path handles Rust. Keep
    // Rust out of the nag surface explicitly.
    if language_id == "rust" {
        return ScipTooling::NotRequired;
    }
    if which::which(exporter.command).is_ok() {
        ScipTooling::Available {
            binary: exporter.command,
        }
    } else {
        ScipTooling::Missing {
            binary: exporter.command,
            install_cmd: strip_install_prefix(exporter.install_hint),
        }
    }
}

/// Strip the conventional "Install with: " prefix from corpus-engine's
/// install hint so we can print the bare command.
fn strip_install_prefix(hint: &str) -> &str {
    hint.strip_prefix("Install with: ")
        .or_else(|| hint.strip_prefix("Install via rustup: "))
        .unwrap_or(hint)
}

fn describe_rust(root: &Path) -> String {
    let Ok(content) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return "Rust".into();
    };
    let Ok(parsed) = content.parse::<toml::Value>() else {
        return "Rust".into();
    };
    if let Some(members) = parsed
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
    {
        return format!("Rust workspace ({} crates)", members.len());
    }
    "Rust".into()
}

fn has_file_with_ext(root: &Path, ext: &str, max_depth: usize) -> bool {
    fn walk(dir: &Path, ext: &str, depth: usize, max: usize) -> bool {
        if depth > max {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                // Skip conventional junk dirs so we don't walk
                // node_modules / target looking for .ts.
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if matches!(
                        name,
                        "node_modules"
                            | "target"
                            | "dist"
                            | "build"
                            | ".git"
                            | ".sovereign"
                            | ".venv"
                    ) {
                        continue;
                    }
                }
                if walk(&p, ext, depth + 1, max) {
                    return true;
                }
            } else if p.extension().and_then(|e| e.to_str()) == Some(ext) {
                return true;
            }
        }
        false
    }
    walk(root, ext, 0, max_depth)
}

// ─── Dependencies ────────────────────────────────────────────────────────────

/// Best-effort top-level dependency parse. We read the conventional
/// manifest file per language and pull direct deps (plus dev deps
/// where the file distinguishes). We intentionally don't walk lock
/// files — the point here is to tell `project found` "what external
/// services/libraries does this repo touch?", not to produce a
/// reproducible SBOM.
fn gather_deps(root: &Path) -> Vec<DetectedDependency> {
    let mut out = Vec::new();
    parse_cargo_deps(root, &mut out);
    parse_package_json_deps(root, &mut out);
    parse_go_mod_deps(root, &mut out);
    parse_pyproject_deps(root, &mut out);
    // Requirements.txt is the classic Python escape hatch — parse it
    // if present regardless of whether pyproject.toml also exists.
    parse_requirements_txt(root, &mut out);
    out
}

fn parse_cargo_deps(root: &Path, out: &mut Vec<DetectedDependency>) {
    let path = root.join("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(parsed) = text.parse::<toml::Value>() else {
        return;
    };
    // `[dependencies]` + `[dev-dependencies]`. For a workspace
    // manifest, deps usually live on member crates — still, workspace
    // `[workspace.dependencies]` is a useful signal.
    push_toml_deps(&parsed, "dependencies", "Cargo.toml", DepKind::Direct, out);
    push_toml_deps(&parsed, "dev-dependencies", "Cargo.toml", DepKind::Dev, out);
    if let Some(ws) = parsed.get("workspace") {
        push_toml_deps(ws, "dependencies", "Cargo.toml", DepKind::Direct, out);
    }
}

fn push_toml_deps(
    node: &toml::Value,
    key: &str,
    source: &str,
    kind: DepKind,
    out: &mut Vec<DetectedDependency>,
) {
    let Some(table) = node.get(key).and_then(|v| v.as_table()) else {
        return;
    };
    for (name, value) in table {
        let version = match value {
            toml::Value::String(s) => Some(s.clone()),
            toml::Value::Table(t) => t
                .get("version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            _ => None,
        };
        out.push(DetectedDependency {
            name: name.clone(),
            version,
            source_file: source.into(),
            kind,
        });
    }
}

fn parse_package_json_deps(root: &Path, out: &mut Vec<DetectedDependency>) {
    let path = root.join("package.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    for (key, kind) in [
        ("dependencies", DepKind::Direct),
        ("devDependencies", DepKind::Dev),
        ("peerDependencies", DepKind::Direct),
    ] {
        let Some(obj) = parsed.get(key).and_then(|v| v.as_object()) else {
            continue;
        };
        for (name, version) in obj {
            out.push(DetectedDependency {
                name: name.clone(),
                version: version.as_str().map(|s| s.to_string()),
                source_file: "package.json".into(),
                kind,
            });
        }
    }
}

fn parse_go_mod_deps(root: &Path, out: &mut Vec<DetectedDependency>) {
    let path = root.join("go.mod");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    // go.mod grammar we care about:
    //   require <path> <version>
    //   require (
    //     <path> <version>
    //     ...
    //   )
    // The `// indirect` suffix means it's pulled transitively.
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("require (") {
            in_block = true;
            continue;
        }
        if in_block {
            if line == ")" {
                in_block = false;
                continue;
            }
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            push_go_require(line, out);
            continue;
        }
        if let Some(rest) = line.strip_prefix("require ") {
            push_go_require(rest, out);
        }
    }
}

fn push_go_require(line: &str, out: &mut Vec<DetectedDependency>) {
    // Strip trailing "// indirect" (and anything after) — we still
    // record direct requires here; indirect deps are filtered out.
    let indirect = line.contains("// indirect");
    if indirect {
        return;
    }
    let line = line.split("//").next().unwrap_or(line).trim();
    let mut parts = line.split_whitespace();
    let (Some(name), Some(version)) = (parts.next(), parts.next()) else {
        return;
    };
    out.push(DetectedDependency {
        name: name.to_string(),
        version: Some(version.to_string()),
        source_file: "go.mod".into(),
        kind: DepKind::Direct,
    });
}

fn parse_pyproject_deps(root: &Path, out: &mut Vec<DetectedDependency>) {
    let path = root.join("pyproject.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(parsed) = text.parse::<toml::Value>() else {
        return;
    };
    // PEP 621: [project].dependencies = ["requests >= 2", ...]
    if let Some(deps) = parsed
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for dep in deps {
            if let Some(spec) = dep.as_str() {
                out.push(parse_pep508_spec(spec, "pyproject.toml", DepKind::Direct));
            }
        }
    }
    // Poetry: [tool.poetry.dependencies] = { foo = "^1" }
    if let Some(table) = parsed
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        for (name, v) in table {
            if name == "python" {
                continue;
            }
            let version = match v {
                toml::Value::String(s) => Some(s.clone()),
                toml::Value::Table(t) => t
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                _ => None,
            };
            out.push(DetectedDependency {
                name: name.clone(),
                version,
                source_file: "pyproject.toml".into(),
                kind: DepKind::Direct,
            });
        }
    }
}

fn parse_pep508_spec(spec: &str, source: &str, kind: DepKind) -> DetectedDependency {
    // Very loose PEP 508: split on first non-identifier char.
    let name_end = spec
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.')
        .unwrap_or(spec.len());
    let name = spec[..name_end].trim().to_string();
    let version = spec[name_end..].trim().to_string();
    let version = if version.is_empty() {
        None
    } else {
        Some(version)
    };
    DetectedDependency {
        name,
        version,
        source_file: source.into(),
        kind,
    }
}

fn parse_requirements_txt(root: &Path, out: &mut Vec<DetectedDependency>) {
    let path = root.join("requirements.txt");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with("-") {
            continue;
        }
        out.push(parse_pep508_spec(line, "requirements.txt", DepKind::Direct));
    }
}

// ─── Embed model ─────────────────────────────────────────────────────────────

/// Is the embed model runtime on this machine? Sovereign's
/// documentation pipeline expects it; init reports its absence as an
/// actionable gap rather than silently proceeding with broken search.
///
/// The canonical location is under `~/.svrnmesh/models/`. We check
/// for the directory's existence — a more thorough check would
/// verify a specific file, but init is meant to be a one-second
/// pass and `sovereign_cli_shared::dirs::sovereign_root()` plus a
/// `models` subdir is the honest indicator that setup was ever run.
fn embed_model_available() -> bool {
    sovereign_cli_shared::dirs::sovereign_root()
        .join("models")
        .is_dir()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn rust_project_detected_and_marked_not_required() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n\n[dependencies]\nreqwest = \"0.11\"\nserde = { version = \"1\", features = [\"derive\"] }\n",
        );
        let obs = observe(tmp.path());
        assert_eq!(obs.languages.len(), 1);
        assert_eq!(obs.languages[0].id, "rust");
        assert_eq!(obs.languages[0].scip_tooling, ScipTooling::NotRequired);
        let names: Vec<&str> = obs.deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"reqwest"));
        assert!(names.contains(&"serde"));
        let reqwest = obs.deps.iter().find(|d| d.name == "reqwest").unwrap();
        assert_eq!(reqwest.version.as_deref(), Some("0.11"));
        assert_eq!(reqwest.source_file, "Cargo.toml");
        assert_eq!(reqwest.kind, DepKind::Direct);
    }

    #[test]
    fn rust_workspace_description_counts_crates() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\n",
        );
        let obs = observe(tmp.path());
        assert_eq!(obs.languages[0].display, "Rust workspace (3 crates)");
    }

    #[test]
    fn go_project_detected_with_dep_parse() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "go.mod",
            "module example.com/foo\n\ngo 1.22\n\nrequire (\n\tgithub.com/polygon-io/client-go v1.14.0\n\tgithub.com/gorilla/websocket v1.5.0 // indirect\n)\n\nrequire github.com/stretchr/testify v1.9.0\n",
        );
        let obs = observe(tmp.path());
        assert_eq!(obs.languages.len(), 1);
        assert_eq!(obs.languages[0].id, "go");
        let names: Vec<&str> = obs.deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"github.com/polygon-io/client-go"));
        assert!(names.contains(&"github.com/stretchr/testify"));
        assert!(
            !names.contains(&"github.com/gorilla/websocket"),
            "indirect deps must be filtered out"
        );
    }

    #[test]
    fn go_scip_tooling_reports_missing_with_install_command() {
        // `scip-go` will not be on PATH in CI / dev boxes unless the
        // developer has installed it. This test confirms the
        // structure of the Missing variant regardless.
        let tooling = scip_tooling_for("go");
        match tooling {
            ScipTooling::Available { binary } => assert_eq!(binary, "scip-go"),
            ScipTooling::Missing {
                binary,
                install_cmd,
            } => {
                assert_eq!(binary, "scip-go");
                assert!(
                    install_cmd.contains("go install") && install_cmd.contains("scip-go"),
                    "install_cmd should be the bare go-install one-liner, got `{install_cmd}`"
                );
            }
            ScipTooling::NotRequired => panic!("go has a required indexer"),
        }
    }

    #[test]
    fn package_json_detects_typescript_when_ts_files_present() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "package.json",
            r#"{"name":"x","dependencies":{"express":"^4","axios":"^1"},"devDependencies":{"typescript":"^5"}}"#,
        );
        // Seed a .ts file somewhere shallow.
        write(tmp.path(), "index.ts", "export const x = 1;\n");
        let obs = observe(tmp.path());
        assert_eq!(obs.languages.len(), 1);
        assert_eq!(obs.languages[0].id, "typescript");
        let names: Vec<&str> = obs.deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"express"));
        assert!(names.contains(&"axios"));
        assert!(names.contains(&"typescript"));
        let express = obs.deps.iter().find(|d| d.name == "express").unwrap();
        assert_eq!(express.kind, DepKind::Direct);
        let ts_dep = obs.deps.iter().find(|d| d.name == "typescript").unwrap();
        assert_eq!(ts_dep.kind, DepKind::Dev);
    }

    #[test]
    fn package_json_without_ts_files_is_javascript() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "package.json",
            r#"{"name":"x","dependencies":{"lodash":"^4"}}"#,
        );
        let obs = observe(tmp.path());
        assert_eq!(obs.languages.len(), 1);
        assert_eq!(obs.languages[0].id, "javascript");
    }

    #[test]
    fn python_pyproject_pep621_deps_parsed() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "pyproject.toml",
            "[project]\nname = \"foo\"\nversion = \"0.1\"\ndependencies = [\n  \"requests >= 2.31\",\n  \"polygon-api-client ~= 1.12\",\n]\n",
        );
        let obs = observe(tmp.path());
        assert_eq!(obs.languages[0].id, "python");
        let names: Vec<&str> = obs.deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"requests"));
        assert!(names.contains(&"polygon-api-client"));
    }

    #[test]
    fn requirements_txt_skips_comments_and_options() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "requirements.txt",
            "# top comment\n\npandas==2.2.0\n-e .\nnumpy>=1.26  # inline\n--index-url https://mirror\n",
        );
        let obs = observe(tmp.path());
        let names: Vec<&str> = obs.deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"pandas"));
        assert!(names.contains(&"numpy"));
        assert!(
            !names.iter().any(|n| n.starts_with('-')),
            "options must be skipped"
        );
    }

    #[test]
    fn git_presence_tracked() {
        let tmp = tempdir().unwrap();
        let obs = observe(tmp.path());
        assert!(!obs.has_git);

        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        let obs2 = observe(tmp.path());
        assert!(obs2.has_git);
    }

    #[test]
    fn no_source_files_produces_empty_observation_without_error() {
        let tmp = tempdir().unwrap();
        let obs = observe(tmp.path());
        assert!(obs.languages.is_empty());
        assert!(obs.deps.is_empty());
        assert!(!obs.has_git);
    }

    #[test]
    fn strip_install_prefix_cleans_hints() {
        assert_eq!(
            strip_install_prefix("Install with: npm install -g x"),
            "npm install -g x"
        );
        assert_eq!(
            strip_install_prefix("Install via rustup: rustup component add ra"),
            "rustup component add ra"
        );
        // Unrecognized prefix passes through — we'd rather print
        // something informative than swallow the hint.
        assert_eq!(strip_install_prefix("See the docs"), "See the docs");
    }
}
