// SPDX-License-Identifier: AGPL-3.0-or-later
//! Does a register destination actually exist?
//!
//! `quality/CONCEPTS.toml`'s `canonical` column IS the ledger's `dest`: every
//! label written by the register pass copies it verbatim, `status` groups open
//! holdings by it, and `next` renders it into the order a worker reads. So a
//! canonical naming a home the type does not have sends that worker to write
//! an import that cannot compile — and it does so in the register's own voice,
//! which is the well-formed-and-wrong failure ARCH §18 exists to refuse.
//!
//! That was not hypothetical. Measured 2026-08-24 on this tree, seven rows
//! named `sovereign_contracts::…` paths for types that live in `kernel-types`,
//! and the code said so out loud: `kernel-types/src/answer.rs` carries
//! *"`quality/CONCEPTS.toml` writes the canonical home as
//! `sovereign_contracts::answer::Answer`. That home **cannot hold this
//! type**"*. Three rows of that family (Answer, Draft, Citation) were repaired
//! when rung `nc-11-answer` landed; the seven siblings from `nc-1-kernel`,
//! `nc-10-judgement` and `nc-4-evidence` were not. The register looked
//! maintained, which is worse than looking stale.
//!
//! # Three verdicts, and only one of them may be handed to a worker
//!
//! [`Resolution::Defined`] / [`Resolution::ReExported`] — a worker can `use`
//! this path today. [`Resolution::Unbuilt`] — the name is defined nowhere
//! first-party, so this is a genuine future home and the row is honest work
//! not yet done. [`Resolution::Elsewhere`] — the name IS defined, somewhere
//! that is not here, and the canonical does not re-export it: something built
//! this noun and the register never learned where it went.
//!
//! The last two differ in diagnosis and agree on the action: neither may be
//! cut into an order.
//!
//! # Why the working tree and not the SCIP graph
//!
//! The graph is the better instrument for "where is this symbol", and
//! `converge::census` uses it. It is the wrong one here for two reasons. It
//! speaks for the LAST INDEXED COMMIT — which is exactly why
//! `xtask concept-gate` is advisory — so a canonical repaired in the working
//! tree stays invisible until the next index, and the check would refuse the
//! fix that repairs it. And re-export edges are the load-bearing case
//! (`corpus_engine::Evidence`, `oicp_types::Capability`), which a definition
//! index does not carry. The question asked here is "can a worker write this
//! `use` line against the tree in front of them", and the tree is the only
//! thing that answers it.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a register `canonical` resolves to against the working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Declared at the canonical path. A worker can `use` it.
    Defined { file: String, line: usize },
    /// Re-exported at the canonical path. A worker can `use` it.
    ReExported { file: String },
    /// The name is declared nowhere in first-party production code. A
    /// legitimate `target` home nobody has minted yet.
    Unbuilt,
    /// Declared — but not here, and the canonical path does not re-export it.
    Elsewhere { sites: Vec<String> },
    /// Not a Rust type path. Two register rows are like this on purpose —
    /// `Command`'s canonical is a TOML contract file and `WireMessage`'s is a
    /// whole crate — so the question "does it exist" is still answerable and
    /// is still asked. A contract file that was deleted is exactly as broken
    /// as a module that was never written.
    NotATypePath { what: &'static str, exists: bool },
}

impl Resolution {
    /// Is there something at the canonical path today?
    ///
    /// This is the one question `home` declares an answer to, so it is also
    /// the one that decides whether a destination may be written into a label
    /// or cut into an order.
    pub fn exists(&self) -> bool {
        match self {
            Self::Defined { .. } | Self::ReExported { .. } => true,
            Self::NotATypePath { exists, .. } => *exists,
            Self::Unbuilt | Self::Elsewhere { .. } => false,
        }
    }

