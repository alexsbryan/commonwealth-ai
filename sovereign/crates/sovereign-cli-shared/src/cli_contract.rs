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
    /// The [`Experience::id`] this journey serves. REQUIRED: a journey that
    /// serves no named promise is a command enumeration, which is the drift
    /// this axis exists to stop. Several journeys may serve one experience —
    /// that is the point, since a promise usually needs more than one
    /// sequence to be proven (build it, then ask it a question).
    pub experience: String,
    /// What this journey needs from the lane running it. Empty means any
    /// lane can run it. See [`Need`].
    #[serde(default)]
    pub needs: Vec<Need>,
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

impl Journey {
    /// Does any step of this journey exercise `capability`, and assert
    /// something about the output when it does?
    ///
    /// The output condition is not decoration. Every code-intelligence tool
    /// in this repo exits **0** when it finds nothing — `symbols` on an
    /// unknown name prints "No symbol named X found in any installed code
    /// corpus" and exits 0; so do `callers`, `callees` and `capability_map`
    /// (measured 2026-07-29). A step that names a capability and checks only
    /// its exit code is therefore satisfied by the tool answering NOTHING,
    /// which is precisely the vacuous-green shape the journey layer exists
    /// to kill. Naming a capability without asserting output does not count
    /// as exercising it.
    ///
    /// A READ proves itself inline; a MUTATION is proven downstream. `corpus
    /// install` returns before its ingest lands and cannot assert its own
    /// effect — the proof is the later `corpus status` that finds the corpus,
    /// which is what makes a journey a sequence rather than a bag. So a
    /// mutating step counts as exercised when any LATER step in the same
    /// journey asserts output. A non-mutating step gets no such credit:
    /// nothing downstream can prove that `symbols` answered.
    ///
    /// Matching: a single-word capability must appear as a whole ARGV token
    /// (splitting on whitespace and `=`), so `notes` does not satisfy
    /// `note`, and `--name={symbol}` yields `--name` and `{symbol}`. A
    /// capability containing a space is matched as a substring, which is how
    /// a multi-word invocation like `corpus snapshot publish` is declared.
    pub fn exercises(&self, capability: &str) -> bool {
        let asserts = |s: &JourneyStep| s.expect.as_ref().is_some_and(|e| e.inspects_output());
        self.steps.iter().enumerate().any(|(i, s)| {
            if !step_names(s, capability) {
                return false;
            }
            asserts(s) || (s.mutates && self.steps[i + 1..].iter().any(asserts))
        })
    }

    /// Does any lane actually RUN this journey? `skip_live` means no: not the
    /// mutating sandbox lane, not the read-only capability lane, not CI.
    ///
    /// Load-bearing for every honest count in this file. 14 of the 32 journeys
    /// carry a journey-level `skip_live` (needs a second machine, a paid GPU
    /// pod, a multi-minute benchmark), and their 62 steps are NEVER EXECUTED
    /// BY ANYTHING. Whatever those steps declare, no lane can catch a
    /// regression in them — so counting them alongside the steps that do run
    /// mixes two different defects and lets the cheap repair (sprinkle
    /// `exit = 0` on a step nobody runs) satisfy a ratchet aimed at the
    /// expensive one.
    pub fn runs_live(&self) -> bool {
        self.skip_live.is_none()
    }

    /// Steps of this journey a lane will actually execute, with their indices.
    /// Empty when the journey itself is `skip_live`, whatever its steps say.
    pub fn live_steps(&self) -> impl Iterator<Item = (usize, &JourneyStep)> {
        let runs = self.runs_live();
        self.steps
            .iter()
            .enumerate()
            .filter(move |(_, s)| runs && s.skip_live.is_none())
    }

    /// Steps that name `capability` at all, asserted or not. The ratchet's
    /// error message uses this to distinguish "nobody drives it" from
    /// "somebody drives it and checks only the exit code" — two different
    /// repairs, and the second is the one that reads as covered.
    pub fn mentions(&self, capability: &str) -> bool {
        self.steps.iter().any(|s| step_names(s, capability))
    }
}

impl JourneyStep {
    /// What a lane could catch if this step went wrong. See [`Evidence`].
    pub fn evidence(&self) -> Evidence {
        match &self.expect {
            Some(e) if e.inspects_output() => Evidence::Output,
            Some(e) if e.exit.is_some() => Evidence::ExitOnly,
            _ => Evidence::None,
        }
    }

