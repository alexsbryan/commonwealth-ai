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
//! # Commands are a set; journeys are a sequence
//!
//! The `[[command]]` rows above prove each verb *exists*. They cannot prove
//! that any ordered sequence of them *works* — install→query→remove was
//! unverified anywhere in this repo until [`Journey`] was added. A journey is
//! a named, ordered, persona-tagged use case whose steps each resolve back to
//! a declared [`Command`]; three consumers reconcile against it:
//!
//! - `cli_contract_journeys` test — static: every step resolves to a real
//!   command, every public command belongs to a journey or is ledgered in
//!   [`Stranded`], every journey cites a doc that exists.
//! - `cli_journey_dispatch` test — offline: replays each journey's argv in
//!   order and asserts no dispatch miss.
//! - `cli-journey-verify.sh` — live: runs the sequence against a real daemon
//!   in a hermetic HOME and asserts [`Expect`], including that a `mutates`
//!   step's effect is visible and later reversed.
//!
//! [`Stranded`] is the ledger of verbs that belong to no journey. It exists
//! so "stranded" is a tracked, *shrinking* quantity rather than a silent one:
//! the ratchet test forbids it growing.
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

// ── Journeys ────────────────────────────────────────────────────────────

/// A named, ordered use case: the sequence a person actually types to get
/// something done. Primary key is [`Journey::id`].
///
/// Journeys are derived from the sequences the docs already teach, not
/// invented — [`Journey::doc`] cites where, and the static test checks that
/// citation still resolves.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journey {
    /// Stable kebab-case id, e.g. `"corpus-lifecycle"`. The primary key.
    pub id: String,
    /// One-line human title: what the person is trying to accomplish.
    pub title: String,
    /// Who runs it.
    pub persona: Persona,
    /// User-impact tier, 1 (product is broken without it) to 5 (specialist
    /// loop). Drives live-run ordering and triage severity, mirroring the
    /// desktop journey manifest.
    pub tier: u8,
    /// Audience tier. A `public` journey may not contain a `dev-tools` step.
    #[serde(default)]
    pub visibility: Visibility,
    /// Where this sequence is taught, as a repo-relative path with an
    /// optional `#anchor` or `:line`. Checked for existence by the static test.
    #[serde(default)]
    pub doc: Option<String>,
    /// Why this journey is not run by the live harness. `Some` = skipped
    /// live (multi-machine, destructive, needs paid infra) but still
    /// statically checked and dispatch-replayed.
    #[serde(default)]
    pub skip_live: Option<String>,
    /// The ordered steps. Maps the TOML `[[journey.step]]` array.
    #[serde(default, rename = "step")]
    pub steps: Vec<JourneyStep>,
}

/// One step of a [`Journey`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JourneyStep {
    /// What the user types, minus the leading `sovereign`. May carry
    /// arguments and `{placeholder}` tokens, e.g.
    /// `"corpus install {corpus}"`. The declared [`Command`] is resolved by
    /// longest-prefix match — see [`Contract::resolve_step`].
    pub run: String,
    /// Overrides prefix resolution when `run` is ambiguous. Must equal a
    /// declared [`Command::path`].
    #[serde(default)]
    pub command: Option<String>,
    /// This step changes state. The live harness requires that a journey
    /// containing a mutating step also asserts the mutation (a later step
    /// with `stdout_contains`) and, where reversible, reverses it.
    #[serde(default)]
    pub mutates: bool,
    /// What the live harness asserts about this step.
    #[serde(default)]
    pub expect: Option<Expect>,
    /// Why this step is not run live (e.g. needs a second machine). The
    /// step is still statically checked and dispatch-replayed.
    #[serde(default)]
    pub skip_live: Option<String>,
    /// Seconds this step's assertions may be re-checked before failing,
    /// for commands whose effect lands ASYNCHRONOUSLY.
    ///
    /// `corpus install` is the motivating case: it POSTs to the daemon and
    /// returns immediately, the ingest runs in a task, and the corpus becomes
    /// visible to `corpus status` a moment later. Asserting instantly after it
    /// asserts something the command never promised, so the step failed for a
    /// reason that had nothing to do with correctness.
    ///
    /// This is NOT a flake allowance. The assertion is unchanged and still has
    /// to hold; the step is simply given the system's own documented latency
    /// to produce it, and the runner reports how long it waited. Steps without
    /// it are checked exactly once, as before — do not sprinkle it on a step
    /// that is merely unreliable.
    #[serde(default)]
    pub settle_secs: Option<u64>,
    /// Free-text note rendered in the journey report.
    #[serde(default)]
    pub note: Option<String>,
}