    /// One line, for the health block and for a refusal message.
    pub fn render(&self) -> String {
        match self {
            Self::Defined { file, line } => format!("defined      {file}:{line}"),
            Self::ReExported { file } => format!("re-exported  {file}"),
            Self::Unbuilt => "UNBUILT      the name is declared nowhere first-party".to_string(),
            Self::Elsewhere { sites } => {
                let mut s = format!("ELSEWHERE    declared at {}", sites[0]);
                if sites.len() > 1 {
                    s.push_str(&format!(" (+{} more)", sites.len() - 1));
                }
                s
            }
            Self::NotATypePath { what, exists: true } => format!("present      {what}"),
            Self::NotATypePath {
                what,
                exists: false,
            } => format!("ABSENT       {what}"),
        }
    }
}

/// The workspace's crate map plus a lazily-built index of type declarations.
pub struct Workspace {
    root: PathBuf,
    /// `kernel_types` -> `<root>/kernel-types`. Keyed by the RUST crate name
    /// (underscores), because that is the spelling a canonical path uses.
    crates: BTreeMap<String, PathBuf>,
    /// Built on first miss only: type name -> `file:line` sites. A `status`
    /// whose rows all resolve never pays for the sweep.
    defs: OnceCell<BTreeMap<String, Vec<String>>>,
}

impl Workspace {
    /// Read `<root>/Cargo.toml`'s workspace members and note where each lives.
    pub fn scan(root: &Path) -> Result<Self, String> {
        let manifest = root.join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest)
            .map_err(|e| format!("{}: {e}", manifest.display()))?;
        let parsed: toml::Value =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
        let members = parsed
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(toml::Value::as_array)
            .ok_or_else(|| format!("{}: no [workspace] members", manifest.display()))?;

        let mut crates = BTreeMap::new();
        for m in members.iter().filter_map(toml::Value::as_str) {
            let dir = root.join(m);
            let Ok(t) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
                continue;
            };
            let Ok(v) = toml::from_str::<toml::Value>(&t) else {
                continue;
            };
            let Some(name) = v
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(toml::Value::as_str)
            else {
                continue;
            };
            crates.insert(name.replace('-', "_"), dir);
        }
        Ok(Self {
            root: root.to_path_buf(),
            crates,
            defs: OnceCell::new(),
        })
    }

    /// Resolve one register `canonical` against the tree.
    pub fn resolve(&self, canonical: &str) -> Resolution {
        if canonical.contains('/') || canonical.ends_with(".toml") {
            // A repo-relative contract file. `Command`'s totality says the CLI
            // contract is generative — the parser, dispatch and help text are
            // produced from it — so the file IS the home and its absence is a
            // real failure, not a category the check skips.
            return Resolution::NotATypePath {
                what: "a contract file, not a Rust path",
                exists: self.root.join(canonical).is_file(),
            };
        }
        let parts: Vec<&str> = canonical.split("::").collect();
        if parts.len() < 2 {
            // A whole crate as the home — `WireMessage` names `sovereign_wire`
            // because the noun is a FAMILY of wire messages, not one type.
            return Resolution::NotATypePath {
                what: "a crate name, not a type path",
                exists: self.crates.contains_key(canonical),
            };
        }
        let (crate_name, ty) = (parts[0], *parts.last().expect("len >= 2"));
        let mods = &parts[1..parts.len() - 1];

        let Some(dir) = self.crates.get(crate_name) else {
            return self.elsewhere_or_unbuilt(ty);
        };
        // A bin-only crate roots at main.rs; the path is still intra-crate truth.
        let mut cur = dir.join("src/lib.rs");
        if !cur.is_file() {
            cur = dir.join("src/main.rs");
            if !cur.is_file() {
                return self.elsewhere_or_unbuilt(ty);
            }
        }
        for seg in mods {
            match child_module(&cur, seg) {
                Some(next) => cur = next,
                None => return self.elsewhere_or_unbuilt(ty),
            }
        }
        match declares(&cur, ty) {
            Some(Declared::Definition(line)) => Resolution::Defined {
                file: self.rel(&cur),
                line,
            },
            Some(Declared::ReExport) => Resolution::ReExported {
                file: self.rel(&cur),
            },
            None => self.elsewhere_or_unbuilt(ty),
        }
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .unwrap_or(p)
            .display()
            .to_string()
    }

    /// The canonical path did not carry the type. Is it anywhere at all?
    ///
    /// The distinction is the whole point: nowhere means the home is honest
    /// future work; somewhere-else means the register lost track of a move.
    fn elsewhere_or_unbuilt(&self, ty: &str) -> Resolution {
        let defs = self.defs.get_or_init(|| index_declarations(&self.root));
        match defs.get(ty) {
            Some(sites) if !sites.is_empty() => Resolution::Elsewhere {
                sites: sites.clone(),
            },
            _ => Resolution::Unbuilt,
        }
    }
}

