// SPDX-License-Identifier: AGPL-3.0-or-later
//! The first-run plan and its progress line — what `svrn setup --plan --json`
//! prints and what `svrn setup --yes --json` streams.
//!
//! # Why these types are here
//!
//! A machine with no `config.toml` has no daemon to ask: the sidecar exits 1
//! off a TTY with no config (`sovereign-cli-daemon/src/daemon_cmd/mod.rs`,
//! the `SetupConfig::exists()` branch) and refuses a config with no `[models]`
//! (`daemon_cmd/build/inference.rs`, the `config.models()` arm). So first run
//! is not an HTTP call — a client SPAWNS the sidecar's own `setup` verb and
//! reads stdout. That is still a wire: two processes agreeing on a shape.
//!
//! These types were `sovereign_inference::hardware::{HardwareProfile,
//! ProfileName}`, `sovereign_inference::setup_planner::PrimaryOption` and
//! `sovereign_core::models_manifest::SlotConfig`. Each is pure serde over
//! primitives, and naming one used to cost a client the whole inference stack
//! or the runtime hub. They keep ONE definition (ARCH principle 8) and both
//! old paths `pub use` them, so every existing importer is unchanged — this
//! is a relocation, not a rename.

use serde::{Deserialize, Serialize};

use crate::oicp::CapabilityProfile;

/// Detected hardware capabilities used for model loading decisions.
///
/// Detection itself stays where the probe is: `sovereign_inference::hardware::
/// detect_hardware()` reads `sysinfo` and the llama.cpp backend device list,
/// neither of which belongs at this layer. An inherent `impl` cannot cross a
/// crate boundary, which is why `detect` is a free function there rather than
/// a constructor here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    /// Total system RAM in bytes, as the OS reports it.
    pub system_ram_bytes: u64,
    /// A non-CPU backend device was found (or this is Apple Silicon).
    pub gpu_available: bool,
    /// The backend device's name, when one was found.
    pub gpu_name: Option<String>,
    /// Discrete VRAM in bytes. On unified memory this repeats system RAM.
    pub gpu_memory_bytes: Option<u64>,
    /// `n_gpu_layers` to offload: 999 when a GPU is present, else 0.
    pub recommended_gpu_layers: u32,
    /// True on Apple Silicon (M-series) where GPU and CPU share the same
    /// unified memory pool. When true, `system_ram_bytes` is the effective
    /// VRAM for profile selection.
    pub is_unified_memory: bool,
}

impl HardwareProfile {
    /// Total system RAM in GiB.
    pub fn system_ram_gb(&self) -> f64 {
        self.system_ram_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    }

    /// Effective VRAM available for model loading.
    /// On unified memory systems (Apple Silicon) this is the full system RAM.
    /// On discrete GPU systems this is the GPU's VRAM.
    pub fn effective_vram_gb(&self) -> f32 {
        if self.is_unified_memory {
            self.system_ram_bytes as f32 / 1_073_741_824.0
        } else {
            self.gpu_memory_bytes.unwrap_or(0) as f32 / 1_073_741_824.0
        }
    }
}

/// Hardware-tier profile. Used to select the appropriate model sizes from
/// the models.toml manifest (cpu_only → low_mem → default → high → very_high).
///
/// The serde spelling is the manifest's own section key, so a `profile=` query
/// parameter, a `--plan --json` payload and a `[profiles.<name>]` lookup are
/// one vocabulary rather than three (ARCH principle 8). Before this moved, the
/// PascalCase derive form and four hand-written `match`es on the snake_case
/// form coexisted; [`ProfileName::as_str`] and [`ProfileName::from_wire`] are
/// now the only two sites that know the spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileName {
    /// No usable GPU.
    CpuOnly,
    /// 1–7 GB effective.
    LowMem,
    /// 8–19 GB effective.
    Default,
    /// 20–23 GB effective.
    High,
    /// 24 GB effective and up.
    VeryHigh,
}

