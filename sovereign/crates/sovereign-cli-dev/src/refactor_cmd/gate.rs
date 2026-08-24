// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-item ENTRY GATE — mandatory, from `quality/REFACTOR_FACTORY.md`:
//! representation match · wire form match with a passing fixture · trait
//! surface adequate · fallibility volume counted.
//!
//! An item failing the wire check is NOT SCHEDULED — it is filed as a finding.
//! The check reads the target's SOURCE (the serde surface on the definition)
//! and RUNS the declared fixture test; a spec declaration the source
//! contradicts fails the gate. The compiler is exhaustive over types and blind
//! to encoding — this gate is the encoding half, statically; the wire differ
//! (rf-2) is its dynamic proof at apply time.

use super::census;
use super::discover::WorkspaceMeta;
use super::spec::RefactorSpec;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

// ── Locating and reading a type definition ──────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case", tag = "shape")]
pub enum TypeDecl {
    /// `pub struct Name(Inner);`
    TupleStruct {
        inner: String,
    },
    /// `pub struct Name { .. }`
    NamedStruct,
    /// `define_id!(Name, ..)` — `[u8; 16]` with DERIVED serde.
    DefineId,
    Enum,
    NotFound,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TypeDefSite {
    pub file: PathBuf,
    pub line: usize,
    pub decl: TypeDecl,
}

/// Find `name`'s definition inside `package_dir` by reading source — no SCIP
/// dependency, so the gate answers even when the graph lags the tree.
pub fn locate_type_def(package_dir: &Path, name: &str) -> TypeDefSite {
    let tuple_re = regex::Regex::new(&format!(
        r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?struct\s+{}\s*\(\s*(.+?)\s*\)\s*;",
        regex::escape(name)
    ))
    .expect("escaped name");
    let named_re = regex::Regex::new(&format!(
        r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?struct\s+{}\s*(?:<[^>]*>)?\s*\{{",
        regex::escape(name)
    ))
    .expect("escaped name");
    let enum_re = regex::Regex::new(&format!(
        r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?enum\s+{}\b",
        regex::escape(name)
    ))
    .expect("escaped name");
    let define_re = regex::Regex::new(&format!(r"^\s*define_id!\s*\(\s*{}\b", regex::escape(name)))
        .expect("escaped name");

    for file in census::walk_rs_files(package_dir, census::EXCLUDE_DIRS_DECL) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let decl = if let Some(c) = tuple_re.captures(line) {
                Some(TypeDecl::TupleStruct {
                    inner: c[1].to_string(),
                })
            } else if define_re.is_match(line) {
                Some(TypeDecl::DefineId)
            } else if named_re.is_match(line) {
                Some(TypeDecl::NamedStruct)
            } else if enum_re.is_match(line) {
                Some(TypeDecl::Enum)
            } else {
                None
            };
            if let Some(decl) = decl {
                return TypeDefSite {
                    file,
                    line: i + 1,
                    decl,
                };
            }
        }
    }
    TypeDefSite {
        file: package_dir.to_path_buf(),
        line: 0,
        decl: TypeDecl::NotFound,
    }
}

/// The definition's serde surface, read off the source.
pub fn observed_wire(site: &TypeDefSite, name: &str) -> String {
    match &site.decl {
        TypeDecl::DefineId => {
            "byte-array (define_id!: derived serde over [u8; 16] — a 16-integer JSON array)"
                .to_string()
        }
        TypeDecl::NotFound => "unknown (definition not found)".to_string(),
        _ => {
            let Ok(text) = std::fs::read_to_string(&site.file) else {
                return "unknown (unreadable source)".to_string();
            };
            let lines: Vec<&str> = text.lines().collect();
            // Attribute block directly above the definition line.
            let mut i = site.line.saturating_sub(1);
            let mut transparent = false;
            while i > 0 {
                let l = lines[i - 1].trim_start();
                if l.starts_with("#[") || l.starts_with("///") || l.starts_with("//") {
                    if l.contains("serde(transparent)") {
                        transparent = true;
                    }
                    i -= 1;
                } else {
                    break;
                }
            }
            if transparent {
                return "transparent".to_string();
            }
            if text.contains(&format!("impl Serialize for {name}"))
                || text.contains(&format!("impl serde::Serialize for {name}"))
            {
                return "custom (hand-written Serialize impl — read it)".to_string();
            }
            "derived-default".to_string()
        }
    }
}

// ── The wire fixture — run, never asserted ──────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case", tag = "verdict")]
pub enum FixtureVerdict {
    Passed {
        tests: usize,
    },
    Failed {
        passed: usize,
        failed: usize,
    },
    /// The filter matched nothing: a zero-test run is never green.
    NeverRan,
    CouldNotJudge {
        reason: String,
    },
    /// `--skip-fixture`: a named substitution, not a silent one (ARCH §18.3).
    SkippedOnRequest,
    NotDeclared,
}

pub fn run_fixture_test(root: &Path, package: &str, test: &str) -> FixtureVerdict {
    let out = match std::process::Command::new("cargo")
        .args(["test", "-p", package, test])
        .current_dir(root)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return FixtureVerdict::CouldNotJudge {
                reason: format!("cargo test did not run: {e}"),
            }
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let re = regex::Regex::new(r"test result: \w+\. (\d+) passed; (\d+) failed").expect("static");
    let (mut passed, mut failed) = (0usize, 0usize);
    let mut summaries = 0;
    for c in re.captures_iter(&stdout) {
        summaries += 1;
        passed += c[1].parse::<usize>().unwrap_or(0);
        failed += c[2].parse::<usize>().unwrap_or(0);
    }
    if summaries == 0 {
        return FixtureVerdict::CouldNotJudge {
            reason: format!(
                "no test summary in cargo output (exit {})",
                out.status.code().unwrap_or(-1)
            ),
        };
    }
    if failed > 0 {
        FixtureVerdict::Failed { passed, failed }
    } else if passed == 0 {
        FixtureVerdict::NeverRan
    } else {
        FixtureVerdict::Passed { tests: passed }
    }
}

// ── The four checks for one spec ────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct SpecGateReport {
    pub spec_id: String,
    pub target: String,
    // 1. representation
    pub seed_from: String,
    pub representation: TypeDecl,
    pub def_file: String,
    pub def_line: usize,
    pub representation_matches: bool,
    // 2. wire
    pub wire_declared: Option<String>,
    pub wire_observed: String,
    pub wire_consistent: bool,
    pub fixture: FixtureVerdict,
    // 3. trait surface
    pub impls_present: Vec<String>,
    pub impls_planned_missing: Vec<String>,
    // 4. fallibility
    pub constructor: String,
    pub decl_sites: usize,
    pub decl_files: usize,
    /// Wire surfaces the spec declares — proven per-surface by `wire-check`
    /// (rf-2) at apply time; named here so the declaration is visible.
    pub wire_surfaces: Vec<String>,
}