/// The register surveyed against the tree: one row, one verdict.
///
/// This is the health line `status` prints. It is a MEASUREMENT taken fresh on
/// every invocation, never a stored count — same interlock as the burn-down it
/// sits beside (`REFACTOR_LEDGER.md` §"Closure is an absence, not a record").
///
/// # The check is symmetric, and the asymmetric version would have missed it
///
/// An earlier draft asked only "does every canonical resolve?" and carried the
/// exceptions as a hardcoded list in a test. That list is a second decider for
/// a question the register already answers (ARCH §10.6), and it can only catch
/// drift in one direction. The drift that actually happened ran the other way:
/// seven homes were MINTED and the register went on calling them targets. So
/// the row declares `home` and this compares it to the tree, and a `planned`
/// row that has quietly acquired a home fails exactly as loudly as a `minted`
/// row that lost one.
pub struct RegisterHealth {
    pub rows: Vec<Row>,
}

/// One register row, and what the tree says about its canonical.
pub struct Row {
    pub name: String,
    pub canonical: String,
    /// What the register declares.
    pub declared: super::labels::Home,
    /// What the working tree shows.
    pub found: Resolution,
}

impl Row {
    /// Does the tree agree with the declaration?
    pub fn agrees(&self) -> bool {
        matches!(
            (self.declared, self.found.exists()),
            (super::labels::Home::Minted, true) | (super::labels::Home::Planned, false)
        )
    }

    /// The repair, named. A disagreement is never reported without one.
    pub fn remedy(&self) -> &'static str {
        match self.declared {
            super::labels::Home::Minted => {
                "declared minted but nothing is there — repair `canonical`, or set home = \"planned\""
            }
            super::labels::Home::Planned => {
                "declared planned but the home EXISTS — set home = \"minted\" (this is the drift that broke the register)"
            }
        }
    }
}

impl RegisterHealth {
    pub fn survey(root: &Path) -> Result<Self, String> {
        let ws = Workspace::scan(root)?;
        let register = super::labels::load_register(root)?;
        Ok(Self {
            rows: register
                .into_iter()
                .map(|r| {
                    let found = ws.resolve(&r.canonical);
                    Row {
                        name: r.name,
                        canonical: r.canonical,
                        declared: r.home,
                        found,
                    }
                })
                .collect(),
        })
    }

    /// Rows where the register and the tree disagree. A non-empty list is not
    /// a warning: every one of these is a `dest` that must not be cut into an
    /// order until it is settled.
    pub fn disagreements(&self) -> Vec<&Row> {
        self.rows.iter().filter(|r| !r.agrees()).collect()
    }

