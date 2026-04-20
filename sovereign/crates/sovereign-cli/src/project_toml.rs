//! `.sovereign/project.toml` — ATOS lifecycle + observation surface.
//!
//! Distinct from the older `.sovereign/project.json` (which holds
//! transport/indexing config: port, corpus id, flags) — project.toml
//! is the stable record of what init observed and where the project
//! is in its ATOS lifecycle. `status`, `found`, `amend`, and `doctor`
//! read from here so none of them re-run observation on every call.
//!
//! Schema (v1):
//!
//! ```toml
//! schema_version = 1
//!
//! [observation]
//! observed_at = 1712345678       # unix seconds
//! has_git = true
//! embed_model_available = false
//!
//! [[observation.language]]
//! id = "rust"
//! display = "Rust workspace (12 crates)"
//! scip = "not_required"          # "available" | "missing" | "not_required"
//! scip_binary = ""               # populated when available/missing
//! scip_install_cmd = ""          # populated when missing
//!
//! [[observation.dep]]
//! name = "reqwest"
//! version = "0.11"
//! source_file = "Cargo.toml"
//! kind = "direct"                # "direct" | "dev"
//!
//! [lifecycle]
//! founded = false
//! charter_version = 0
//! current_phase = 0
//! ```
//!
//! Additive-only evolution: future fields go under existing tables
//! or new tables; never repurpose a key. `schema_version` bumps on
//! breaking reshape (there won't be one in M6).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::observation::{
    DepKind, DetectedDependency, LanguageObservation, ProjectObservation, ScipTooling,
};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectTomlFile {
    pub schema_version: u32,
    pub observation: ObservationSection,
    pub lifecycle: LifecycleSection,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ObservationSection {
    pub observed_at: i64,
    pub has_git: bool,
    pub embed_model_available: bool,
    /// Serialized as `[[observation.language]]`.
    #[serde(rename = "language", default)]
    pub languages: Vec<LanguageEntry>,
    /// Serialized as `[[observation.dep]]`.
    #[serde(rename = "dep", default)]
    pub deps: Vec<DepEntry>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LanguageEntry {
    pub id: String,
    pub display: String,
    pub scip: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub scip_binary: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub scip_install_cmd: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DepEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub source_file: String,
    pub kind: String,
}

/// Lifecycle fields populated by `sovereign project found` /
/// `amend`. M6.1 leaves them at defaults; M6.3+ fills them in.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LifecycleSection {
    pub founded: bool,
    pub charter_version: u32,
    pub current_phase: u32,
    /// SHA-256 of `.sovereign/CHARTER.md` at founding/amend time.
    /// Subsequent sessions compare against this to detect drift
    /// (charter edited outside the amendment flow). Empty when
    /// not yet founded.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub charter_hash: String,
}

// ─── Conversion ──────────────────────────────────────────────────────────────

impl ProjectTomlFile {
    /// Build a fresh file from a live observation. Lifecycle stays
    /// at defaults. Callers that want to preserve existing lifecycle
    /// state should [`Self::read`] first and replace only
    /// `observation`.
    pub fn from_observation(obs: &ProjectObservation) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            observation: ObservationSection::from_observation(obs),
            lifecycle: LifecycleSection::default(),
        }
    }

    pub fn read(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| std::io::Error::other(format!("parse project.toml: {e}")))
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::other(format!("serialize project.toml: {e}")))?;
        std::fs::write(path, body)
    }

    /// Replace the observation section, preserve lifecycle. Use this
    /// from `init` so re-running doesn't reset the founded flag.
    pub fn update_observation(&mut self, obs: &ProjectObservation) {
        self.observation = ObservationSection::from_observation(obs);
        // schema_version may have been defaulted to 0 when reading
        // a hypothetical future file; pin it to the current version
        // on write.
        self.schema_version = SCHEMA_VERSION;
    }
}

impl ObservationSection {
    fn from_observation(obs: &ProjectObservation) -> Self {
        Self {
            observed_at: unix_now(),
            has_git: obs.has_git,
            embed_model_available: obs.embed_model_available,
            languages: obs
                .languages
                .iter()
                .map(LanguageEntry::from_observation)
                .collect(),
            deps: obs.deps.iter().map(DepEntry::from_observation).collect(),
        }
    }
}