impl SpecGateReport {
    pub fn passed(&self) -> bool {
        let fixture_ok = matches!(
            self.fixture,
            FixtureVerdict::Passed { .. } | FixtureVerdict::SkippedOnRequest
        );
        self.representation_matches && self.wire_consistent && fixture_ok
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let verdict = if self.passed() { "PASSED" } else { "REFUSED" };
        let _ = writeln!(
            out,
            "entry gate [{verdict}] — {} -> {}",
            self.spec_id, self.target
        );
        let _ = writeln!(
            out,
            "  representation  {}  ({:?} at {}:{}; seed retypes `{}`)",
            if self.representation_matches {
                "MATCH"
            } else {
                "MISMATCH"
            },
            self.representation,
            self.def_file,
            self.def_line,
            self.seed_from,
        );
        let _ = writeln!(
            out,
            "  wire form       {}  (declared {:?}, observed \"{}\"; surfaces [{}] proven \
             per-surface by wire-check at apply time)",
            if self.wire_consistent {
                "MATCH"
            } else {
                "MISMATCH"
            },
            self.wire_declared.as_deref().unwrap_or("<undeclared>"),
            self.wire_observed,
            self.wire_surfaces.join(", "),
        );
        let fixture = match &self.fixture {
            FixtureVerdict::Passed { tests } => format!("PASSED ({tests} test(s))"),
            FixtureVerdict::Failed { passed, failed } => {
                format!("FAILED ({passed} passed, {failed} failed)")
            }
            FixtureVerdict::NeverRan => {
                "NEVER RAN — the filter matched no test; a zero-test run is never green".to_string()
            }
            FixtureVerdict::CouldNotJudge { reason } => format!("COULD NOT JUDGE — {reason}"),
            FixtureVerdict::SkippedOnRequest => {
                "SKIPPED on request — wire unproven this run".to_string()
            }
            FixtureVerdict::NotDeclared => {
                "NOT DECLARED — a wire form without a fixture is asserted, not proven".to_string()
            }
        };
        let _ = writeln!(out, "  wire fixture    {fixture}");
        let _ = writeln!(
            out,
            "  trait surface   {} present [{}]; prepare (rf-3) adds [{}]",
            if self.impls_planned_missing.is_empty() {
                "adequate now:"
            } else {
                "adequate after prepare:"
            },
            self.impls_present.join(", "),
            self.impls_planned_missing.join(", "),
        );
        let _ = writeln!(
            out,
            "  fallibility     constructor {}; {} declaration site(s) across {} file(s) bound \
             the volume (exact count lands with classify)",
            self.constructor, self.decl_sites, self.decl_files,
        );
        out
    }
}