impl ProfileName {
    /// The manifest section key, which is also the wire spelling.
    pub const fn as_str(&self) -> &'static str {
        match self {
            ProfileName::CpuOnly => "cpu_only",
            ProfileName::LowMem => "low_mem",
            ProfileName::Default => "default",
            ProfileName::High => "high",
            ProfileName::VeryHigh => "very_high",
        }
    }

    /// Parse the wire spelling. `None` for anything else — an unknown
    /// profile is REPORTED by the caller, never silently bucketed into
    /// `Default` (ARCH principle 6).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "cpu_only" => Some(ProfileName::CpuOnly),
            "low_mem" => Some(ProfileName::LowMem),
            "default" => Some(ProfileName::Default),
            "high" => Some(ProfileName::High),
            "very_high" => Some(ProfileName::VeryHigh),
            _ => None,
        }
    }

    /// Every tier, smallest first. The one list; `tier_rank` in
    /// `setup_planner` reads its order.
    pub const ALL: [ProfileName; 5] = [
        ProfileName::CpuOnly,
        ProfileName::LowMem,
        ProfileName::Default,
        ProfileName::High,
        ProfileName::VeryHigh,
    ];
}

/// A single slot's declared model — one `[profiles.<tier>.<slot>]` row of
/// `models.toml`, and the shape the setup plan describes a download with.
///
/// The fields read at runtime are `file` (for matching loaded GGUFs) and
/// `capabilities` (for OICP routing). Everything else is documentation the
/// picker renders — serde ignores unknown TOML keys for structs by default,
/// so `quirks_override` and friends pass through untouched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlotConfig {
    /// The GGUF filename, which is also the key a loaded model matches on.
    pub file: String,
    /// Optional model identity — the stable substring that
    /// uniquely names this base model across quantisations and
    /// repo uploads. `"Qwen3.5-27B"` matches both
    /// `Qwen_Qwen3.5-27B-Q4_K_M.gguf` (bartowski's Q4) and
    /// `Qwen3.5-27B.Q8_0.gguf` (a local Q8 dump) and any future
    /// `*-Q5_K_S.gguf`. When absent, capability lookup falls
    /// back to exact filename match only.
    ///
    /// Declaring `base_name` lets a single manifest row cover
    /// every quantisation of the same weights — capabilities are
    /// a property of the model, not the quant.
    #[serde(default)]
    pub base_name: String,
    /// Architecture family, for the prompt-format and quirks lookup.
    #[serde(default)]
    pub family: String,
    /// Quantisation label as the upload spells it (`"Q4_K_M"`).
    #[serde(default)]
    pub quant: String,
    /// Advertised artifact size. The download validator uses it as a floor,
    /// which is what catches a 200 KB HTML error page saved as a 35 GB model.
    #[serde(default)]
    pub size_gb: f64,
    /// The model emits reasoning traces.
    #[serde(default)]
    pub thinking: bool,
    /// The HuggingFace repo landing page. `hf_download_url` derives the raw
    /// `/resolve/main/<file>` link from it.
    #[serde(default)]
    pub hf_url: String,
    /// OICP capability declarations for this base model. Empty
    /// when absent — the mesh routing path falls back to
    /// conservative defaults for BYOM or legacy entries that
    /// haven't been annotated yet.
    #[serde(default)]
    pub capabilities: CapabilityProfile,
}

/// One row in the curated primary-model picker. Carries the slot
/// definition plus a `recommended` flag so callers know which entry
/// is the default for the detected hardware.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrimaryOption {
    /// Profile this slot was drawn from — the [`ProfileName::as_str`]
    /// spelling. A `String` rather than the enum because the curated
    /// opt-in alternatives borrow a tier's key as a display/sizing label
    /// without being a selectable hardware tier.
    pub profile: String,
    /// The slot itself.
    pub slot: SlotConfig,
    /// This is the pick for the detected hardware.
    pub recommended: bool,
}

impl std::ops::Deref for PrimaryOption {
    type Target = SlotConfig;
    fn deref(&self) -> &Self::Target {
        &self.slot
    }
}

