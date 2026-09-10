// SPDX-License-Identifier: AGPL-3.0-or-later
//! The job vocabulary — what a work plane submits, offers, and executes.
//!
//! # Why this is layer 0
//!
//! A job crosses the wire between a submitter and a donor that share no code:
//! the donor may be a third-party peer running its own build. So the nouns
//! belong in the same leaf as the rest of the wire vocabulary, beside
//! [`CapabilityHint`](crate::CapabilityHint) and
//! [`TenantId`](crate::TenantId), and not in the crate that happens to
//! dispatch first.
//!
//! # What deliberately is NOT here
//!
//! **The codec, the fold, and the predicate.** `WorkAct`, `WorkProjection`,
//! `may_take` and the `JobExecutor` trait live in `commonwealth-work` — they
//! need `commonwealth-rail-core`'s `Payload` and `Admission`, which a leaf
//! this crate's consumers link unconditionally must not drag in. This module
//! is only the vocabulary those pieces fold over: no dispatch, no lease, no
//! clock, no I/O.
//!
//! **The hash.** See [`JobUnit::unit_hash`] — it is a field here and a
//! computation there, for the same reason.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use kernel_types::quality::{Precondition, VerdictSource};

use crate::tenant::TenantId;
use crate::tool::{Idempotency, ToolExample};

// -----------------------------------------------------------------
// JobKind
// -----------------------------------------------------------------

/// The kind of work a unit is, and which revision of that kind's contract it
/// speaks — spelled `id:vN` on the wire.
///
/// # Why the version is part of the identity
///
/// A donor advertises the kinds it can run and a submitter names one. If the
/// two disagree about what `ingest` means, the job runs and returns a wrong
/// answer rather than refusing — so the contract revision has to be
/// comparable, which means it has to be in the name. `manifest::features`
/// already spells exactly this (`INGEST_V1 = "ingest:v1"`,
/// `INGEST_RECIPE_TEST = "ingest:recipe_test"`), so this type reads the
/// spelling the crate already publishes rather than minting a second one.
///
/// # What is valid, and why
///
/// The tree had **no versioned-id type** when this was written: four call
/// sites `split_once('@')` a `name@version` fragment and every one of them
/// discards the version (`corpus-engine/xtask/src/lint_gate.rs:208`,
/// `sovereign-cli-dev/src/refactor_cmd/discover.rs:414`,
/// `sovereign-work-atlas/src/repo_id.rs:187`,
/// `commonwealth-transport/examples/tunnel_bench.rs:238`). This is the first
/// type that keeps it, so `@` is refused by name rather than quietly reparsed:
/// a habit that parses-and-discards is exactly how a version skew becomes
/// invisible.
///
/// Checks run in this order, so a value that violates several is reported by
/// the first:
///
/// 1. The input is trimmed. Empty or whitespace-only →
///    [`Empty`](InvalidJobKind::Empty).
/// 2. Internal whitespace → [`Whitespace`](InvalidJobKind::Whitespace). The
///    kind is compared verbatim against an offer's advertised list; whitespace
///    makes two visually identical kinds unequal.
/// 3. `@` anywhere → [`AtVersionSpelling`](InvalidJobKind::AtVersionSpelling).
///    Named separately from "no version" so the error tells the author the
///    spelling is wrong rather than that the version is missing.
/// 4. No [`VERSION_SEPARATOR`](Self::VERSION_SEPARATOR) →
///    [`MissingVersion`](InvalidJobKind::MissingVersion). An unversioned kind
///    is the skew this type exists to make impossible.
/// 5. Empty id component → [`EmptyId`](InvalidJobKind::EmptyId).
/// 6. Last segment not `v`-prefixed →
///    [`VersionNotPrefixed`](InvalidJobKind::VersionNotPrefixed). The split is
///    on the LAST separator, because ids in this crate carry their own
///    (`"constraint:allowlist:url"`), so the id half may contain `:` and the
///    version half may not.
/// 7. Non-digits after the `v` →
///    [`VersionNotNumeric`](InvalidJobKind::VersionNotNumeric).
/// 8. A leading zero (`v01`) →
///    [`VersionNotCanonical`](InvalidJobKind::VersionNotCanonical). `v01` and
///    `v1` would parse to the same number and render as one string — two wire
///    spellings of one identity, which breaks the verbatim comparison rule 2
///    exists for.
/// 9. A version past `u32` →
///    [`VersionOutOfRange`](InvalidJobKind::VersionOutOfRange).
///
/// # Construction
///
/// [`JobKind::parse`] for anything off the wire or out of config;
/// [`JobKind::new`] when the two halves are already separate. There is
/// deliberately **no `Default`** — a kind that materializes on its own is the
/// silent substitution an identity type must not do (ARCH §18.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobKind {
    id: String,
    version: u32,
}