    fn minted(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| r.declared == super::labels::Home::Minted)
            .count()
    }

    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let bad = self.disagreements();
        let _ = writeln!(
            out,
            " register homes:            {} minted / {} planned, of {}{}",
            self.minted(),
            self.rows.len() - self.minted(),
            self.rows.len(),
            if bad.is_empty() {
                " — tree agrees"
            } else {
                ""
            }
        );
        if bad.is_empty() {
            return out;
        }
        let _ = writeln!(
            out,
            "   {} row(s) where the register and the working tree disagree:",
            bad.len()
        );
        for r in bad {
            let _ = writeln!(out, "   {:<16} {}", r.name, r.canonical);
            let _ = writeln!(out, "   {:<16} tree says: {}", "", r.found.render());
            let _ = writeln!(out, "   {:<16} {}", "", r.remedy());
        }
        out
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "minted": self.minted(),
            "planned": self.rows.len() - self.minted(),
            "total": self.rows.len(),
            "disagreements": self.disagreements().iter().map(|r| serde_json::json!({
                "name": r.name,
                "canonical": r.canonical,
                "declared": match r.declared {
                    super::labels::Home::Minted => "minted",
                    super::labels::Home::Planned => "planned",
                },
                "tree": r.found.render(),
                "remedy": r.remedy(),
            })).collect::<Vec<_>>(),
        })
    }
}

enum Declared {
    Definition(usize),
    ReExport,
}