pub fn gate_spec(
    root: &Path,
    meta: &WorkspaceMeta,
    spec: &RefactorSpec,
    run_fixture: bool,
) -> SpecGateReport {
    let package = spec.target_package();
    let package_dir = meta
        .get(&package)
        .map(|p| p.dir.clone())
        .unwrap_or_else(|| root.join(&package));
    let name = spec.target_name();
    let site = locate_type_def(&package_dir, name);

    // 1. representation: the seed's `from` type must be the target's inner
    // representation, or every site is a conversion rather than a retype.
    let representation_matches = matches!(
        &site.decl,
        TypeDecl::TupleStruct { inner } if inner == &spec.discover.seed.from
    );

    // 2. wire.
    let wire_observed = observed_wire(&site, name);
    let wire_declared = spec.safety.as_ref().map(|s| s.wire.clone());
    let wire_consistent = match &wire_declared {
        Some(d) => wire_observed.starts_with(d.as_str()),
        None => false, // undeclared wire form: unproven, refused
    };
    let fixture = match (
        run_fixture,
        spec.safety.as_ref().and_then(|s| s.fixture.as_ref()),
    ) {
        (_, None) => FixtureVerdict::NotDeclared,
        (false, Some(_)) => FixtureVerdict::SkippedOnRequest,
        (true, Some(f)) => run_fixture_test(root, &f.package, &f.test),
    };

    // 3. trait surface: which of the planned impls already exist.
    let (impls_present, impls_planned_missing) =
        split_present_impls(&package_dir, name, &spec.prepare.impls);

    // 4. fallibility: read the constructor, bound the volume by declarations.
    let constructor = constructor_shape(&site, name);
    let files = census::walk_rs_files(root, census::EXCLUDE_DIRS_DECL);
    let decls =
        census::find_decl_sites(&files, &spec.discover.seed.field, &spec.discover.seed.from);
    let decl_files = decls.len();
    let decl_sites: usize = decls.iter().map(|d| d.lines.len()).sum();
    for d in &decls {
        tracing::debug!(
            target: "refactor",
            file = %d.path.display(),
            sites = d.lines.len(),
            "seed declaration sites"
        );
    }

    SpecGateReport {
        spec_id: spec.id.clone(),
        target: spec.target.clone(),
        seed_from: spec.discover.seed.from.clone(),
        representation: site.decl.clone(),
        def_file: site.file.display().to_string(),
        def_line: site.line,
        representation_matches,
        wire_declared,
        wire_observed,
        wire_consistent,
        fixture,
        impls_present,
        impls_planned_missing,
        constructor,
        decl_sites,
        decl_files,
        wire_surfaces: spec
            .safety
            .as_ref()
            .map(|s| s.surfaces.clone())
            .unwrap_or_default(),
    }
}