impl JobKind {
    /// The separator between id and version. `:`, the spelling
    /// `manifest::features` already uses.
    pub const VERSION_SEPARATOR: char = ':';

    /// The prefix on the version component: `v`, as in `ingest:v1`. Present so
    /// a trailing numeric id segment (`shard:3`) is not silently read as a
    /// version.
    pub const VERSION_PREFIX: &'static str = "v";

    /// The version spelling this type refuses. `name@1` is what the four
    /// parse-and-discard helpers in this tree accept; see [`JobKind`] for why
    /// it is an error here and not an alias.
    pub const REFUSED_VERSION_SEPARATOR: char = '@';

    /// Build a kind from an already-split id and version.
    ///
    /// The id is held to rules 1-3 and 5 on [`JobKind`]; the version needs no
    /// checking because it arrives already typed.
    pub fn new(id: impl AsRef<str>, version: u32) -> Result<Self, InvalidJobKind> {
        let id = id.as_ref().trim();
        if id.is_empty() {
            return Err(InvalidJobKind::Empty);
        }
        if id.chars().any(|c| c.is_whitespace()) {
            return Err(InvalidJobKind::Whitespace);
        }
        if id.contains(Self::REFUSED_VERSION_SEPARATOR) {
            return Err(InvalidJobKind::AtVersionSpelling);
        }
        Ok(Self {
            id: id.to_string(),
            version,
        })
    }

    /// Parse a raw `id:vN` kind — from an offer, a submission, or config.
    ///
    /// Leading and trailing whitespace is trimmed; everything else is held to
    /// the rules on [`JobKind`].
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, InvalidJobKind> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() {
            return Err(InvalidJobKind::Empty);
        }
        if raw.chars().any(|c| c.is_whitespace()) {
            return Err(InvalidJobKind::Whitespace);
        }
        if raw.contains(Self::REFUSED_VERSION_SEPARATOR) {
            return Err(InvalidJobKind::AtVersionSpelling);
        }
        // LAST separator: an id may carry its own (`constraint:allowlist:url`),
        // the version may not.
        let (id, version) = raw
            .rsplit_once(Self::VERSION_SEPARATOR)
            .ok_or(InvalidJobKind::MissingVersion)?;
        if id.is_empty() {
            return Err(InvalidJobKind::EmptyId);
        }
        let digits = version
            .strip_prefix(Self::VERSION_PREFIX)
            .ok_or(InvalidJobKind::VersionNotPrefixed)?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(InvalidJobKind::VersionNotNumeric);
        }
        if digits.len() > 1 && digits.starts_with('0') {
            return Err(InvalidJobKind::VersionNotCanonical);
        }
        let version = digits
            .parse::<u32>()
            .map_err(|_| InvalidJobKind::VersionOutOfRange)?;
        Ok(Self {
            id: id.to_string(),
            version,
        })
    }

    /// The id half — what kind of work this is, without its revision.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The revision of that kind's contract.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// True iff `other` is the same kind at a different revision — the shape
    /// of a version skew, as opposed to a kind the donor simply does not run.
    /// The two refusals are different sentences and this is the one place the
    /// difference is decided (ARCH §10.6).
    pub fn is_skew_of(&self, other: &JobKind) -> bool {
        self.id == other.id && self.version != other.version
    }
}

impl std::fmt::Display for JobKind {
    /// The wire form, and the only one: `id:vN`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}{}{}",
            self.id,
            Self::VERSION_SEPARATOR,
            Self::VERSION_PREFIX,
            self.version
        )
    }
}

/// Reasons [`JobKind`] construction can fail. One variant per rejection rule;
/// the rule each one enforces is on [`JobKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidJobKind {
    /// The input was empty or whitespace-only.
    Empty,
    /// The input contained internal whitespace, so two visually identical
    /// kinds would not compare equal against an offer's list.
    Whitespace,
    /// The version was spelled `id@N`. This vocabulary spells it `id:vN`; see
    /// [`JobKind`] for the four parse-and-discard sites that make this worth
    /// its own variant.
    AtVersionSpelling,
    /// The input carried no version at all. An unversioned kind cannot be
    /// checked for skew, which is the whole point of the type.
    MissingVersion,
    /// The input was all version and no id (`":v1"`).
    EmptyId,
    /// The last segment did not start with `v`.
    VersionNotPrefixed,
    /// The characters after the `v` were not all digits.
    VersionNotNumeric,
    /// The version carried a leading zero, so two spellings would name one
    /// identity.
    VersionNotCanonical,
    /// The version did not fit in a `u32`.
    VersionOutOfRange,
}