impl LanguageEntry {
    fn from_observation(lang: &LanguageObservation) -> Self {
        let (tag, binary, install) = match &lang.scip_tooling {
            ScipTooling::Available { binary } => ("available", (*binary).into(), String::new()),
            ScipTooling::Missing {
                binary,
                install_cmd,
            } => ("missing", (*binary).into(), (*install_cmd).into()),
            ScipTooling::NotRequired => ("not_required", String::new(), String::new()),
        };
        Self {
            id: lang.id.clone(),
            display: lang.display.clone(),
            scip: tag.into(),
            scip_binary: binary,
            scip_install_cmd: install,
        }
    }
}

impl DepEntry {
    fn from_observation(dep: &DetectedDependency) -> Self {
        Self {
            name: dep.name.clone(),
            version: dep.version.clone(),
            source_file: dep.source_file.clone(),
            kind: match dep.kind {
                DepKind::Direct => "direct".into(),
                DepKind::Dev => "dev".into(),
            },
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation;
    use tempfile::tempdir;

    #[test]
    fn round_trip_preserves_all_fields() {
        let tmp = tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1\"\n\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        let obs = observation::observe(tmp.path());
        let mut file = ProjectTomlFile::from_observation(&obs);
        file.lifecycle.founded = true;
        file.lifecycle.charter_version = 2;
        file.lifecycle.current_phase = 1;

        let path = tmp.path().join(".sovereign").join("project.toml");
        file.write(&path).unwrap();
        let reloaded = ProjectTomlFile::read(&path).unwrap();
        assert_eq!(reloaded.schema_version, SCHEMA_VERSION);
        assert_eq!(reloaded.lifecycle.founded, true);
        assert_eq!(reloaded.lifecycle.charter_version, 2);
        assert_eq!(reloaded.lifecycle.current_phase, 1);
        assert_eq!(reloaded.observation.languages.len(), 1);
        assert_eq!(reloaded.observation.languages[0].id, "rust");
        assert_eq!(reloaded.observation.languages[0].scip, "not_required");
        assert!(
            reloaded.observation.deps.iter().any(|d| d.name == "serde"),
            "deps survive round trip"
        );
    }

    #[test]
    fn update_observation_preserves_lifecycle() {
        // Simulate: init runs, found sets lifecycle, init runs again.
        // The second init must NOT wipe founded=true.
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("go.mod"), "module x\n").unwrap();
        let mut file = ProjectTomlFile::from_observation(&observation::observe(tmp.path()));
        file.lifecycle.founded = true;
        file.lifecycle.charter_version = 1;
        file.lifecycle.current_phase = 2;

        // Second observation after some change.
        std::fs::write(
            tmp.path().join("go.mod"),
            "module x\nrequire github.com/foo/bar v1.0.0\n",
        )
        .unwrap();
        let obs2 = observation::observe(tmp.path());
        file.update_observation(&obs2);
        assert!(file.lifecycle.founded, "founded survives re-observation");
        assert_eq!(file.lifecycle.charter_version, 1);
        assert_eq!(file.lifecycle.current_phase, 2);
        assert!(
            file.observation
                .deps
                .iter()
                .any(|d| d.name == "github.com/foo/bar"),
            "updated deps are picked up"
        );
    }

    #[test]
    fn scip_missing_round_trips_binary_and_install_command() {
        let lang = LanguageObservation {
            id: "go".into(),
            display: "Go".into(),
            scip_tooling: ScipTooling::Missing {
                binary: "scip-go",
                install_cmd: "go install github.com/sourcegraph/scip-go@latest",
            },
        };
        let entry = LanguageEntry::from_observation(&lang);
        assert_eq!(entry.scip, "missing");
        assert_eq!(entry.scip_binary, "scip-go");
        assert!(entry.scip_install_cmd.contains("go install"));

        let text = toml::to_string_pretty(&entry).unwrap();
        assert!(
            text.contains("scip_install_cmd"),
            "install_cmd must be serialized, not skipped"
        );
    }
}