/// What `svrn setup --plan --json` prints: everything a first-run wizard
/// needs to render its screens before anything is downloaded or written.
///
/// It is a PLAN, not a result — running it changes nothing on disk, which is
/// why a client may ask for it on a machine that has never been set up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupPlan {
    /// What the probe found.
    pub hardware: HardwareProfile,
    /// The tier that follows from it.
    pub profile: ProfileName,
    /// Every primary the tier can run, recommended one flagged.
    pub catalog: Vec<PrimaryOption>,
    /// The fast slot for this tier. `None` means the bundled manifest has
    /// none — reported, never substituted.
    pub fast: Option<SlotConfig>,
    /// The embedding slot for this tier, same rule.
    pub embed: Option<SlotConfig>,
}

/// The phases `svrn setup --yes --json` reports, in the order they occur.
///
/// A closed set, so an enum (ARCH principle 9). The names are the ones the
/// desktop's `SetupPhase` already narrates, so the mapping on the other side
/// is a `match` with no invented cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupProgressPhase {
    /// Reading what this machine can do.
    DetectingHardware,
    /// Creating the data root and its subdirectories.
    PreparingDataDir,
    /// Fetching the thoughtful slot.
    DownloadingPrimary,
    /// Fetching the quick slot.
    DownloadingFast,
    /// Fetching the embedding slot.
    DownloadingEmbed,
    /// Writing `config.toml`.
    WritingConfig,
    /// Terminal success. `config_path` is populated.
    Done,
    /// Terminal failure. `error` is populated.
    Failed,
}

/// One line of `svrn setup --yes --json` stdout: exactly one JSON object,
/// newline-terminated, one phase per line.
///
/// Absent fields are absent, not zero — a download with no `Content-Length`
/// reports `total: None` and the renderer shows an indeterminate bar, rather
/// than a fraction computed against a fabricated denominator (ARCH principle
/// 6). The exit code is still the verdict; these lines are the narration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupProgressLine {
    /// Which phase this line reports.
    pub phase: SetupProgressPhase,
    /// One human sentence, safe to render verbatim.
    pub message: String,
    /// The artifact being fetched, on download phases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Bytes received so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloaded: Option<u64>,
    /// Bytes expected, when the server said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// `downloaded / total`, clamped to 0..=1. `None` when `total` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
    /// Seconds remaining from a rolling rate, when one can be computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<u64>,
    /// Where the config was written. Populated on [`SetupProgressPhase::Done`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    /// Why the run failed. Populated on [`SetupProgressPhase::Failed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SetupProgressLine {
    /// A phase announcement with no numbers attached.
    pub fn narration(phase: SetupProgressPhase, message: impl Into<String>) -> Self {
        Self {
            phase,
            message: message.into(),
            file: None,
            downloaded: None,
            total: None,
            fraction: None,
            eta_seconds: None,
            config_path: None,
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire spelling and the manifest section key are the same string,
    /// in both directions, for every variant. Watched fail: change one arm
    /// of `as_str` and this goes red on that tier.
    #[test]
    fn profile_name_round_trips_through_its_wire_spelling() {
        for p in ProfileName::ALL {
            assert_eq!(ProfileName::from_wire(p.as_str()), Some(p), "{p:?}");
            let json = serde_json::to_string(&p).expect("serialize");
            assert_eq!(
                json,
                format!("\"{}\"", p.as_str()),
                "serde and as_str must spell {p:?} the same way"
            );
        }
    }

    /// An unknown profile is refused, not bucketed. Watched fail: replace
    /// the `_ => None` arm with `_ => Some(ProfileName::Default)`.
    #[test]
    fn an_unknown_profile_is_refused_rather_than_defaulted() {
        assert_eq!(ProfileName::from_wire("gigantic"), None);
        assert_eq!(ProfileName::from_wire("Default"), None);
        assert_eq!(ProfileName::from_wire(""), None);
    }

    /// Absent progress numbers stay absent on the wire. Watched fail: drop
    /// the `skip_serializing_if` attributes and `total` appears as `null`,
    /// which a renderer reading `total ?? 0` turns into a 0-byte download.
    #[test]
    fn a_narration_line_carries_no_null_numbers() {
        let line = SetupProgressLine::narration(
            SetupProgressPhase::PreparingDataDir,
            "Preparing storage.",
        );
        let json = serde_json::to_string(&line).expect("serialize");
        assert_eq!(
            json,
            r#"{"phase":"preparing_data_dir","message":"Preparing storage."}"#
        );
    }
}