impl std::fmt::Display for InvalidJobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::Empty => "job kind is empty",
            Self::Whitespace => "job kind contains whitespace",
            Self::AtVersionSpelling => "job kind spells its version with '@'; use 'id:vN'",
            Self::MissingVersion => "job kind carries no ':vN' version",
            Self::EmptyId => "job kind has no id before its version",
            Self::VersionNotPrefixed => "job kind version is not 'v'-prefixed",
            Self::VersionNotNumeric => "job kind version is not a number",
            Self::VersionNotCanonical => "job kind version has a leading zero",
            Self::VersionOutOfRange => "job kind version does not fit in a u32",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for InvalidJobKind {}

impl Serialize for JobKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for JobKind {
    /// Deserialization goes through [`JobKind::parse`]: a permissive
    /// `Deserialize` would be a hole straight past the validator for every
    /// value that arrives over the wire.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

// -----------------------------------------------------------------
// Isolation
// -----------------------------------------------------------------

/// How far a donor separates a running job from the rest of its machine —
/// a total order, weakest first.
///
/// Declaration order IS the strength order and `Ord` is derived from it, so
/// there is one implementation of "stronger than" (ARCH §10.6). Adding a
/// level means inserting it at its strength, which is a reviewable diff.
///
/// Deliberately has **no `Default`**: a donor that does not say how it isolates
/// has not answered the question, and defaulting the answer to either end is a
/// silent substitution (ARCH §18.3) — one direction over-promises the
/// submitter, the other silently refuses work the donor could have taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Isolation {
    /// Same process as the host. No separation at all: a panic or a leak is
    /// the host's.
    InProcess,
    /// A child process, killed as a group on timeout. Filesystem and network
    /// are the host's.
    Subprocess,
    /// A rootless container — its own filesystem and process namespace,
    /// sharing the kernel.
    RootlessContainer,
    /// A virtual machine: its own kernel.
    Vm,
}

impl Isolation {
    /// True iff a donor offering `self` may run work that requires
    /// `required` — that is, iff `self` is AT LEAST as strong.
    ///
    /// The direction is the one thing worth pinning here, and it is pinned by
    /// `isolation_covers_upward_only`: a `Vm` donor covers a `Subprocess`
    /// requirement and an `InProcess` donor covers nothing above itself. The
    /// inverse comparison would let the weakest donor accept the strictest
    /// job, which is the failure this method exists to prevent — the same
    /// shape, and the same reason, as `ConsentGrant::covers`
    /// (`sovereign-contracts/src/egress.rs:64-72`).
    pub fn covers(&self, required: Isolation) -> bool {
        *self >= required
    }
}

// -----------------------------------------------------------------
// JobRequirements
// -----------------------------------------------------------------

/// What a unit needs from the host that runs it.
///
/// A **sibling** of [`InferenceRequirements`](crate::InferenceRequirements),
/// not an extension of it: zero field overlap, because "which machine may run
/// this shell command" and "which model may serve this completion" are
/// different questions that happen to share the word. The shape is copied —
/// every field optional, serde-defaulted, and the "absent means…" rule written
/// once in an accessor rather than at each call site.
///
/// `preconditions` is `kernel_types::quality::Precondition`, the registry
/// vocabulary `quality/instruments.toml` already declares. It spells a toolbox
/// as `Container` and an executable as `Binary`, which is exactly what a CI
/// unit needs to state, so there is no second precondition enum here.
///
/// # Why there is no `effective_os()`
///
/// [`InferenceRequirements`](crate::InferenceRequirements) can default an
/// absent field because the spec names the default (`general`, `Normal`).
/// Nothing names a default host. Absent means "any", and materializing the
/// donor's own os as though the submitter had asked for it is the silent
/// substitution ARCH §18.3 forbids — so the rule is written as a predicate
/// ([`accepts_host`](Self::accepts_host)) instead, which is the same
/// one-decider discipline without the invented value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRequirements {
    /// The repo revision the unit's result is only valid against. Absent means
    /// the unit does not depend on repo state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_rev: Option<String>,
    /// Required operating system, in `std::env::consts::OS`'s spelling
    /// (`linux`, `macos`). Absent means any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// Required architecture, in `std::env::consts::ARCH`'s spelling
    /// (`x86_64`, `aarch64`). Absent means any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// The WEAKEST isolation a donor may run this unit under. Absent means the
    /// submitter states no requirement and the donor's own floor decides.
    ///
    /// Added 2026-09-10, and the gap it closes is worth recording: [`Isolation`]
    /// has been an ordered ladder with an upward-only [`covers`](Isolation::covers)
    /// since the plane was designed, `WorkOffer` has carried an `isolation`
    /// field the whole time, and `commonwealth_work::WorkRefusal::IsolationBelow`
    /// has existed with the two fields this comparison produces — and NOTHING
    /// read any of it. The offer was written by every donor and read by none,
    /// and the refusal's only construction was a Display test. A vocabulary
    /// for a check nobody performs reads, to the next person, as a check that
    /// happens.
    ///
    /// Absent is "any" for the same reason `os` and `arch` are (see the type
    /// doc): materializing the donor's own level as though the submitter had
    /// asked for it would let the weakest donor manufacture its own
    /// permission, which is §18.3's silent substitution in the one place it
    /// would be least visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<Isolation>,
    /// What must be true of the host before the unit can run at all.
    ///
    /// Serialized as the registry's own label strings (`container:
    /// sovereign-vulkan`, `binary:python3`) through
    /// [`Precondition::parse`]/`label`, so this crate does not become a second
    /// speller of a vocabulary `kernel-types` already canonicalizes
    /// (ARCH §10.6).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        with = "precondition_labels"
    )]
    pub preconditions: Vec<Precondition>,
}