/// Find the file defining `seg` as a child module of the module in `cur`.
fn child_module(cur: &Path, seg: &str) -> Option<PathBuf> {
    let parent = cur.parent()?;
    let stem = cur.file_name()?.to_str()?;
    // `foo.rs` hosts its children in `foo/`; `mod.rs` / `lib.rs` / `main.rs`
    // host theirs beside themselves.
    let dir = if matches!(stem, "mod.rs" | "lib.rs" | "main.rs") {
        parent.to_path_buf()
    } else {
        parent.join(stem.trim_end_matches(".rs"))
    };
    for cand in [dir.join(format!("{seg}.rs")), dir.join(seg).join("mod.rs")] {
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Does this file declare `ty` — as a definition, or as a `pub use`?
fn declares(file: &Path, ty: &str) -> Option<Declared> {
    let src = std::fs::read_to_string(file).ok()?;
    for (i, line) in src.lines().enumerate() {
        if let Some(name) = definition_name(line) {
            if name == ty {
                return Some(Declared::Definition(i + 1));
            }
        }
    }
    // `pub use` may wrap across lines (`pub use x::{\n  A,\n  B,\n};`), so the
    // re-export scan works on the statement, not the line.
    for stmt in src.split("pub use ").skip(1) {
        let body = match stmt.split_once(';') {
            Some((b, _)) => b,
            None => continue,
        };
        if body
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|w| w == ty)
        {
            return Some(Declared::ReExport);
        }
    }
    None
}

/// The type name this line declares, if it declares one.
///
/// Deliberately narrow: `pub` (with any visibility qualifier) followed by one
/// of the five item keywords. A `pub(crate)` item is a declaration for this
/// purpose — the question is where the noun LIVES, not who may import it.
fn definition_name(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let rest = t.strip_prefix("pub")?;
    let rest = match rest.strip_prefix('(') {
        Some(r) => r.split_once(')')?.1,
        None => rest,
    };
    let rest = rest.strip_prefix(char::is_whitespace)?.trim_start();
    for kw in ["struct ", "enum ", "trait ", "type ", "union "] {
        if let Some(after) = rest.strip_prefix(kw) {
            let name = after
                .trim_start()
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or_default();
            if !name.is_empty() && name.starts_with(char::is_uppercase) {
                return Some(name);
            }
        }
    }
    None
}

/// Every type declaration in first-party production Rust, name -> `file:line`.
///
/// Test-scope code is out by construction, for the same reason `concept-gate`
/// exempts it: a fixture named `Evidence` is not a second home for the noun.
fn index_declarations(root: &Path) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if p.is_dir() {
                if matches!(
                    name.as_ref(),
                    "target" | ".git" | "tests" | "benches" | "examples" | "node_modules"
                ) || name.starts_with('.')
                {
                    continue;
                }
                stack.push(p);
            } else if name.ends_with(".rs") {
                let Ok(src) = std::fs::read_to_string(&p) else {
                    continue;
                };
                let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
                for (i, line) in src.lines().enumerate() {
                    if let Some(ty) = definition_name(line) {
                        out.entry(ty.to_string())
                            .or_default()
                            .push(format!("{rel}:{}", i + 1));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> Workspace {
        let root = super::super::census::repo_root().expect("repo root");
        Workspace::scan(&root).expect("workspace scan")
    }

    #[test]
    fn a_definition_line_yields_its_name_and_a_use_line_does_not() {
        assert_eq!(definition_name("pub struct Origin {"), Some("Origin"));
        assert_eq!(definition_name("    pub enum Verdict {"), Some("Verdict"));
        assert_eq!(
            definition_name("pub(crate) struct EvidenceContext {"),
            Some("EvidenceContext")
        );
        assert_eq!(definition_name("pub type Result<T> = ..."), Some("Result"));
        // Not declarations: an import, a private item, a lowercase item.
        assert_eq!(definition_name("pub use origin::Origin;"), None);
        assert_eq!(definition_name("struct Private;"), None);
        assert_eq!(definition_name("pub fn origin() {}"), None);
    }

    /// A destination a worker can `use` today reads as usable.
    #[test]
    fn the_repaired_evidence_chain_canonicals_resolve() {
        let ws = workspace();
        for canonical in [
            "kernel_types::judgement::Verdict",
            "kernel_types::judgement::Judgement",
            "kernel_types::origin::Origin",
            "kernel_types::custody::Custody",
            "kernel_types::attribution::Attribution",
            "kernel_types::answer::Answer",
            "corpus_engine::index::EvidenceSet",
        ] {
            let r = ws.resolve(canonical);
            assert!(r.exists(), "{canonical}: {}", r.render());
        }
    }

    /// Re-exports are the load-bearing case a definition index would miss.
    #[test]
    fn a_re_exported_destination_resolves() {
        let ws = workspace();
        let r = ws.resolve("corpus_engine::Evidence");
        assert!(
            matches!(r, Resolution::ReExported { .. }),
            "corpus_engine::Evidence: {}",
            r.render()
        );
    }

    /// THE NEGATIVE CONTROL (ARCH §18.1). A check with no failing input you can
    /// name is not a check, so this pins the exact stale path the register
    /// carried until 2026-08-24: `sovereign-contracts` has no `verdict` module
    /// and never will — the type lives in the kernel, and `judgement.rs` says
    /// why. If this ever reads `usable`, the resolver has gone blind and every
    /// health line it prints is a false green.
    #[test]
    fn the_stale_canonical_this_check_was_built_for_is_refused() {
        let ws = workspace();
        let r = ws.resolve("sovereign_contracts::verdict::Verdict");
        assert!(!r.exists(), "the control must not resolve: {}", r.render());
        assert!(
            matches!(r, Resolution::Elsewhere { .. }),
            "Verdict is declared elsewhere in this workspace, so the verdict must \
             be Elsewhere and not Unbuilt — the two carry different diagnoses: {}",
            r.render()
        );
    }

    /// A home nobody has built is UNBUILT, not `Elsewhere` — honest future work
    /// rather than a lost move. Distinguishing them is the module's whole job.
    #[test]
    fn a_name_declared_nowhere_is_unbuilt_not_elsewhere() {
        let ws = workspace();
        assert_eq!(
            ws.resolve("kernel_types::judgement::NoSuchNounExistsHere"),
            Resolution::Unbuilt
        );
    }

    /// The three rows whose canonical is deliberately not a Rust path are
    /// reported as such, never counted as failures.
    #[test]
    fn a_non_type_canonical_is_named_rather_than_failed() {
        let ws = workspace();
        assert!(matches!(
            ws.resolve("sovereign/docs/cli-contract.toml"),
            Resolution::NotATypePath { .. }
        ));
        assert!(matches!(
            ws.resolve("sovereign_wire"),
            Resolution::NotATypePath { .. }
        ));
    }

    /// THE RATCHET, and it runs in both directions.
    ///
    /// Every row declares `home`; the tree is the arbiter. A `minted` row whose
    /// canonical stops resolving fails — that is the direction any check would
    /// have caught. A `planned` row whose canonical has quietly acquired a home
    /// fails too, and THAT is the direction that actually broke this register:
    /// seven nouns were minted in kernel-types across three rungs while their
    /// rows went on naming `sovereign_contracts::…` paths that never existed.
    ///
    /// There is no exception list. An earlier draft carried one as a const here,
    /// which is a second decider for a question `quality/CONCEPTS.toml` already
    /// answers (ARCH §10.6) — and being a test-local const, it could only ever
    /// encode the one direction its author thought of.
    #[test]
    fn every_register_home_matches_the_working_tree() {
        let root = super::super::census::repo_root().expect("repo root");
        let health = RegisterHealth::survey(&root).expect("survey");
        let bad = health.disagreements();
        assert!(
            bad.is_empty(),
            "the register and the tree disagree on {} row(s):\n{}",
            bad.len(),
            bad.iter()
                .map(|r| format!(
                    "  {} -> {}\n    tree says: {}\n    {}",
                    r.name,
                    r.canonical,
                    r.found.render(),
                    r.remedy()
                ))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// Both directions of the ratchet, exercised on constructed rows rather
    /// than on the live register — so the property is pinned even on a tree
    /// where every row happens to agree (ARCH §18.1: a check with no failing
    /// input you can name is not a check).
    #[test]
    fn a_disagreement_is_caught_whichever_way_it_points() {
        use super::super::labels::Home;
        let minted_but_absent = Row {
            name: "Ghost".into(),
            canonical: "kernel_types::ghost::Ghost".into(),
            declared: Home::Minted,
            found: Resolution::Unbuilt,
        };
        assert!(!minted_but_absent.agrees());
        assert!(minted_but_absent.remedy().contains("nothing is there"));

        let planned_but_present = Row {
            name: "Verdict".into(),
            canonical: "kernel_types::judgement::Verdict".into(),
            declared: Home::Planned,
            found: Resolution::Defined {
                file: "kernel-types/src/judgement.rs".into(),
                line: 91,
            },
        };
        assert!(
            !planned_but_present.agrees(),
            "a planned home that already exists is the drift that broke the register"
        );
        assert!(planned_but_present.remedy().contains("EXISTS"));

        // And the two agreeing shapes stay quiet.
        for (declared, found) in [
            (
                Home::Minted,
                Resolution::ReExported {
                    file: "x.rs".into(),
                },
            ),
            (Home::Planned, Resolution::Unbuilt),
        ] {
            let r = Row {
                name: "X".into(),
                canonical: "a::b::X".into(),
                declared,
                found,
            };
            assert!(r.agrees(), "{:?} must agree", r.declared);
        }
    }

    /// A contract file is a home too, and a deleted one is exactly as broken as
    /// a module nobody wrote. `Command`'s canonical is `cli-contract.toml` and
    /// its totality calls the contract GENERATIVE, so its absence is a failure
    /// rather than a category the check declines to judge.
    #[test]
    fn a_non_type_home_still_answers_whether_it_exists() {
        let ws = workspace();
        let present = ws.resolve("sovereign/docs/cli-contract.toml");
        assert!(present.exists(), "{}", present.render());
        assert!(matches!(present, Resolution::NotATypePath { .. }));

        let absent = ws.resolve("sovereign/docs/no-such-contract.toml");
        assert!(!absent.exists(), "{}", absent.render());

        // A crate as a home: `sovereign_wire` is planned and not yet a crate.
        let crate_home = ws.resolve("sovereign_wire");
        assert!(!crate_home.exists(), "{}", crate_home.render());
        assert!(
            ws.resolve("kernel_types").exists(),
            "an existing crate named as a home must read as present"
        );
    }
}
