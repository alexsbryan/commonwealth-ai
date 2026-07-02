// SPDX-License-Identifier: AGPL-3.0-or-later
//! Loader + types for the CLI contract manifest (`docs/cli-contract.toml`).
//!
//! The manifest is the single source of truth for the commands the
//! `sovereign` CLI promises. Three consumers reconcile against it, all
//! parsing through this one code path (no second parser):
//!
//! - `cli_contract_code` test — manifest ↔ real binaries: every promised
//!   command must dispatch (forward); no real command may be untracked
//!   (reverse, via `__dump-commands`).
//! - `cli_contract_docs` test — manifest ↔ `CLI_REFERENCE.md` / `README.md`,
//!   tiered by [`Visibility`].
//! - `cli-contract-live-verify.sh` — read-only [`Smoke`] probes against a
//!   running daemon (emitted as TSV by the dev-gated `__contract-smoke` arm).
//!
//! Two axes are deliberately orthogonal:
//! - [`Feature`] is the *build gate* — does the command dispatch in this
//!   cargo build (`default` vs `dev-tools` vs `awareness`).
//! - [`Visibility`] is the *audience* — is it a public contract surface
//!   (inference + mesh + knowledge bases, what the top-level READMEs
//!   surface) or internal/dev tooling. A `public` command that is also
//!   `dev-tools` is a contradiction (it can't run in the shipped binary)
//!   and the conformance tests flag it.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// One command the CLI promises. Primary key is [`Command::path`] — the
/// argv path the user types, minus the leading `sovereign`, space-joined
/// (e.g. `"mesh create"`, `"pipeline pod up"`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    /// Argv path minus the `sovereign` prefix. The primary key.
    pub path: String,
    /// Which binary dispatches it after routing.
    pub binary: Binary,
    /// Whether it is help/parse-probeable offline, or needs a live daemon.
    #[serde(default)]
    pub classification: Classification,
    /// Build gate the command lives behind.
    #[serde(default)]
    pub feature: Feature,
    /// Audience tier (public contract vs internal tooling).
    #[serde(default)]
    pub visibility: Visibility,
    /// Flags the command promises (contract-checked against docs, lenient).
    #[serde(default)]
    pub flags: Vec<Flag>,
    /// How the offline code-probe verifies the command exists.
    #[serde(default)]
    pub probe: Probe,
    /// Required when `probe = "skip"`: why the command can't be probed.
    #[serde(default)]
    pub probe_skip_reason: Option<String>,
    /// Intentionally undocumented surface: present in the binary on
    /// purpose (legacy/internal), exempt from the docs check but still
    /// tracked so the reverse code-check finds no *untracked* command.
    #[serde(default)]
    pub hidden: bool,
    /// If set, this row is a synonym spelling of the named canonical path.
    #[serde(default)]
    pub alias_of: Option<String>,
    /// Optional read-only live probe (only meaningful when
    /// `classification = "daemon"`).
    #[serde(default)]
    pub smoke: Option<Smoke>,
}

/// Which sibling binary handles the command after dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Binary {
    /// Handled in-process by the `sovereign-cli` dispatcher itself.
    Dispatcher,
    /// `sovereign-cli-dev` (project / code / atos / tools).
    Dev,
    /// `sovereign-cli-llm` (mesh / corpus / chat / enrich / bench / …).
    Llm,
    /// `sovereign-cli-daemon` (daemon / setup / doctor / install-service).
    Daemon,
}

/// Whether the command can be exercised offline or needs a live daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Classification {
    /// `--help` / parse / exit-code testable with no daemon, no model.
    #[default]
    Offline,
    /// Needs `localhost:9741` + loaded models to do real work.
    Daemon,
}

/// The cargo build gate the command lives behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Feature {
    /// Present in the shipped (product) build.
    #[default]
    Default,
    /// Only when built `--features dev-tools` (+ the `sovereign-cli-dev` sibling).
    DevTools,
    /// Only when built `--features awareness`.
    Awareness,
}

/// Audience tier — the public contract vs internal/dev tooling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Surfaced in the top-level READMEs: local inference + mesh +
    /// knowledge bases. Held to the strictest conformance bar.
    Public,
    /// Internal / developer tooling. Documented in CLI_REFERENCE (the full
    /// reference) but not required in the READMEs.
    #[default]
    Internal,
}

/// How the offline code-conformance probe verifies a command exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Probe {
    /// `<path> --help` exits 0 and output is not an "unknown subcommand"
    /// miss. The common case (the handler honors `help::wants_help`).
    #[default]
    Help,
    /// `<path>` with no args; assert dispatch reached the handler (output
    /// is not a dispatcher miss); exit code ignored. For leaf handlers
    /// that treat `--help` as a positional (e.g. `atos spec diff`).
    NoArgs,
    /// Not code-probed (requires `probe_skip_reason`): interactive,
    /// mutating-on-bare-invocation, or otherwise unprobeable offline.
    Skip,
}

/// A flag the command promises. Checked (leniently) against the docs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flag {
    /// Long-form flag, e.g. `"--rebuild-index"`.
    pub name: String,
    /// Whether the flag takes a value (`--name <id>`).
    #[serde(default)]
    pub takes_value: bool,
    /// Whether the flag is required (informational today).
    #[serde(default)]
    pub required: bool,
}