impl JobRequirements {
    /// Requirements that constrain nothing — the honest reading of an absent
    /// block, and the same value as [`Default`].
    pub fn any() -> Self {
        Self::default()
    }

    /// True iff a host running `os`/`arch` satisfies the platform half.
    /// An absent field is no constraint; this is the one place that rule is
    /// written.
    pub fn accepts_host(&self, os: &str, arch: &str) -> bool {
        self.os.as_deref().is_none_or(|want| want == os)
            && self.arch.as_deref().is_none_or(|want| want == arch)
    }

    /// True iff a host at `rev` satisfies the revision half. An absent
    /// `repo_rev` is no constraint.
    pub fn accepts_repo_rev(&self, rev: &str) -> bool {
        self.repo_rev.as_deref().is_none_or(|want| want == rev)
    }

    /// True iff a donor `offering` this isolation may run the unit.
    ///
    /// The comparison is [`Isolation::covers`]'s and not a second one — the
    /// direction is the whole point and it is pinned there: a stronger donor
    /// covers a weaker demand, never the reverse. An absent requirement is no
    /// constraint, which is NOT the same as satisfied-by-anything downstream:
    /// the donor's own floor still applies, and these are two different
    /// questions (is this donor allowed to offer the kind at all, and does
    /// this unit's submitter demand more than the donor gives).
    pub fn accepts_isolation(&self, offering: Isolation) -> bool {
        self.isolation.is_none_or(|want| offering.covers(want))
    }
}

/// Wire form for [`JobRequirements::preconditions`]: the registry's label
/// strings, parsed and rendered by `kernel-types` so this crate holds no
/// second spelling of the vocabulary.
mod precondition_labels {
    use super::*;

    #[allow(clippy::ptr_arg)] // serde's `with` signature takes the concrete Vec.
    pub fn serialize<S: Serializer>(
        value: &Vec<Precondition>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let labels: Vec<String> = value.iter().map(|p| p.label()).collect();
        labels.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Precondition>, D::Error> {
        let labels = Vec::<String>::deserialize(deserializer)?;
        labels
            .iter()
            .map(|label| {
                // An unknown word is a parse ERROR naming the row, never a
                // silently-dropped field — the discipline the registry's own
                // reader states (ARCH §18.3).
                Precondition::parse(label).ok_or_else(|| {
                    serde::de::Error::custom(format!("unknown precondition: {label}"))
                })
            })
            .collect()
    }
}

// -----------------------------------------------------------------
// JobUnit
// -----------------------------------------------------------------

/// One indivisible piece of work: what to run, what it needs, and the name it
/// is answered under.
///
/// # No unit touches a model in v0, and there is no field for one
///
/// The design this type comes from lists an `envelope` field on `JobUnit`
/// twice and never defines it. It is deliberately NOT minted here. The
/// governing decision for this rung is that no unit in v0 reaches inference —
/// no model in [`JobRequirements`], none in `JobContext`, no bench lane, no
/// judge — and `cw-work-no-inference` is pre-registered in
/// `quality/campaigns/cw-lift.toml` at 0 → 0 to stop that eroding one payload
/// at a time. An `Option<InferenceRequirements>` on the central job noun is
/// exactly that erosion: it would read to the next author as sanction for
/// inference payloads on `process:v1`.
///
/// Nothing in 5c-5f asks for it either, so ARCH §19 settles the rest — a field
/// with no consumer is inventory. Carrying inference requirements on a unit is
/// an H2 concern; when something actually needs it, the field arrives with the
/// consumer that demanded it and with a definition.
// `PartialEq` and not `Eq`: `payload` is a `serde_json::Value`, which carries
// `f64` and therefore has no total equality. Derived because
// `commonwealth-work`'s order-independence property compares PROJECTIONS —
// two folds of the same journal — and a comparison through `serde_json::to_value`
// would be asserting that the JSON renderings match, which is a weaker and
// differently-shaped claim than that the states are equal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobUnit {
    /// What kind of work this is, and which revision of its contract.
    pub kind: JobKind,
    /// The unit's identity: `ContentHash::of` over the canonical bytes of
    /// `Payload::new(json!({"kind", "payload"}))`.
    ///
    /// **A field here, a computation elsewhere.** `Payload::new`
    /// (`commonwealth-rail-core/src/payload.rs:119`) is the one canonicalizer
    /// in this tree — recursive sorted keys, fractional numbers refused,
    /// >64 KiB refused, re-applied on deserialize — and `commonwealth-work`
    /// computes and verifies this value there. A leaf that cannot name
    /// `Payload` must not re-derive its rules: a second canonicalizer would
    /// disagree with the first exactly when it mattered, and the disagreement
    /// would show up as two units with different identities for one payload.
    /// So this crate carries the hash and does not produce it.
    ///
    /// Hex, lowercase — `ContentHash`'s wire form.
    pub unit_hash: String,
    /// The kind-specific body. Opaque here; the executor registered for
    /// [`kind`](Self::kind) is what gives it a schema, and
    /// [`JobExecutorDescriptor::parameters`] is where that schema is
    /// published.
    pub payload: serde_json::Value,
    /// What the host must satisfy to run it. Defaulted, because a unit that
    /// states nothing genuinely constrains nothing.
    #[serde(default)]
    pub requirements: JobRequirements,
    /// The tenant the unit acts as, when the submitting host scopes state by
    /// one. `None` on a single-tenant host — absent, never the string
    /// `"default"`, which is a tenant and not an absence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<TenantId>,
}