/// Space-insensitive `impl` search across a package: `AsRef<str>` is present
/// when some line contains `impl AsRef<str> for Name`; an entry that already
/// names a `for` (`From<X> for Y`) is matched as spelled.
fn split_present_impls(
    package_dir: &Path,
    name: &str,
    planned: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut haystack = String::new();
    for file in census::walk_rs_files(package_dir, census::EXCLUDE_DIRS_DECL) {
        if let Ok(text) = std::fs::read_to_string(&file) {
            for line in text.lines() {
                let t = line.trim_start();
                if t.starts_with("impl") {
                    haystack.push_str(&t.replace(' ', ""));
                    haystack.push('\n');
                }
            }
        }
    }
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for p in planned {
        let needle = if p.contains(" for ") {
            format!("impl{}", p.replace(' ', ""))
        } else {
            format!("impl{}for{}", p.replace(' ', ""), name)
        };
        if haystack.lines().any(|l| l.contains(&needle)) {
            present.push(p.clone());
        } else {
            missing.push(p.clone());
        }
    }
    (present, missing)
}

fn constructor_shape(site: &TypeDefSite, name: &str) -> String {
    let Ok(text) = std::fs::read_to_string(&site.file) else {
        return "unknown".to_string();
    };
    // Look inside `impl Name {` for `fn new`.
    let mut in_impl = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("impl") && t.contains(&format!("impl {name}")) && !t.contains(" for ") {
            in_impl = true;
        }
        if in_impl && t.starts_with("pub fn new") {
            if t.contains("-> Option<") {
                return "fallible: Option (absence refused, never defaulted)".to_string();
            }
            if t.contains("-> Result<") {
                return "fallible: Result".to_string();
            }
            return "infallible".to_string();
        }
    }
    "no `new` constructor found".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn transparent_tuple_struct_reads_as_transparent_wire() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "ids.rs",
            "#[derive(Clone, Serialize, Deserialize)]\n#[serde(transparent)]\npub struct CorpusId(String);\n\nimpl CorpusId {\n    pub fn new(id: impl Into<String>) -> Option<Self> { None }\n}\n",
        );
        let site = locate_type_def(dir.path(), "CorpusId");
        assert_eq!(
            site.decl,
            TypeDecl::TupleStruct {
                inner: "String".into()
            }
        );
        assert_eq!(observed_wire(&site, "CorpusId"), "transparent");
        assert!(constructor_shape(&site, "CorpusId").starts_with("fallible: Option"));
    }

    #[test]
    fn define_id_reads_as_byte_array_wire_mismatch() {
        // The node_id near-miss, encoded: derived serde over [u8;16] is a
        // 16-integer JSON array. `cargo check` passes the migration and every
        // client breaks — which is exactly why the gate reads the wire form.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "ids.rs", "define_id!(NodeId, \"node\");\n");
        let site = locate_type_def(dir.path(), "NodeId");
        assert_eq!(site.decl, TypeDecl::DefineId);
        assert!(observed_wire(&site, "NodeId").starts_with("byte-array"));
    }

    #[test]
    fn impl_presence_is_space_insensitive_and_for_aware() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.rs",
            "impl AsRef<str> for CorpusId {}\nimpl From<CorpusId> for String {}\n",
        );
        let (present, missing) = split_present_impls(
            dir.path(),
            "CorpusId",
            &[
                "AsRef<str>".to_string(),
                "From<CorpusId> for String".to_string(),
                "FromStr".to_string(),
            ],
        );
        assert_eq!(present, vec!["AsRef<str>", "From<CorpusId> for String"]);
        assert_eq!(missing, vec!["FromStr"]);
    }
}
