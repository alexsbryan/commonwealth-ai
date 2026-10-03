// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage 2 CLASSIFY — deterministic, never a model (ARCH §7.6).
//!
//! Group diagnostics by `(code, expected, found, syntactic context)`, match
//! each class against the spec's `[rules]` table, and split RULED from
//! RESIDUE. Residue is the only agentic stage of the factory and it is not
//! this command: here it is counted and listed, nothing more.

use super::discover::{CrateVerdict, RawDiagnostic};
use super::spec::RefactorSpec;
use regex::Regex;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Syntactic context of a diagnostic, derived from the diagnostic's own text.
/// A closed set with an honest `Other` (ARCH §2): the tag is a grouping key,
/// not a semantic judgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContextTag {
    FnArg,
    StructField,
    Return,
    MatchArm,
    MethodCall,
    TraitBound,
    Unresolved,
    Other,
}

impl ContextTag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FnArg => "fn-arg",
            Self::StructField => "struct-field",
            Self::Return => "return",
            Self::MatchArm => "match-arm",
            Self::MethodCall => "method-call",
            Self::TraitBound => "trait-bound",
            Self::Unresolved => "unresolved",
            Self::Other => "expr",
        }
    }

    fn derive(code: &str, message: &str, context_text: &str) -> Self {
        if code == "E0433" || code == "E0412" {
            return Self::Unresolved;
        }
        if code == "E0599" {
            return Self::MethodCall;
        }
        if code == "E0277" {
            return Self::TraitBound;
        }
        let blob = format!("{message}\n{context_text}");
        if blob.contains("arguments to this") || blob.contains("argument of type") {
            Self::FnArg
        } else if blob.contains("expected due to the type of this field")
            || blob.contains("structure field")
        {
            Self::StructField
        } else if blob.contains("expected because of return type") || blob.contains("return type") {
            Self::Return
        } else if blob.contains("`match` arms have incompatible types")
            || blob.contains("this `match` expression")
        {
            Self::MatchArm
        } else {
            Self::Other
        }
    }
}

/// Pull `expected`/`found` type strings out of a diagnostic. E0308 carries no
/// structured pair (and no `suggested_replacement` — measured, which is why
/// `cargo fix` cannot do this work), but rustc's own rendering is stable:
/// ``expected `T`?, found `U`?`` in the message, a label, or a child note.
pub fn extract_expected_found(
    message: &str,
    context_text: &str,
) -> (Option<String>, Option<String>) {
    static EXPECTED: OnceLock<Regex> = OnceLock::new();
    static FOUND: OnceLock<Regex> = OnceLock::new();
    let expected_re =
        EXPECTED.get_or_init(|| Regex::new(r"expected (?:[a-z]+ )*`([^`]+)`").expect("static"));
    let found_re =
        FOUND.get_or_init(|| Regex::new(r"found (?:[a-z]+ )*`([^`]+)`").expect("static"));
    let blob = format!("{message}\n{context_text}");
    let expected = expected_re.captures(&blob).map(|c| normalize_type(&c[1]));
    let found = found_re.captures(&blob).map(|c| normalize_type(&c[1]));
    (expected, found)
}