/// What a live journey step must produce. Deliberately richer than
/// [`Smoke`]: exit-code-only assertions are why `code search` (a Phase-2
/// placeholder that prints its own stub text and exits 0) reads as working.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Expected exit code. `None` = not asserted.
    #[serde(default)]
    pub exit: Option<i32>,
    /// Substring the combined output must contain.
    #[serde(default)]
    pub stdout_contains: Option<String>,
    /// Substring the combined output must NOT contain — how a journey
    /// proves a removal actually removed something.
    #[serde(default)]
    pub stdout_absent: Option<String>,
    /// Output must be non-empty after trimming.
    #[serde(default)]
    pub stdout_non_empty: bool,
}

impl Expect {
    /// Whether this assertion looks at output at all, or only at the exit
    /// code. The ratchet uses this to require real assertions on the
    /// public tier.
    pub fn inspects_output(&self) -> bool {
        self.stdout_contains.is_some() || self.stdout_absent.is_some() || self.stdout_non_empty
    }
}

/// Who runs a journey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Persona {
    /// Installed the product to use it.
    EndUser,
    /// Keeps a daemon or a mesh healthy.
    Operator,
    /// Works on code with the toolchain.
    Developer,
    /// Authors recipes, corpora, or mesh apps.
    Author,
    /// An AI agent driving the CLI as a tool surface.
    Agent,
}

/// A verb that belongs to no journey, with the reason and what to do about
/// it. The ratchet forbids this ledger growing — a new verb must either
/// join a journey or be added here deliberately.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stranded {
    /// Top-level verb, e.g. `"newsworthy"`.
    pub verb: String,
    /// Why it is stranded — what the audit found.
    pub reason: String,
    /// What should eventually happen to it.
    pub disposition: Disposition,
    /// For `disposition = "fold"`: the verb or journey it belongs under.
    #[serde(default)]
    pub fold_into: Option<String>,
}

/// How a [`JourneyStep`] binds to the declared command surface.
///
/// The middle case is the interesting one: it is how the manifest admits
/// "this journey drives a real subcommand that no `[[command]]` row declares
/// yet" without either failing the build or pretending the step is covered.
/// The static test renders every `VerbOnly` as a to-do list.
#[derive(Debug, Clone)]
pub enum StepBinding<'a> {
    /// The step's leading path equals a declared [`Command::path`].
    Exact(&'a Command),
    /// The top-level verb is tracked, but this exact subcommand path is not
    /// declared. Allowed; requires a `note` on the step.
    VerbOnly(&'a str),
    /// Neither the path nor the verb is tracked — the step drives something
    /// that does not exist. Always a hard failure.
    Unresolved,
}

impl StepBinding<'_> {
    /// The declared command, when the binding is exact.
    pub fn exact(&self) -> Option<&Command> {
        match self {
            StepBinding::Exact(c) => Some(c),
            _ => None,
        }
    }
}

/// Compared by identity of what was bound — [`Command`] itself is not `Eq`
/// (it carries flags and a smoke block), and its `path` is the primary key.
impl PartialEq for StepBinding<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (StepBinding::Exact(a), StepBinding::Exact(b)) => a.path == b.path,
            (StepBinding::VerbOnly(a), StepBinding::VerbOnly(b)) => a == b,
            (StepBinding::Unresolved, StepBinding::Unresolved) => true,
            _ => false,
        }
    }
}
impl Eq for StepBinding<'_> {}