    /// A step the live runner CANNOT FAIL: no expected exit code, no output
    /// assertion, nothing. It is invoked, and whatever happens is reported as
    /// a tick.
    ///
    /// Not hypothetical, and not harmless. `enrich-atlas` declared its first
    /// two steps this way; on 2026-07-29 `enrich init --from-corpus` wrote no
    /// enrichment directory AT ALL and `enrich build --full` followed it, and
    /// both reported ✓ — the journey then failed on step [2] (`enrich status`,
    /// which does assert), pointing the reader two steps past the actual
    /// breakage. An unfalsifiable step does not merely fail to prove its own
    /// command; it MISATTRIBUTES the failure of the sequence.
    ///
    /// Equivalent to `self.evidence() == Evidence::None`, and kept as a named
    /// predicate because that is the concept the ratchets are about — but note
    /// that "unfalsifiable" is necessary, not sufficient: a step in a
    /// `skip_live` journey cannot fail either, whatever it declares. Ask
    /// [`Journey::live_steps`] which steps a lane actually runs before drawing
    /// a conclusion from this.
    pub fn is_unfalsifiable(&self) -> bool {
        self.evidence() == Evidence::None
    }
}

/// Whether a step's `run` names `capability`. See [`Journey::exercises`] for
/// the rule and why it is token-wise rather than a bare `contains`.
fn step_names(step: &JourneyStep, capability: &str) -> bool {
    if capability.contains(char::is_whitespace) {
        return step.run.contains(capability);
    }
    step.run
        .split(|c: char| c.is_whitespace() || c == '=')
        .any(|tok| tok == capability)
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

    /// Whether this block asserts ANYTHING — output or even an exit code.
    pub fn asserts_anything(&self) -> bool {
        self.exit.is_some() || self.inspects_output()
    }
}

/// How much a lane could catch if a step's command broke — the axis that
/// separates a test from a demonstration.
///
/// Three classes, not two, because the middle one is where false confidence
/// actually lives. `Output` catches a wrong answer. `None` catches nothing at
/// all. `ExitOnly` catches a crash and NOTHING ELSE — and in this repo that is
/// much weaker than it sounds: every code-intelligence tool exits 0 when it
/// finds nothing, `sovereign doctor` exits 0 on a sick system by design, and
/// `code search` printed placeholder stub text and exited 0 for a whole
/// release. An `exit = 0` gate over that surface is satisfied by an index that
/// has been wiped.
///
/// So `ExitOnly` is the class to watch, not to celebrate: it is the cheapest
/// way to satisfy a "declare something" ratchet without adding evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// No `expect` block, or one that asserts nothing. Cannot fail.
    None,
    /// An exit code and nothing else.
    ExitOnly,
    /// At least one assertion about what the command actually printed.
    Output,
}

/// One row of [`Contract::assertion_census`] — the numbers behind "how much of
/// this manifest can actually fail?", split by whether a lane runs it.
#[derive(Debug, Clone, Default)]
pub struct EvidenceCount {
    /// Steps that cannot fail: no assertion at all.
    pub none: usize,
    /// Steps asserting only an exit code.
    pub exit_only: usize,
    /// Steps asserting something about output.
    pub output: usize,
}

impl EvidenceCount {
    /// Total steps in this class.
    pub fn total(&self) -> usize {
        self.none + self.exit_only + self.output
    }

    fn add(&mut self, e: Evidence) {
        match e {
            Evidence::None => self.none += 1,
            Evidence::ExitOnly => self.exit_only += 1,
            Evidence::Output => self.output += 1,
        }
    }
}