/// `&kernel_types::CorpusId` -> `&CorpusId`, `alloc::string::String` ->
/// `String`, `&'a str` -> `&str`. Path- and lifetime-blind so one class key
/// covers every crate's spelling of the same pair.
pub fn normalize_type(t: &str) -> String {
    static PATH: OnceLock<Regex> = OnceLock::new();
    static LIFETIME: OnceLock<Regex> = OnceLock::new();
    let path_re = PATH.get_or_init(|| Regex::new(r"\b[A-Za-z_][A-Za-z0-9_]*::").expect("static"));
    let lifetime_re =
        LIFETIME.get_or_init(|| Regex::new(r"'[A-Za-z_][A-Za-z0-9_]*\s+").expect("static"));
    let no_paths = path_re.replace_all(t, "");
    lifetime_re.replace_all(&no_paths, "").trim().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct ClassKey {
    pub code: String,
    pub expected: String,
    pub found: String,
    pub context: ContextTag,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SiteRef {
    pub file: String,
    pub line: u64,
    pub package: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ErrorClass {
    pub key: ClassKey,
    pub count: usize,
    /// Edit from the spec's `[rules]` table; `None` means RESIDUE.
    pub rule: Option<String>,
    pub sample_message: String,
    pub sites: Vec<SiteRef>,
}

pub struct Classification {
    pub classes: Vec<ErrorClass>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Totals {
    pub errors: usize,
    pub classes: usize,
    pub ruled_classes: usize,
    pub ruled_errors: usize,
    pub residue_classes: usize,
    pub residue_errors: usize,
}

impl Classification {
    pub fn totals(&self) -> Totals {
        let (mut ruled_c, mut ruled_e, mut res_c, mut res_e) = (0, 0, 0, 0);
        for c in &self.classes {
            if c.rule.is_some() {
                ruled_c += 1;
                ruled_e += c.count;
            } else {
                res_c += 1;
                res_e += c.count;
            }
        }
        Totals {
            errors: ruled_e + res_e,
            classes: self.classes.len(),
            ruled_classes: ruled_c,
            ruled_errors: ruled_e,
            residue_classes: res_c,
            residue_errors: res_e,
        }
    }

    pub fn render(
        &self,
        sites_per_class: usize,
        crate_verdicts: &BTreeMap<String, CrateVerdict>,
        outside_workspace: &[PathBuf],
    ) -> String {
        let mut out = String::new();
        let t = self.totals();
        let _ = writeln!(
            out,
            "classify: {} error(s) in {} class(es) — {} ruled ({} errors), {} residue ({} errors)",
            t.errors,
            t.classes,
            t.ruled_classes,
            t.ruled_errors,
            t.residue_classes,
            t.residue_errors
        );
        let _ = writeln!(out);

        for (header, want_rule) in [
            ("RULED — deterministic edit known", true),
            (
                "RESIDUE — no rule; the agentic stage (rf-4), counted here",
                false,
            ),
        ] {
            let group: Vec<&ErrorClass> = self
                .classes
                .iter()
                .filter(|c| c.rule.is_some() == want_rule)
                .collect();
            if group.is_empty() {
                continue;
            }
            let _ = writeln!(out, "{header}");
            for c in &group {
                let rule = c.rule.as_deref().unwrap_or("RESIDUE");
                let _ = writeln!(
                    out,
                    "  {:>5}  {} `{}` <- `{}` [{}]  =>  {}",
                    c.count,
                    c.key.code,
                    c.key.expected,
                    c.key.found,
                    c.key.context.as_str(),
                    rule
                );
                for s in c.sites.iter().take(sites_per_class) {
                    let _ = writeln!(out, "         {}:{} ({})", s.file, s.line, s.package);
                }
                if c.sites.len() > sites_per_class {
                    let _ = writeln!(
                        out,
                        "         … and {} more site(s)",
                        c.sites.len() - sites_per_class
                    );
                }
            }
            let _ = writeln!(out);
        }

        let _ = writeln!(
            out,
            "per-crate discover verdicts (four, not two — ARCH §18.1):"
        );
        let mut by_verdict: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (pkg, v) in crate_verdicts {
            let label = match v {
                CrateVerdict::Errors => "errors (enumeration complete this pass)",
                CrateVerdict::Clean => "clean",
                CrateVerdict::NeverRan => "NEVER RAN (blocked by a failed dependency — re-plan after those classes are applied)",
            };
            by_verdict.entry(label).or_default().push(pkg);
        }
        for (label, pkgs) in by_verdict {
            let _ = writeln!(out, "  {label}: {}", pkgs.join(", "));
        }
        if !outside_workspace.is_empty() {
            let _ = writeln!(
                out,
                "  outside the cargo workspace (seeded, but no check can see them): {}",
                outside_workspace
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        out
    }
}

pub fn classify(diags: &[RawDiagnostic], spec: &RefactorSpec) -> Classification {
    let mut groups: BTreeMap<ClassKey, ErrorClass> = BTreeMap::new();
    for d in diags {
        let context = ContextTag::derive(&d.code, &d.message, &d.context_text);
        let (expected, found) = if context == ContextTag::Unresolved {
            // `kernel_types` unresolved: the crate needs the dependency.
            (spec.target.clone(), "missing dependency".to_string())
        } else {
            (
                d.expected.clone().unwrap_or_else(|| "?".to_string()),
                d.found.clone().unwrap_or_else(|| "?".to_string()),
            )
        };
        let key = ClassKey {
            code: d.code.clone(),
            expected,
            found,
            context,
        };
        let entry = groups.entry(key.clone()).or_insert_with(|| {
            let rule = if context == ContextTag::Unresolved {
                Some(format!(
                    "add `{} = {{ workspace = true }}` to the crate's Cargo.toml",
                    spec.target_package()
                ))
            } else {
                spec.rule_for(&key.expected, &key.found).map(String::from)
            };
            ErrorClass {
                key,
                count: 0,
                rule,
                sample_message: d.message.clone(),
                sites: Vec::new(),
            }
        });
        entry.count += 1;
        entry.sites.push(SiteRef {
            file: d.file.clone(),
            line: d.line,
            package: d.package.clone(),
        });
    }
    let mut classes: Vec<ErrorClass> = groups.into_values().collect();
    classes.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
    Classification { classes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refactor_cmd::spec::RefactorSpec;

    fn spec() -> RefactorSpec {
        toml::from_str(
            r#"
id = "corpus-id"
kind = "newtype"
target = "kernel_types::CorpusId"
[discover]
seed = { field = "corpus_id", from = "String" }
[rules]
"&str <- &CorpusId" = "append .as_str()"
"CorpusId <- String" = "wrap CorpusId::new(_)?"
"#,
        )
        .expect("test spec")
    }

    fn diag(code: &str, msg: &str, ctx: &str, file: &str, line: u64) -> RawDiagnostic {
        let (expected, found) = extract_expected_found(msg, ctx);
        RawDiagnostic {
            code: code.into(),
            message: msg.into(),
            file: file.into(),
            line,
            column: 1,
            byte_start: 0,
            byte_end: 0,
            expected,
            found,
            context_text: ctx.into(),
            package: "pkg".into(),
        }
    }

    #[test]
    fn expected_found_come_out_of_rustc_prose_normalized() {
        let (e, f) = extract_expected_found(
            "mismatched types",
            "expected reference `&str`\nfound reference `&kernel_types::CorpusId`\narguments to this function are incorrect",
        );
        assert_eq!(e.as_deref(), Some("&str"));
        assert_eq!(f.as_deref(), Some("&CorpusId"));

        let (e, f) = extract_expected_found("expected `String`, found `CorpusId`", "");
        assert_eq!(e.as_deref(), Some("String"));
        assert_eq!(f.as_deref(), Some("CorpusId"));
    }

    #[test]
    fn normalization_is_path_and_lifetime_blind() {
        assert_eq!(normalize_type("alloc::string::String"), "String");
        assert_eq!(normalize_type("&'a str"), "&str");
        assert_eq!(normalize_type("Vec<std::string::String>"), "Vec<String>");
        assert_eq!(normalize_type("&kernel_types::CorpusId"), "&CorpusId");
    }

    #[test]
    fn same_pair_same_context_is_one_class_and_rules_attach() {
        let s = spec();
        let diags = vec![
            diag(
                "E0308",
                "mismatched types",
                "expected `&str`, found `&kernel_types::CorpusId`\narguments to this function are incorrect",
                "a.rs",
                1,
            ),
            diag(
                "E0308",
                "mismatched types",
                "expected `&str`, found `&corpus_engine::CorpusId`\narguments to this function are incorrect",
                "b.rs",
                9,
            ),
            diag(
                "E0308",
                "mismatched types",
                "expected `kernel_types::CorpusId`, found `String`\nexpected due to the type of this field",
                "c.rs",
                3,
            ),
        ];
        let c = classify(&diags, &s);
        assert_eq!(c.classes.len(), 2);
        let top = &c.classes[0];
        assert_eq!(top.count, 2);
        assert_eq!(top.key.context, ContextTag::FnArg);
        assert_eq!(top.rule.as_deref(), Some("append .as_str()"));
        let second = &c.classes[1];
        assert_eq!(second.key.context, ContextTag::StructField);
        assert_eq!(second.rule.as_deref(), Some("wrap CorpusId::new(_)?"));
        let t = c.totals();
        assert_eq!((t.errors, t.residue_classes), (3, 0));
    }

    #[test]
    fn unresolved_target_crate_classifies_as_missing_dependency() {
        let s = spec();
        let diags = vec![diag(
            "E0433",
            "failed to resolve: use of undeclared crate or module `kernel_types`",
            "use of undeclared crate or module `kernel_types`",
            "x.rs",
            2,
        )];
        let c = classify(&diags, &s);
        assert_eq!(c.classes.len(), 1);
        assert_eq!(c.classes[0].key.context, ContextTag::Unresolved);
        assert!(c.classes[0]
            .rule
            .as_deref()
            .is_some_and(|r| r.contains("kernel-types")));
    }

    #[test]
    fn a_pair_without_a_rule_is_residue_not_defaulted() {
        let s = spec();
        let diags = vec![diag(
            "E0599",
            "no method named `push_str` found for struct `kernel_types::CorpusId`",
            "",
            "y.rs",
            7,
        )];
        let c = classify(&diags, &s);
        assert_eq!(c.classes[0].rule, None);
        assert_eq!(c.totals().residue_errors, 1);
    }
}