/// The intended fate of a [`Stranded`] verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Disposition {
    /// A real journey nobody wrote down. Write the journey.
    Promote,
    /// Belongs under an existing verb or journey. See `fold_into`.
    Fold,
    /// Internal tooling; should be `hidden` and out of the public `--help`.
    Demote,
    /// An experiment behind a build gate. Leave it, revisit deliberately.
    Park,
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
    /// Every promised journey. Maps the TOML `[[journey]]` array.
    #[serde(default, rename = "journey")]
    pub journeys: Vec<Journey>,
    /// Verbs deliberately in no journey. Maps the TOML `[[stranded]]` array.
    #[serde(default, rename = "stranded")]
    pub stranded: Vec<Stranded>,
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

    /// Resolve a journey step to the [`Command`] it invokes.
    ///
    /// `step.command` wins when set. Otherwise the declared command paths
    /// are matched by **longest prefix** against the leading words of
    /// `step.run`, stopping at the first word that is an argument rather
    /// than a path element (anything starting with `-` or `{`). So
    /// `"chat inspect --corpus {corpus} \"q\""` binds to `chat inspect`,
    /// and `"mesh join {join_key}"` to `mesh join`.
    ///
    /// The three outcomes are deliberately distinct — see [`StepBinding`].
    /// Collapsing `VerbOnly` into `Exact` would let a journey drive a
    /// subcommand nobody has declared and still look fully conformant.
    pub fn resolve_step(&self, step: &JourneyStep) -> StepBinding<'_> {
        if let Some(explicit) = &step.command {
            return match self.commands.iter().find(|c| &c.path == explicit) {
                Some(cmd) => StepBinding::Exact(cmd),
                None => StepBinding::Unresolved,
            };
        }
        let words: Vec<&str> = step
            .run
            .split_whitespace()
            .take_while(|w| !w.starts_with('-') && !w.starts_with('{'))
            .collect();
        // Longest prefix first so `corpus install` beats a bare `corpus`.
        let exact = (1..=words.len()).rev().find_map(|n| {
            let candidate = words[..n].join(" ");
            self.commands.iter().find(|c| c.path == candidate)
        });
        if let Some(cmd) = exact {
            return StepBinding::Exact(cmd);
        }
        // No path matched, but the verb itself may be tracked (the manifest
        // declares `mesh create` etc. but no bare `mesh`, so a journey step
        // driving the real-but-undeclared `mesh plan` lands here). Borrow
        // the verb from the matching command's own path, not from the step,
        // so the returned binding shares one lifetime with `self`.
        let verb = words.first().copied().unwrap_or("");
        if !verb.is_empty() {
            if let Some(tracked) = self
                .commands
                .iter()
                .filter_map(|c| c.path.split_whitespace().next())
                .find(|v| *v == verb)
            {
                return StepBinding::VerbOnly(tracked);
            }
        }
        StepBinding::Unresolved
    }

    /// The top-level verb a journey step drives, independent of whether the
    /// full path resolves. Used by the strandedness ratchet.
    pub fn step_verb(step: &JourneyStep) -> &str {
        step.run.split_whitespace().next().unwrap_or("")
    }

    /// Every top-level verb reachable from at least one journey.
    pub fn verbs_in_journeys(&self) -> std::collections::BTreeSet<String> {
        self.journeys
            .iter()
            .flat_map(|j| j.steps.iter())
            .map(|s| Self::step_verb(s).to_string())
            .filter(|v| !v.is_empty())
            .collect()
    }

    /// Journeys the live harness should run, hardest-hitting first: tier
    /// ascending (tier 1 = product is broken without it), then id for a
    /// stable order. Journeys with `skip_live` are excluded.
    pub fn live_journeys(&self) -> Vec<&Journey> {
        let mut out: Vec<&Journey> = self
            .journeys
            .iter()
            .filter(|j| j.skip_live.is_none())
            .collect();
        out.sort_by(|a, b| a.tier.cmp(&b.tier).then_with(|| a.id.cmp(&b.id)));
        out
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
        assert!(!c.journeys.is_empty(), "manifest declares no journeys");
    }

    // ── journeys ────────────────────────────────────────────────────────

    /// A two-command manifest plus one journey, used by the resolution tests.
    fn journey_fixture() -> Contract {
        Contract::parse(
            r#"
[[command]]
path = "corpus install"
binary = "llm"
[[command]]
path = "corpus list"
binary = "llm"
[[command]]
path = "chat inspect"
binary = "llm"
[[command]]
path = "corpus"
binary = "llm"

[[journey]]
id = "corpus-lifecycle"
title = "Install a corpus, query it, remove it"
persona = "end-user"
tier = 1
visibility = "public"
doc = "sovereign/docs/KNOWLEDGE_BASES.md"
[[journey.step]]
run = "corpus list"
[[journey.step]]
run = "corpus install {corpus}"
mutates = true
[[journey.step]]
run = "chat inspect --corpus {corpus} \"a question\""
[journey.step.expect]
stdout_non_empty = true

[[stranded]]
verb = "newsworthy"
reason = "tracked-set summary; one help line, no documented sequence"
disposition = "demote"
"#,
        )
        .expect("parse")
    }

    #[test]
    fn journey_parses_with_steps_and_personas() {
        let c = journey_fixture();
        assert_eq!(c.journeys.len(), 1);
        let j = &c.journeys[0];
        assert_eq!(j.id, "corpus-lifecycle");
        assert_eq!(j.persona, Persona::EndUser);
        assert_eq!(j.tier, 1);
        assert_eq!(j.visibility, Visibility::Public);
        assert!(j.skip_live.is_none());
        assert_eq!(j.steps.len(), 3);
        assert!(j.steps[1].mutates);
        assert!(!j.steps[0].mutates);
        assert!(j.steps[2].expect.as_ref().unwrap().stdout_non_empty);
    }

    #[test]
    fn stranded_ledger_parses() {
        let c = journey_fixture();
        assert_eq!(c.stranded.len(), 1);
        assert_eq!(c.stranded[0].verb, "newsworthy");
        assert_eq!(c.stranded[0].disposition, Disposition::Demote);
        assert!(c.stranded[0].fold_into.is_none());
    }

    /// Build a bare step for the resolution tests.
    fn step(run: &str, command: Option<&str>) -> JourneyStep {
        JourneyStep {
            run: run.into(),
            command: command.map(String::from),
            mutates: false,
            expect: None,
            skip_live: None,
            settle_secs: None,
            note: None,
        }
    }

    #[test]
    fn resolve_step_takes_the_longest_matching_prefix() {
        let c = journey_fixture();
        // "corpus install {corpus}" must bind to `corpus install`, not the
        // shorter `corpus` row that also exists.
        let s = &c.journeys[0].steps[1];
        assert_eq!(c.resolve_step(s).exact().unwrap().path, "corpus install");
    }

    #[test]
    fn resolve_step_stops_at_flags_and_placeholders() {
        let c = journey_fixture();
        // `--corpus`, `{corpus}` and the quoted question are arguments, not
        // path elements; resolution must stop before them.
        let s = &c.journeys[0].steps[2];
        assert_eq!(c.resolve_step(s).exact().unwrap().path, "chat inspect");
    }

    #[test]
    fn resolve_step_honors_an_explicit_command_override() {
        let c = journey_fixture();
        let s = step("corpus install-but-spelled-oddly", Some("corpus install"));
        assert_eq!(c.resolve_step(&s).exact().unwrap().path, "corpus install");
    }

    #[test]
    fn resolve_step_is_unresolved_for_a_command_that_does_not_exist() {
        // The drift this whole layer exists to catch: a doc teaching a verb
        // that was never implemented (ATOS.md prescribes `read-notes`,
        // which exits 1 exactly like a made-up verb).
        let c = journey_fixture();
        let s = step("read-notes --kind decision", None);
        assert_eq!(c.resolve_step(&s), StepBinding::Unresolved);
    }

    #[test]
    fn resolve_step_is_unresolved_when_an_override_names_a_missing_command() {
        let c = journey_fixture();
        let s = step("corpus list", Some("corpus nope"));
        assert_eq!(c.resolve_step(&s), StepBinding::Unresolved);
    }

    #[test]
    fn resolve_step_reports_verb_only_for_an_undeclared_subcommand() {
        // `chat` is tracked (via `chat inspect`), but no row declares
        // `chat purge` — that is a manifest gap, not a nonexistent verb,
        // and the two must not look alike.
        let c = journey_fixture();
        let s = step("chat purge {id}", None);
        assert_eq!(c.resolve_step(&s), StepBinding::VerbOnly("chat"));
        assert!(c.resolve_step(&s).exact().is_none());
    }

    #[test]
    fn verbs_in_journeys_collects_top_level_verbs() {
        let c = journey_fixture();
        let verbs = c.verbs_in_journeys();
        assert!(verbs.contains("corpus"));
        assert!(verbs.contains("chat"));
        assert_eq!(verbs.len(), 2);
    }

    #[test]
    fn live_journeys_sort_by_tier_and_drop_skipped() {
        let c = Contract::parse(
            r#"
[[journey]]
id = "b-specialist"
title = "t"
persona = "developer"
tier = 5
[[journey]]
id = "a-critical"
title = "t"
persona = "end-user"
tier = 1
[[journey]]
id = "c-multimachine"
title = "t"
persona = "operator"
tier = 2
skip_live = "needs a second machine"
"#,
        )
        .expect("parse");
        let live: Vec<&str> = c.live_journeys().iter().map(|j| j.id.as_str()).collect();
        assert_eq!(live, vec!["a-critical", "b-specialist"]);
    }

    #[test]
    fn expect_knows_whether_it_inspects_output() {
        let exit_only = Expect {
            exit: Some(0),
            ..Default::default()
        };
        assert!(!exit_only.inspects_output());
        let real = Expect {
            stdout_contains: Some("my-corpus".into()),
            ..Default::default()
        };
        assert!(real.inspects_output());
        let absence = Expect {
            stdout_absent: Some("my-corpus".into()),
            ..Default::default()
        };
        assert!(absence.inspects_output());
    }
}