// -----------------------------------------------------------------
// WorkOffer
// -----------------------------------------------------------------

/// What a donor advertises it will run — static, config-derived,
/// self-reported.
///
/// [`CapabilityClaim`](crate::CapabilityClaim) for work that is not
/// inference, and the same three caveats hold: it is what the donor SAYS, it
/// is computed once from configuration, and nothing rewrites it after an
/// observed failure. A claim is not a promise and never a lease — the words
/// stay apart here on purpose, because `sovereign/docs/WORK_ATLAS.md` already
/// owns "claim" for agent coordination on the same rail.
// `PartialEq` for the same reason [`JobUnit`] carries it: a fold holding the
// live offer per donor is compared as a whole by `commonwealth-work`'s
// order-independence property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkOffer {
    /// The kinds this donor runs, at the revisions it runs them.
    pub kinds: Vec<JobKind>,
    /// How many units it will hold leases on at once.
    pub max_concurrent: u32,
    /// Whether it drops back to zero concurrency while its operator is at the
    /// keyboard. The donor's own answer; the submitter does not get a vote.
    pub yield_to_foreground: bool,
    /// The strongest isolation it can run a unit under. Compared against a
    /// unit's requirement with [`Isolation::covers`].
    pub isolation: Isolation,
    /// The donor's operating system, in `std::env::consts::OS`'s spelling.
    pub os: String,
    /// The donor's architecture, in `std::env::consts::ARCH`'s spelling.
    pub arch: String,
    /// The repositories it has checked out and will run work against.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<OfferedRepo>,
    /// Whose work it accepts, as actor keys in their hex wire form.
    ///
    /// Tri-state, the same one `HandoffQueue.allowed_peers` already carries:
    /// `None` accepts anyone on the ring, `Some(list)` accepts exactly that
    /// list, and `Some(∅)` accepts nobody but the donor itself — which is
    /// self-only, not open, and is why an empty vector is representable rather
    /// than normalized away.
    ///
    /// A `String` because the key's type lives on the rail, which this leaf
    /// cannot name; `commonwealth-work` binds it to `ActorKey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_from: Option<Vec<String>>,
}

impl WorkOffer {
    /// True iff this offer advertises `kind` at exactly that revision.
    /// Same id, different version is NOT offered — see
    /// [`JobKind::is_skew_of`] for why the two refusals stay apart.
    pub fn offers_kind(&self, kind: &JobKind) -> bool {
        self.kinds.contains(kind)
    }

    /// True iff `actor` (hex) is on the accept list.
    ///
    /// The one place the tri-state on [`accept_from`](Self::accept_from) is
    /// read. `Some(∅)` returns false for every actor including the donor's
    /// own key: "self-only" is the caller's knowledge of who it is, not a
    /// property of the list, and encoding it here would make the donor's key
    /// an implicit member of every list.
    pub fn accepts_from(&self, actor: &str) -> bool {
        match &self.accept_from {
            None => true,
            Some(allowed) => allowed.iter().any(|a| a == actor),
        }
    }
}