/// A safe, read-only invocation the live harness may run against a daemon.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Smoke {
    /// Full argv (minus the binary), e.g. `["mesh", "status"]`.
    pub args: Vec<String>,
    /// Expected exit code (default 0).
    #[serde(default)]
    pub expect_exit: i32,
    /// Optional substring the stdout must contain.
    #[serde(default)]
    pub expect_stdout_contains: Option<String>,
}

/// The parsed manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    /// Schema version (bump on a breaking schema change).
    #[serde(default)]
    pub schema_version: u32,
    /// Every promised command. Maps the TOML `[[command]]` array.
    #[serde(default, rename = "command")]
    pub commands: Vec<Command>,
}

impl Contract {
    /// Parse a manifest from a TOML string. Used by tests with inline
    /// fixtures; `load`/`load_default` wrap it with file I/O.
    pub fn parse(toml_str: &str) -> Result<Contract, String> {
        toml::from_str(toml_str).map_err(|e| format!("parse cli-contract.toml: {e}"))
    }

    /// Load and parse the manifest at `path`.
    pub fn load(path: &Path) -> Result<Contract, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Self::parse(&text)
    }

    /// Load the committed manifest at its canonical path.
    pub fn load_default() -> Result<Contract, String> {
        Self::load(&manifest_path())
    }

    /// Canonical, user-facing commands: alias spellings and intentionally
    /// hidden surface filtered out. The set the docs check holds.
    pub fn canonical(&self) -> impl Iterator<Item = &Command> {
        self.commands
            .iter()
            .filter(|c| !c.hidden && c.alias_of.is_none())
    }
}

/// Resolve `docs/cli-contract.toml` relative to this crate at compile time.
/// The crate lives at `sovereign/crates/sovereign-cli-shared`; the manifest
/// at `sovereign/docs/cli-contract.toml` — two ancestors up, then `docs/`.
pub fn manifest_path() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../sovereign/crates/sovereign-cli-shared
    //   ancestors[0] = .../sovereign-cli-shared
    //   ancestors[1] = .../crates
    //   ancestors[2] = .../sovereign
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(|sovereign_root| sovereign_root.join("docs").join("cli-contract.toml"))
        .unwrap_or_else(|| PathBuf::from("docs/cli-contract.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_command() {
        let toml = r#"
schema_version = 1
[[command]]
path = "mesh create"
binary = "llm"
classification = "offline"
feature = "default"
visibility = "public"
flags = [ { name = "--name", takes_value = true } ]
[command.smoke]
args = ["mesh", "status"]
expect_exit = 0
expect_stdout_contains = "members"
"#;
        let c = Contract::parse(toml).expect("parse");
        assert_eq!(c.schema_version, 1);
        assert_eq!(c.commands.len(), 1);
        let cmd = &c.commands[0];
        assert_eq!(cmd.path, "mesh create");
        assert_eq!(cmd.binary, Binary::Llm);
        assert_eq!(cmd.classification, Classification::Offline);
        assert_eq!(cmd.feature, Feature::Default);
        assert_eq!(cmd.visibility, Visibility::Public);
        assert_eq!(cmd.probe, Probe::Help); // default
        assert!(!cmd.hidden);
        assert_eq!(cmd.flags.len(), 1);
        assert_eq!(cmd.flags[0].name, "--name");
        assert!(cmd.flags[0].takes_value);
        let smoke = cmd.smoke.as_ref().expect("smoke present");
        assert_eq!(smoke.args, vec!["mesh", "status"]);
        assert_eq!(smoke.expect_exit, 0);
    }

    #[test]
    fn defaults_apply_when_fields_absent() {
        let toml = r#"
[[command]]
path = "voice"
binary = "llm"
"#;
        let c = Contract::parse(toml).expect("parse");
        let cmd = &c.commands[0];
        assert_eq!(cmd.classification, Classification::Offline);
        assert_eq!(cmd.feature, Feature::Default);
        assert_eq!(cmd.visibility, Visibility::Internal); // internal by default
        assert_eq!(cmd.probe, Probe::Help);
        assert!(!cmd.hidden);
        assert!(cmd.alias_of.is_none());
        assert!(cmd.smoke.is_none());
    }

    #[test]
    fn kebab_and_alias_round_trip() {
        let toml = r#"
[[command]]
path = "enrich cluster-atlas"
binary = "llm"
feature = "dev-tools"
probe = "no-args"
alias_of = "enrich cluster"

[[command]]
path = "atos spec diff"
binary = "dev"
feature = "dev-tools"
visibility = "internal"
probe = "no-args"
"#;
        let c = Contract::parse(toml).expect("parse");
        assert_eq!(c.commands[0].feature, Feature::DevTools);
        assert_eq!(c.commands[0].probe, Probe::NoArgs);
        assert_eq!(c.commands[0].alias_of.as_deref(), Some("enrich cluster"));
        // canonical() drops the alias row.
        assert_eq!(c.canonical().count(), 1);
        assert_eq!(c.canonical().next().unwrap().path, "atos spec diff");
    }

    #[test]
    fn the_committed_manifest_parses_and_is_nonempty() {
        // Guards the real docs/cli-contract.toml: it must always parse and
        // declare at least the public surface.
        let c = Contract::load_default().expect("docs/cli-contract.toml must parse");
        assert!(!c.commands.is_empty(), "manifest declares no commands");
    }
}
