// SPDX-License-Identifier: AGPL-3.0-or-later
//! The refactor SPEC — a refactor is DATA, not code (ARCH §6).
//!
//! One shape covers all five work-table kinds; specs live in
//! `quality/refactors/*.toml`. The `[rules]` table is shared in spirit across
//! specs: `&str <- &T` is the same edit for every string newtype, so atom 2
//! inherits atom 1's rules by copying the table into its own spec today and by
//! a shared table when there are enough atoms to justify one.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// The five work-table kinds. A closed set, so an enum (ARCH §2) — a spec
/// naming a sixth kind is a parse error, not a silently-accepted string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpecKind {
    Newtype,
    AdoptApi,
    DeleteLoser,
    MergeShape,
    RetypeField,
}

impl SpecKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Newtype => "newtype",
            Self::AdoptApi => "adopt-api",
            Self::DeleteLoser => "delete-loser",
            Self::MergeShape => "merge-shape",
            Self::RetypeField => "retype-field",
        }
    }
}

/// The edit that makes rustc enumerate: retype field `field` from `from` to
/// the spec target, then read every diagnostic.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedEdit {
    pub field: String,
    pub from: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoverSpec {
    pub seed: SeedEdit,
}

/// The wire fixture is a NAMED TEST, run by `plan` — proven, never asserted.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireFixture {
    pub package: String,
    pub test: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafetySpec {
    /// Declared wire form ("transparent", "hex-string", "byte-array", ...).
    /// The entry gate verifies this against the definition's serde surface;
    /// a declaration the source contradicts fails the gate.
    pub wire: String,
    #[serde(default)]
    pub surfaces: Vec<String>,
    pub fixture: Option<WireFixture>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareSpec {
    #[serde(default)]
    pub impls: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefactorSpec {
    pub id: String,
    pub kind: SpecKind,
    /// Fully-qualified target, e.g. `kernel_types::CorpusId`.
    pub target: String,
    pub discover: DiscoverSpec,
    pub safety: Option<SafetySpec>,
    #[serde(default)]
    pub prepare: PrepareSpec,
    /// `"<expected> <- <found>" -> edit`. Classes without a rule are RESIDUE —
    /// the only agentic stage, and it is not this command.
    #[serde(default)]
    pub rules: BTreeMap<String, String>,
}

impl RefactorSpec {
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        toml::from_str(&raw).map_err(|e| format!("parsing {}: {e}", path.display()))
    }

    /// Bare type name: `CorpusId` from `kernel_types::CorpusId`.
    pub fn target_name(&self) -> &str {
        self.target.rsplit("::").next().unwrap_or(&self.target)
    }

    /// Extern crate name of the target: `kernel_types`.
    pub fn target_crate(&self) -> &str {
        self.target.split("::").next().unwrap_or(&self.target)
    }

    /// Package name as Cargo spells it (`kernel-types` for `kernel_types`).
    pub fn target_package(&self) -> String {
        self.target_crate().replace('_', "-")
    }

    /// Rule lookup for a classified `(expected, found)` pair.
    pub fn rule_for(&self, expected: &str, found: &str) -> Option<&str> {
        self.rules
            .get(&format!("{expected} <- {found}"))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"
id     = "corpus-id"
kind   = "newtype"
target = "kernel_types::CorpusId"

[discover]
seed = { field = "corpus_id", from = "String" }

[safety]
wire     = "transparent"
surfaces = ["json", "sqlite"]
fixture  = { package = "kernel-types", test = "corpus_id_is_transparent_on_the_wire" }

[prepare]
impls = ["AsRef<str>"]

[rules]
"&str <- &CorpusId" = "append .as_str()"
"#;

    #[test]
    fn spec_round_trips_and_derives_names() {
        let s: RefactorSpec = toml::from_str(SPEC).expect("parse");
        assert_eq!(s.kind, SpecKind::Newtype);
        assert_eq!(s.target_name(), "CorpusId");
        assert_eq!(s.target_crate(), "kernel_types");
        assert_eq!(s.target_package(), "kernel-types");
        assert_eq!(s.rule_for("&str", "&CorpusId"), Some("append .as_str()"));
        assert_eq!(s.rule_for("String", "CorpusId"), None);
        let fx = s.safety.as_ref().and_then(|s| s.fixture.as_ref()).unwrap();
        assert_eq!(fx.package, "kernel-types");
    }

    #[test]
    fn a_sixth_kind_is_a_parse_error_not_a_string() {
        // Closed sets are enums (ARCH §2): the spec format cannot grow a kind
        // by typo.
        let bad = SPEC.replace("\"newtype\"", "\"rewrite-everything\"");
        assert!(toml::from_str::<RefactorSpec>(&bad).is_err());
    }

    #[test]
    fn unknown_spec_fields_are_refused() {
        let bad = SPEC.replace("[prepare]", "[prepare]\nimpel = []\n[dead]");
        assert!(toml::from_str::<RefactorSpec>(&bad).is_err());
    }
}