/// A repository a donor has on disk and will run work against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferedRepo {
    /// Absolute path on the donor. Never sent to a submitter's executor as an
    /// input path — it is here so the donor can resolve its own checkout.
    pub path: String,
    /// The remote the checkout tracks, which is what actually identifies the
    /// repo across donors.
    pub url: String,
}

// -----------------------------------------------------------------
// JobExecutorDescriptor
// -----------------------------------------------------------------

/// What a host publishes about a job executor it can run — the same subject
/// as [`ToolDescriptor`](crate::ToolDescriptor), at a work plane's
/// resolution.
///
/// Field names are `ToolDescriptor`'s where the question is the same
/// (`parameters`, `examples`, `idempotency`), and `quality::Instrument`'s
/// where a runner is asking a runner's question (`est_secs`,
/// `could_not_judge_exits`, `verdict`). Neither vocabulary is re-spelled: an
/// executor is a tool that a *runner* invokes, so both halves of the question
/// already had an owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobExecutorDescriptor {
    /// The kind this executor is registered for. Identity, not a label — a
    /// registry resolves a unit to an executor by exactly this value.
    pub kind: JobKind,
    /// The isolation the executor requires. A donor whose offer does not
    /// [`cover`](Isolation::covers) this cannot register it.
    pub isolation: Isolation,
    /// JSON schema of the accepted [`JobUnit::payload`].
    pub parameters: serde_json::Value,
    /// Concrete examples of correct payloads.
    #[serde(default)]
    pub examples: Vec<ToolExample>,
    /// Whether running a unit twice duplicates its effect. **The retry gate**:
    /// a lease that lapses is re-queued, so a `NonIdempotent` executor is one
    /// whose unit must not be re-run after a silent lessee — which makes this
    /// field, not a policy elsewhere, the thing that decides.
    pub idempotency: Idempotency,
    /// How often the lessee must renew while running. The donor heartbeats at
    /// this interval and the projection reaps a lease that misses.
    pub lease_interval_ms: u64,
    /// Seconds a scheduler RESERVES for one unit. Not a measured actual and
    /// not a timeout: under-reserving starves a unit that would have finished.
    /// `None` is "nobody has timed it", reported rather than defaulted to zero
    /// (ARCH §18.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est_secs: Option<u64>,
    /// Exit codes that mean COULD-NOT-JUDGE rather than failed. Declared per
    /// executor, never inferred — `sovereign-test.sh` exits 4 on zero tests
    /// and 5 on an unattributable run, and a runner guessing a range would get
    /// one of them wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub could_not_judge_exits: Vec<i32>,
    /// Where the runner reads this executor's verdict from.
    ///
    /// Serialized as the registry's own label (`exit-code`,
    /// `judgement-line`), through `kernel-types`, for the same reason
    /// [`JobRequirements::preconditions`] is.
    #[serde(with = "verdict_source_label")]
    pub verdict: VerdictSource,
}

/// Wire form for [`JobExecutorDescriptor::verdict`]: the registry's own label,
/// parsed and rendered by `kernel-types`.
mod verdict_source_label {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &VerdictSource,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.label())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<VerdictSource, D::Error> {
        let raw = String::deserialize(deserializer)?;
        VerdictSource::parse(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown verdict source: {raw}")))
    }
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ───── JobKind ────────────────────────────────────────────

    #[test]
    fn job_kind_parse_rejects_unversioned() {
        // The skew this type exists to make impossible: a donor and a
        // submitter that both say `ingest` and mean different contracts.
        assert_eq!(
            JobKind::parse("ingest").unwrap_err(),
            InvalidJobKind::MissingVersion
        );
    }

    #[test]
    fn job_kind_parse_rejects_at_spelling() {
        // `name@1` is what the four parse-and-discard helpers in this tree
        // accept. Refused by its own name, not folded into MissingVersion.
        assert_eq!(
            JobKind::parse("ingest@1").unwrap_err(),
            InvalidJobKind::AtVersionSpelling
        );
        assert_eq!(
            JobKind::parse("ingest@v1").unwrap_err(),
            InvalidJobKind::AtVersionSpelling
        );
    }

    #[test]
    fn job_kind_parse_accepts_the_manifest_spelling() {
        // Neither rejection above is vacuous: this is the spelling
        // `manifest::features` already publishes, and it parses.
        let k = JobKind::parse(crate::manifest::features::INGEST_V1).expect("`ingest:v1` parses");
        assert_eq!(k.id(), "ingest");
        assert_eq!(k.version(), 1);
        assert_eq!(k.to_string(), "ingest:v1");
        assert_eq!(
            JobKind::parse("  process:v2  ").expect("trimmed").id(),
            "process"
        );
    }

