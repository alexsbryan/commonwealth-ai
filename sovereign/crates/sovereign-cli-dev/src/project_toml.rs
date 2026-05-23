//! `.sovereign/project.toml` — ATOS lifecycle + observation surface.
//!
//! Distinct from the older `.sovereign/project.json` (which holds
//! transport/indexing config: port, corpus id, flags) — project.toml
//! is the stable record of what init observed and where the project
//! is in its ATOS lifecycle. `status`, `found`, `amend`, and `doctor`
//! read from here so none of them re-run observation on every call.
//!
//! Schema (v2):
//!
//! ```toml
//! schema_version = 2
//!
//! [project]
//! name = "sovereign"             # default = parent dir name
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
//! v1 → v2: introduces `[project] name` for initiative-to-project
//! matching by the strategic digest (Phase 8 of the Relational +
//! Strategic Awareness changeset). v1 files load fine — a missing
//! `[project]` section defaults to `name = ""`; the read path then
//! fills in the parent directory name on first save.
//!
//! Additive-only evolution: future fields go under existing tables
//! or new tables; never repurpose a key.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::observation::{
    DepKind, DetectedDependency, LanguageObservation, ProjectObservation, ScipTooling,
};

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectTomlFile {
    pub schema_version: u32,
    pub project: ProjectSection,
    pub observation: ObservationSection,
    pub lifecycle: LifecycleSection,
}

/// Stable identity of the project — currently just the human-readable
/// name used to match an `Initiative` entity from the conversational
/// atlas to a local ATOS project (see
/// `sovereign-tools::knowledge_view::timeline::AtosLookup`).
///
/// The name is editable: `project init` writes the parent directory
/// name as a default, and the user can later edit `.sovereign/project.toml`
/// directly. Empty `name` after read means "fall back to parent dir
/// at match time" — the read helper performs that fallback so callers
/// downstream always see a non-empty value.
#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct ProjectSection {
    /// Display + match name. Default-empty for v1 files; the loader
    /// fills it from the parent directory name when missing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
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
    /// User explicitly answered "no" to the init-time git prompt.
    /// Set once, never unset — the next `init` won't re-badger them.
    /// They can still `git init` manually later; the lack of a repo
    /// is then their deliberate choice, not an oversight.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub git_declined_at_init: bool,
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
            project: ProjectSection::default(),
            observation: ObservationSection::from_observation(obs),
            lifecycle: LifecycleSection::default(),
        }
    }

    pub fn read(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| std::io::Error::other(format!("parse project.toml: {e}")))
    }

    /// Read the file and ensure `project.name` is non-empty. When the
    /// file is v1 (no `[project]` section, or `name = ""`), fall
    /// back to the parent directory name of `.sovereign/`'s parent
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
    /// Also fills `project.name` from the parent directory if the
    /// existing file omitted it (v1 → v2 migration without losing
    /// any lifecycle state).
    pub fn update_observation(&mut self, obs: &ProjectObservation, project_toml_path: &Path) {
        self.observation = ObservationSection::from_observation(obs);
        if self.project.name.is_empty() {
            self.project.name = infer_project_name(project_toml_path);
        }
        // schema_version may have been defaulted to 0 when reading
        // a hypothetical future file; pin it to the current version
        // on write.
        self.schema_version = SCHEMA_VERSION;
    }
}

/// Best-effort default: parent-of-parent directory name (the dir
/// that contains `.sovereign/`). Returns empty if the path is too
/// shallow or non-UTF-8.
fn infer_project_name(project_toml_path: &Path) -> String {
    project_toml_path
        .parent() // .sovereign/
        .and_then(|p| p.parent()) // repo root
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_default()
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
    fn update_observation_fills_empty_project_name_from_parent_dir() {
        // A v1 file (empty project.name) gets its name populated when
        // `init` re-runs and calls update_observation. Lifecycle is
        // preserved, schema_version is bumped to current.
        let tmp = tempdir().unwrap();
        let repo_root = tmp.path().join("commonwealth-ai");
        std::fs::create_dir_all(&repo_root).unwrap();
        let toml_path = repo_root.join(".sovereign").join("project.toml");

        let mut file = ProjectTomlFile::default();
        file.schema_version = 1; // v1
        file.lifecycle.founded = true;
        file.lifecycle.charter_version = 3;
        // project.name intentionally empty.

        let obs = observation::observe(&repo_root);
        file.update_observation(&obs, &toml_path);

        assert_eq!(file.project.name, "commonwealth-ai");
        assert_eq!(file.schema_version, SCHEMA_VERSION);
        assert!(file.lifecycle.founded);
        assert_eq!(file.lifecycle.charter_version, 3);
    }

    #[test]
    fn update_observation_preserves_explicit_project_name() {
        let tmp = tempdir().unwrap();
        let repo_root = tmp.path().join("dir-name");
        std::fs::create_dir_all(&repo_root).unwrap();
        let toml_path = repo_root.join(".sovereign").join("project.toml");

        let mut file = ProjectTomlFile::default();
        file.schema_version = SCHEMA_VERSION;
        file.project.name = "Custom Name".into(); // user-edited

        let obs = observation::observe(&repo_root);
        file.update_observation(&obs, &toml_path);

        assert_eq!(
            file.project.name, "Custom Name",
            "non-empty name must not be overwritten"
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
        let project_toml_path = tmp.path().join(".sovereign/project.toml");
        file.update_observation(&obs2, &project_toml_path);
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