/// The manifest's evidence, partitioned by whether any lane executes it.
///
/// The number this exists to stop anyone quoting is "133 steps". A step in a
/// `skip_live` journey is a written intention; a step with no `expect` block is
/// an invocation. Neither is a test, and both used to be counted in the same
/// total as the 42 steps that actually assert an answer.
#[derive(Debug, Clone, Default)]
pub struct AssertionCensus {
    /// Steps some lane runs.
    pub live: EvidenceCount,
    /// Steps NO lane runs — journey-level `skip_live`, or a step-level one.
    pub never_run: EvidenceCount,
    /// `journey[idx] run` for every LIVE step that asserts nothing. The
    /// to-do list, in the order a reader would fix it.
    pub live_unfalsifiable: Vec<String>,
    /// Live journeys carrying no output assertion anywhere — a sequence that
    /// runs end to end and can only ever prove that the binary starts.
    pub live_journeys_without_output: Vec<String>,
    /// Journeys no lane runs at all, with the manifest's own reason.
    pub never_run_journeys: Vec<(String, String)>,
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

// ── Experiences ─────────────────────────────────────────────────────────

/// A documented promise the product makes, and the capabilities that promise
/// is made of. Primary key is [`Experience::id`]; journeys point at it with
/// [`Journey::experience`].
///
/// WHY THIS AXIS EXISTS. The ratchets one level down are all verb-driven —
/// "every public verb belongs to a journey" — so the manifest grew to model
/// COMMANDS, and journeys became vehicles for verb coverage. `code-intel-
/// lifecycle` is the tell: six steps (`project init|list|status|refresh|
/// serve|stop`) that prove the index BUILDS and never ask it a question. It
/// is named for a capability and tests only plumbing.
///
/// Measured 2026-07-29: of the 23 tools `.claude/CLAUDE.md` mandates for
/// every agent session, 18 were named by no journey step at all — including
/// `symbols`, `callers`, `callees`, `blast` and `code_search`, the five the
/// instructions say to use INSTEAD of reading files. Nothing was failing;
/// nothing was watching either. That gap was only findable by
/// cross-referencing the instructions against the manifest by hand.
///
/// [`Experience::capabilities`] is the fix: name what the promise is made
/// of, and let the ratchet find the hole. [`Experience::gap`] is the other
/// half — an experience with no journey yet is DECLARED, so "code-intel chat
/// has no journey" is a visible, tracked, shrinking quantity rather than
/// something a future audit has to rediscover.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Experience {
    /// Stable kebab-case id, e.g. `"code-intelligence"`.
    pub id: String,
    /// The promise in the user's words: what they get, not what runs.
    pub title: String,
    /// Where the promise is made, as a repo-relative path with an optional
    /// `#anchor` or `:line`. REQUIRED — unlike [`Journey::doc`], because an
    /// undocumented experience is not a promise, it is an intention.
    pub doc: String,
    /// The named capabilities this promise is made of — tool ids, or the
    /// exact words typed. Each must be exercised by a step of some journey
    /// serving this experience, in a step that asserts something about
    /// OUTPUT. See [`Journey::exercises`] for the matching rule.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Set when NO journey serves this experience yet, saying why. The
    /// ratchet caps how many of these may exist and the cap only ever
    /// shrinks, so a declared gap is a debt with a name.
    #[serde(default)]
    pub gap: Option<String>,
    /// Free-text note rendered in the experience map.
    #[serde(default)]
    pub note: Option<String>,
}

/// Something a journey needs from the LANE that runs it, which not every
/// lane can supply.
///
/// Distinct from `skip_live` (a property of the journey: "needs a second
/// machine, never run it automatically") and from the runner's `--exclude`
/// (a property of one invocation). This is the property that was previously
/// hardcoded in `cli-journey-sandbox.sh` as a `SANDBOX_EXCLUDES` array: two
/// journey ids and one shared prose reason, invisible from the manifest.
/// That is the same class of defect the experience axis fixes — a fact about
/// a journey living somewhere nobody cross-references.
///
/// Declaring it here lets the two lanes PARTITION the manifest from one
/// source: the sandbox lane says `--lacks` for what a throwaway HOME cannot
/// have, and the read-only operator lane runs exactly what the sandbox
/// skipped. Nothing is dropped by both, and the reason printed is the
/// manifest's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Need {
    /// The operator's real `$HOME` — Claude Code transcripts under
    /// `~/.claude/projects`, an accumulated notes db, a drift report on
    /// disk. A throwaway sandbox HOME has none of it by construction, so a
    /// sandbox run of these journeys can only ever report a false failure.
    OperatorHome,
    /// A live code index and SCIP call graph over a real repository. Built
    /// by `project init` in minutes with `rust-analyzer` present — too
    /// expensive and too fragile to build inside a per-run sandbox, and the
    /// operator's daemon already has one.
    IndexedRepo,
}