    #[test]
    fn job_kind_splits_on_the_last_separator_so_ids_may_carry_colons() {
        // `constraint:allowlist:url` is a real id in this crate.
        let k = JobKind::parse("constraint:allowlist:v3").expect("multi-segment id");
        assert_eq!(k.id(), "constraint:allowlist");
        assert_eq!(k.version(), 3);
        assert_eq!(k.to_string(), "constraint:allowlist:v3");
    }

    #[test]
    fn job_kind_refuses_the_second_spelling_of_one_version() {
        assert_eq!(
            JobKind::parse("ingest:v01").unwrap_err(),
            InvalidJobKind::VersionNotCanonical
        );
        assert_eq!(
            JobKind::parse("shard:3").unwrap_err(),
            InvalidJobKind::VersionNotPrefixed
        );
        assert_eq!(
            JobKind::parse("ingest:vx").unwrap_err(),
            InvalidJobKind::VersionNotNumeric
        );
        assert_eq!(
            JobKind::parse("ingest:v99999999999").unwrap_err(),
            InvalidJobKind::VersionOutOfRange
        );
        assert_eq!(JobKind::parse(":v1").unwrap_err(), InvalidJobKind::EmptyId);
        assert_eq!(JobKind::parse("  ").unwrap_err(), InvalidJobKind::Empty);
        assert_eq!(
            JobKind::parse("in gest:v1").unwrap_err(),
            InvalidJobKind::Whitespace
        );
    }

    #[test]
    fn job_kind_deserialize_runs_the_validator() {
        let k: JobKind = serde_json::from_str("\"process:v1\"").expect("valid");
        assert_eq!(k, JobKind::new("process", 1).expect("built"));
        assert_eq!(serde_json::to_string(&k).expect("ser"), "\"process:v1\"");
        assert!(serde_json::from_str::<JobKind>("\"process@1\"").is_err());
        assert!(serde_json::from_str::<JobKind>("\"process\"").is_err());
    }

    #[test]
    fn job_kind_skew_is_not_the_same_refusal_as_unknown() {
        let v1 = JobKind::parse("ingest:v1").expect("v1");
        let v2 = JobKind::parse("ingest:v2").expect("v2");
        let other = JobKind::parse("process:v1").expect("other");
        assert!(v1.is_skew_of(&v2));
        assert!(!v1.is_skew_of(&v1));
        assert!(!v1.is_skew_of(&other));
    }

    // ───── Isolation ──────────────────────────────────────────

    #[test]
    fn isolation_covers_upward_only() {
        // The direction ConsentGrant::covers pins for custody, here for
        // isolation: the STRONGER side covers the weaker requirement.
        assert!(Isolation::Vm.covers(Isolation::Subprocess));
        assert!(Isolation::RootlessContainer.covers(Isolation::InProcess));
        assert!(Isolation::Subprocess.covers(Isolation::Subprocess));
        // The inverse would let the weakest donor take the strictest job.
        assert!(!Isolation::InProcess.covers(Isolation::Subprocess));
        assert!(!Isolation::Subprocess.covers(Isolation::Vm));
    }

    // ───── JobRequirements ────────────────────────────────────

    #[test]
    fn absent_requirements_constrain_nothing() {
        let any = JobRequirements::any();
        assert!(any.accepts_host("linux", "x86_64"));
        assert!(any.accepts_host("macos", "aarch64"));
        assert!(any.accepts_repo_rev("deadbeef"));
    }

    #[test]
    fn stated_requirements_refuse_the_wrong_host() {
        let req = JobRequirements {
            os: Some("linux".into()),
            arch: Some("x86_64".into()),
            repo_rev: Some("deadbeef".into()),
            isolation: Some(Isolation::RootlessContainer),
            preconditions: vec![Precondition::Container("sovereign-vulkan".into())],
        };
        assert!(req.accepts_host("linux", "x86_64"));
        assert!(!req.accepts_host("macos", "x86_64"));
        assert!(!req.accepts_host("linux", "aarch64"));
        assert!(!req.accepts_repo_rev("cafe"));
        // The isolation half is a LADDER, not equality: a stronger donor
        // satisfies a weaker demand and never the reverse. Asserting only the
        // refusal would pass on a decider that refuses every donor.
        assert!(req.accepts_isolation(Isolation::RootlessContainer));
        assert!(req.accepts_isolation(Isolation::Vm));
        assert!(!req.accepts_isolation(Isolation::Subprocess));
        assert!(
            JobRequirements::any().accepts_isolation(Isolation::InProcess),
            "an absent requirement is no constraint — the donor's own floor \
             still decides what it may offer, and that is a different question"
        );
    }

    #[test]
    fn preconditions_round_trip_through_the_registry_spelling() {
        let req = JobRequirements {
            preconditions: vec![
                Precondition::Container("sovereign-vulkan".into()),
                Precondition::Binary("python3".into()),
                Precondition::PortListening(9741),
            ],
            ..JobRequirements::default()
        };
        let json = serde_json::to_string(&req).expect("ser");
        assert!(
            json.contains("container:sovereign-vulkan") && json.contains("binary:python3"),
            "{json}"
        );
        let back: JobRequirements = serde_json::from_str(&json).expect("de");
        assert_eq!(back, req);
        // An unknown word is an error naming it, never a dropped field.
        assert!(
            serde_json::from_str::<JobRequirements>(r#"{"preconditions":["vibes:1"]}"#).is_err()
        );
        // Absent stays absent.
        let empty: JobRequirements = serde_json::from_str("{}").expect("de");
        assert_eq!(empty, JobRequirements::any());
        assert_eq!(serde_json::to_string(&empty).expect("ser"), "{}");
    }

    // ───── WorkOffer ──────────────────────────────────────────

    fn an_offer() -> WorkOffer {
        WorkOffer {
            kinds: vec![JobKind::parse("process:v1").expect("kind")],
            max_concurrent: 2,
            yield_to_foreground: true,
            isolation: Isolation::Subprocess,
            os: "linux".into(),
            arch: "x86_64".into(),
            repos: vec![OfferedRepo {
                path: "/home/donor/dev/commonwealth-ai".into(),
                url: "https://example.invalid/commonwealth-ai.git".into(),
            }],
            accept_from: None,
        }
    }

    #[test]
    fn an_offer_is_exact_about_the_revision_it_runs() {
        let offer = an_offer();
        assert!(offer.offers_kind(&JobKind::parse("process:v1").expect("k")));
        assert!(!offer.offers_kind(&JobKind::parse("process:v2").expect("k")));
        assert!(!offer.offers_kind(&JobKind::parse("ingest:v1").expect("k")));
    }

    #[test]
    fn accept_from_is_a_tri_state_and_empty_is_not_open() {
        let mut offer = an_offer();
        assert!(
            offer.accepts_from("ab12"),
            "None accepts anyone on the ring"
        );
        offer.accept_from = Some(vec!["ab12".into()]);
        assert!(offer.accepts_from("ab12"));
        assert!(!offer.accepts_from("cd34"));
        offer.accept_from = Some(vec![]);
        assert!(
            !offer.accepts_from("ab12"),
            "Some(empty) is self-only, not open"
        );
    }

    // ───── JobUnit / JobExecutorDescriptor ────────────────────

    #[test]
    fn a_unit_round_trips_with_everything_absent_that_can_be() {
        let unit = JobUnit {
            kind: JobKind::parse("process:v1").expect("kind"),
            unit_hash: "0".repeat(64),
            payload: serde_json::json!({"argv": ["echo", "hi"]}),
            requirements: JobRequirements::any(),
            tenant: None,
        };
        let json = serde_json::to_value(&unit).expect("ser");
        assert!(json.get("tenant").is_none(), "absent, never \"default\"");
        let back: JobUnit = serde_json::from_value(json).expect("de");
        assert_eq!(back.kind, unit.kind);
        assert_eq!(back.unit_hash, unit.unit_hash);
        assert_eq!(back.requirements, unit.requirements);
    }

    #[test]
    fn a_descriptor_round_trips_the_registry_verdict_spelling() {
        let d = JobExecutorDescriptor {
            kind: JobKind::parse("process:v1").expect("kind"),
            isolation: Isolation::Subprocess,
            parameters: serde_json::json!({"type": "object"}),
            examples: vec![ToolExample {
                situation: "run one test lane".into(),
                call: serde_json::json!({"argv": ["cargo", "test"]}),
            }],
            idempotency: Idempotency::Idempotent,
            lease_interval_ms: 30_000,
            est_secs: None,
            could_not_judge_exits: vec![4, 5],
            verdict: VerdictSource::JudgementLine,
        };
        let json = serde_json::to_string(&d).expect("ser");
        assert!(json.contains("\"judgement-line\""), "{json}");
        assert!(!json.contains("est_secs"), "unmeasured is absent, not zero");
        let back: JobExecutorDescriptor = serde_json::from_str(&json).expect("de");
        assert_eq!(back.verdict, VerdictSource::JudgementLine);
        assert_eq!(back.could_not_judge_exits, vec![4, 5]);
        assert!(serde_json::from_str::<JobExecutorDescriptor>(
            &json.replace("judgement-line", "vibes")
        )
        .is_err());
    }
}