impl Need {
    /// The token used in the manifest and on the runner's `--lacks` flag.
    pub fn as_str(&self) -> &'static str {
        match self {
            Need::OperatorHome => "operator-home",
            Need::IndexedRepo => "indexed-repo",
        }
    }

    /// What a lane is admitting when it says it lacks this. Printed by the
    /// runner, so a skipped journey explains itself.
    pub fn why(&self) -> &'static str {
        match self {
            Need::OperatorHome => {
                "needs the operator's real HOME (Claude transcripts, notes, drift report)"
            }
            Need::IndexedRepo => "needs a live code index + SCIP graph over a real repo",
        }
    }
}

/// A shared prerequisite journeys stand on — a STATE of the machine a
/// journey assumes rather than builds: a running daemon, a joined mesh, an
/// installed corpus, an indexed repo. Maps the TOML `[[dependency]]` array.
///
/// This is the docs-side twin of [`Need`]. `Need` is the LANE vocabulary
/// (what a test environment cannot supply); a `Dependency` is the USER
/// vocabulary: what a person must already have before a journey doc applies
/// to them, and the ONE canonical doc that gets them there. Before this
/// axis existed (measured 2026-07-31) there was no canonical doc for any of
/// them, so every journey doc re-taught its prerequisites inline — mesh
/// create/join was re-explained in 15 docs, daemon setup in 10, corpus
/// install in 13, repo indexing in 8. A journey doc should open with links
/// to its dependencies' docs, not a fresh retelling.
///
/// Every [`Need`] variant must have a `Dependency` row with
/// `id == Need::as_str()`, so a lane exclusion always has a doc explaining
/// the state the excluded journeys assumed. Enforced in
/// `cli_contract_journeys`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    /// Stable kebab-case id, e.g. `"joined-mesh"`. Named for the STATE the
    /// user has (joined-mesh), not the action that got them there.
    pub id: String,
    /// The prerequisite in the user's words, e.g. "A joined mesh".
    pub title: String,
    /// The one canonical doc that gets a user this state — the module that
    /// journey docs link instead of re-explaining. Repo-relative path,
    /// optional `#anchor`.
    pub doc: String,
    /// The read-only command that proves the state exists, per the
    /// capability rule (a read proves itself inline): `mesh status`,
    /// `doctor`, `corpus status`. Rendered in the experience map so "do I
    /// have this?" is always one command away.
    pub verify: String,
    /// Free-text note rendered alongside.
    #[serde(default)]
    pub note: Option<String>,
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
    /// Every promise the product makes. Maps the TOML `[[experience]]`
    /// array. Journeys serve these; see [`Experience`].
    #[serde(default, rename = "experience")]
    pub experiences: Vec<Experience>,
    /// Every promised journey. Maps the TOML `[[journey]]` array.
    #[serde(default, rename = "journey")]
    pub journeys: Vec<Journey>,
    /// Shared prerequisites journeys stand on. Maps the TOML
    /// `[[dependency]]` array. See [`Dependency`].
    #[serde(default, rename = "dependency")]
    pub dependencies: Vec<Dependency>,
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

    /// The journeys serving one experience, in manifest order.
    pub fn journeys_for(&self, experience_id: &str) -> Vec<&Journey> {
        self.journeys
            .iter()
            .filter(|j| j.experience == experience_id)
            .collect()
    }

    /// A declared experience by id.
    pub fn experience(&self, id: &str) -> Option<&Experience> {
        self.experiences.iter().find(|e| e.id == id)
    }

    /// Capabilities of `experience` that NO serving journey exercises with an
    /// output assertion. Empty is the healthy state; a non-empty result is
    /// the hole. Each entry pairs the capability with whether some step at
    /// least NAMES it — `(capability, mentioned_but_unasserted)`.
    pub fn unproven_capabilities<'a>(
        &self,
        experience: &'a Experience,
    ) -> Vec<(&'a str, bool)> {
        let serving = self.journeys_for(&experience.id);
        experience
            .capabilities
            .iter()
            .filter(|cap| !serving.iter().any(|j| j.exercises(cap)))
            .map(|cap| {
                let mentioned = serving.iter().any(|j| j.mentions(cap));
                (cap.as_str(), mentioned)
            })
            .collect()
    }

    /// Count what this manifest can actually catch, split by whether a lane
    /// runs it. One definition, shared by the ratchets, the `svrn contract`
    /// report and the docs — so the number in the doc cannot drift from the
    /// number the gate uses.
    ///
    /// Walked in manifest order (not tier order): the output is read as a
    /// to-do list against the file, so it should point at the file's own
    /// sequence.
    pub fn assertion_census(&self) -> AssertionCensus {
        let mut c = AssertionCensus::default();
        for j in &self.journeys {
            if let Some(why) = &j.skip_live {
                c.never_run_journeys
                    .push((j.id.clone(), why.split_whitespace().collect::<Vec<_>>().join(" ")));
            }
            let mut live_output = 0usize;
            let mut live_any = 0usize;
            for (i, s) in j.steps.iter().enumerate() {
                let ev = s.evidence();
                if j.runs_live() && s.skip_live.is_none() {
                    c.live.add(ev);
                    live_any += 1;
                    if ev == Evidence::Output {
                        live_output += 1;
                    }
                    if ev == Evidence::None {
                        c.live_unfalsifiable.push(format!("{}[{}] {}", j.id, i, s.run));
                    }
                } else {
                    c.never_run.add(ev);
                }
            }
            if live_any > 0 && live_output == 0 {
                c.live_journeys_without_output.push(j.id.clone());
            }
        }
        c
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

[[experience]]
id = "knowledge-corpora"
title = "Ask questions of a body of documents and get cited answers"
doc = "sovereign/docs/KNOWLEDGE_BASES.md"
capabilities = ["corpus list", "corpus install", "chat inspect"]

[[experience]]
id = "unserved-thing"
title = "A promise with no journey yet"
doc = "sovereign/docs/KNOWLEDGE_BASES.md"
gap = "declared so the fixture covers the gap register"

[[journey]]
id = "corpus-lifecycle"
title = "Install a corpus, query it, remove it"
persona = "end-user"
experience = "knowledge-corpora"
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

    #[test]
    fn experiences_parse_and_journeys_point_at_them() {
        let c = journey_fixture();
        assert_eq!(c.experiences.len(), 2);
        assert_eq!(c.journeys[0].experience, "knowledge-corpora");
        assert_eq!(c.journeys_for("knowledge-corpora").len(), 1);
        assert!(c.journeys_for("unserved-thing").is_empty());
        assert!(c.experience("knowledge-corpora").is_some());
        assert!(c.experience("no-such-experience").is_none());
        // `needs` defaults to empty — any lane may run this journey.
        assert!(c.journeys[0].needs.is_empty());
    }

    #[test]
    fn a_read_proves_itself_inline_and_a_mutation_is_proven_downstream() {
        // The fixture is [corpus list (no expect), corpus install (mutates,
        // no expect), chat inspect (stdout_non_empty)].
        //
        // `corpus list` is a READ that asserts nothing, so it NAMES the
        // capability without proving it — and that is the whole point: every
        // code-intel tool in this repo exits 0 when it finds nothing, so an
        // exit-code-only read is satisfied by a tool that answered nothing.
        //
        // `corpus install` asserts nothing either, but it MUTATES and a later
        // step asserts output, which is exactly how a sequence proves an
        // effect that the command itself returns before finishing.
        let c = journey_fixture();
        let e = c.experience("knowledge-corpora").expect("declared");
        assert_eq!(
            c.unproven_capabilities(e),
            vec![("corpus list", true)],
            "only the unasserted READ is unproven, and it is mentioned"
        );
        let j = &c.journeys[0];
        assert!(j.mentions("corpus list"));
        assert!(!j.exercises("corpus list"), "an unasserted read proves nothing");
        assert!(j.exercises("corpus install"), "proven by the later assertion");
        assert!(j.exercises("chat inspect"), "proven inline");
    }

    #[test]
    fn a_trailing_mutation_with_nothing_after_it_is_unproven() {
        // The downstream credit is strictly LATER steps. A journey that ends
        // on a mutation has nobody left to prove it — `mesh leave` is the
        // real instance: it reverses the federation and no step looks at the
        // result.
        let c = Contract::parse(
            r#"
[[command]]
path = "corpus install"
binary = "llm"
[[command]]
path = "corpus remove"
binary = "llm"
[[experience]]
id = "e"
title = "t"
doc = "sovereign/docs/KNOWLEDGE_BASES.md"
capabilities = ["corpus remove"]
[[journey]]
id = "j"
title = "t"
persona = "end-user"
experience = "e"
tier = 1
[[journey.step]]
run = "corpus install {corpus}"
mutates = true
[journey.step.expect]
stdout_non_empty = true
[[journey.step]]
run = "corpus remove {corpus}"
mutates = true
"#,
        )
        .expect("parse");
        let e = c.experience("e").expect("declared");
        assert_eq!(
            c.unproven_capabilities(e),
            vec![("corpus remove", true)],
            "an earlier assertion cannot prove a later mutation"
        );
    }

    #[test]
    fn capability_matching_is_token_wise_not_substring() {
        // `notes` must not satisfy a `note` capability, and a flag's value
        // must be reachable: `--name={symbol}` yields `--name` and
        // `{symbol}`. Without this, `note` (write) would read as covered by
        // any journey that merely READS notes.
        let j = Journey {
            id: "x".into(),
            title: "x".into(),
            persona: Persona::Agent,
            experience: "e".into(),
            needs: vec![],
            tier: 4,
            visibility: Visibility::Internal,
            doc: None,
            skip_live: None,
            steps: vec![JourneyStep {
                run: "tools call symbols --name={symbol}".into(),
                command: None,
                mutates: false,
                expect: Some(Expect {
                    stdout_non_empty: true,
                    ..Default::default()
                }),
                skip_live: None,
                settle_secs: None,
                note: None,
            }],
        };
        assert!(j.exercises("symbols"));
        assert!(j.exercises("{symbol}"));
        assert!(!j.exercises("symbol"), "no substring match on a token");
        assert!(!j.exercises("symbolsx"));
        assert!(j.exercises("tools call symbols"), "multi-word is substring");
    }

    #[test]
    fn needs_parse_from_kebab_case() {
        let c = Contract::parse(
            r#"
[[command]]
path = "tools call"
binary = "dev"
[[experience]]
id = "e"
title = "t"
doc = "sovereign/docs/CODE_INTELLIGENCE.md"
[[journey]]
id = "j"
title = "t"
persona = "agent"
experience = "e"
tier = 3
needs = ["indexed-repo", "operator-home"]
[[journey.step]]
run = "tools call symbols --name=X"
"#,
        )
        .expect("parse");
        assert_eq!(
            c.journeys[0].needs,
            vec![Need::IndexedRepo, Need::OperatorHome]
        );
        assert_eq!(Need::IndexedRepo.as_str(), "indexed-repo");
        assert!(Need::OperatorHome.why().contains("operator's real HOME"));
    }

    #[test]
    fn dependencies_parse() {
        let c = Contract::parse(
            r#"
[[dependency]]
id = "joined-mesh"
title = "A joined mesh"
doc = "docs/JOIN_A_MESH.md"
verify = "mesh status"
"#,
        )
        .expect("parse");
        assert_eq!(c.dependencies.len(), 1);
        assert_eq!(c.dependencies[0].id, "joined-mesh");
        assert_eq!(c.dependencies[0].verify, "mesh status");
        assert!(c.dependencies[0].note.is_none());
    }

    #[test]
    fn needs_are_delimiter_safe() {
        // `__journey-plan` emits needs as `token:why` pairs joined by `;`, on a
        // TAB-separated row, and the shell runner splits on exactly those. A
        // reason containing the separator silently truncates itself — which is
        // not hypothetical: the first version joined pairs with `,` and
        // `operator-home`'s reason ("Claude transcripts, notes, drift report")
        // printed as "(Claude transcripts" in the live lane.
        //
        // The token must also stay free of `:`, since the pair splits on the
        // first one.
        for n in [Need::OperatorHome, Need::IndexedRepo] {
            let why = n.why();
            assert!(!why.contains(';'), "{:?}: reason contains the pair separator `;`: {why}", n);
            assert!(!why.contains('\t'), "{:?}: reason contains a TAB: {why}", n);
            assert!(!why.is_empty(), "{:?}: every need must explain itself", n);
            let tok = n.as_str();
            assert!(!tok.contains(':'), "{:?}: token contains `:`", n);
            assert!(!tok.contains(';'), "{:?}: token contains `;`", n);
            assert!(
                tok.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{:?}: token `{tok}` is not kebab-case",
                n
            );
        }
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
experience = "e"
tier = 5
[[journey]]
id = "a-critical"
title = "t"
persona = "end-user"
experience = "e"
tier = 1
[[journey]]
id = "c-multimachine"
title = "t"
persona = "operator"
experience = "e"
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
