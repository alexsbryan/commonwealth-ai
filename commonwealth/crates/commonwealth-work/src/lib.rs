// SPDX-License-Identifier: AGPL-3.0-or-later
//! The work plane on the ring rail.
//!
//! A unit of work is submitted, offered for, leased, renewed and completed as
//! **signed acts on one rail namespace**. There is no queue server, no lease
//! table and no HTTP route: the queue is a fold over
//! [`Admission`](commonwealth_rail_core::Admission), which is the total order
//! every node already agrees on. A node that holds the journal holds the
//! queue.
//!
//! # The three properties this crate exists to hold
//!
//! **Zero I/O and zero clock in the core.** The codec, the seal, the fold and
//! the lease predicate read nothing and ask nothing what time it is. `now` is
//! a parameter of every function that needs it, which is what makes the fold
//! reproducible on a peer that replays a journal from 2029. The exceptions are
//! [`process`] — the executor seam, where work actually runs — and
//! [`attribution`], where a host reads its own `rustc` to say what it is; both
//! are behind the `process` feature so a lifter of the fold never links them.
//!
//! **One canonical writer.** A unit's identity is
//! [`ContentHash`](kernel_types::ContentHash) over the bytes of a
//! [`Payload`](commonwealth_rail_core::Payload), and `Payload::new` is the
//! only canonicalizer in this tree — recursive sorted keys, fractional numbers
//! refused, >64 KiB refused, re-applied on deserialize. There is no second one
//! here and there must never be: two canonicalizers disagree exactly when it
//! matters, and the disagreement shows up as two identities for one payload.
//!
//! **A package closure a third party can lift.** `commonwealth-work` joins
//! `[[package]] commonwealth` in `quality/ARCH_LAYERS.toml` with its own
//! `[[forbid]] -> sovereign-*` row, and `commonwealth/BOUNDARY.md` records the
//! measured closure. cw-lift 5f builds this crate outside the monorepo; the
//! manifest's dependency list is the contract, not a preference.
//!
//! # Module map — who owns what (cw-lift 5c)
//!
//! Wave 1 (this lane) landed [`actor`], [`act`] and [`seal`], and this file.
//! Wave 2 adds two modules each, and adds its own `pub mod` line here so the
//! two lanes never edit the same region:
//!
//! - **Lane B** — `src/projection.rs` (`WorkProjection::fold`, `expired`) and
//!   `src/refusal.rs` (`WorkRefusal`, `may_take`). Add:
//!   `pub mod projection;` and `pub mod refusal;`
//! - **Lane C** — `src/executor.rs` (`JobExecutor`, the registry, `JobContext`,
//!   `JobError`) and `src/process.rs` (`ProcessExecutor`). Add:
//!   `pub mod executor;` and
//!   `#[cfg(feature = "process")] pub mod process;`
//!
//! Modules are `pub mod` and this file re-exports only Wave 1's own surface,
//! so a Wave 2 lane never has to touch a re-export list either.

pub mod act;
pub mod actor;
/// How a host describes ITSELF when it reports work. Behind `process` with
/// the executor, and for the same reason: it reads `rustc --version`, and a
/// lifter of the fold alone must not link a subprocess.
#[cfg(feature = "process")]
pub mod attribution;
pub mod executor;
#[cfg(feature = "process")]
pub mod process;
pub mod projection;
pub mod refusal;
pub mod seal;

pub use act::{
    from_payload, kind_of, read, to_payload, Completion, Failure, RailWorkAct, Revocation,
    Submission, UnitRef, WorkAct, WorkActKind, WorkActs, DEFAULT_TTL_SECS, MAX_TTL_SECS,
    MIN_TTL_SECS,
};
pub use actor::{ActorKey, InvalidActorKey};
// The handoff id, re-exported for the same reason `ActorKey` and `seal` are:
// a client of this plane should not have to link the crate that happens to
// DEFINE the noun in order to use the plane. `svrn quality check --distribute`
// (cw-lift 5e) needed exactly `HandoffId::generate()` and nothing else from
// `commonwealth-core`, and taking the direct dep grew that crate's fan-in
// 14 -> 15 — a god-crate edge bought for one constructor (ARCH §8.3).
pub use commonwealth_core::ids::HandoffId;
pub use seal::{seal, unit_hash, verify, WorkSealError};

/// The rail namespace every act in this crate rides on.
///
/// ONE spelling, here, for the same reason `measurements_rail` reuses
/// `MEASUREMENTS_APP_ID` verbatim (ARCH §10.6): a second literal on the
/// daemon side would be a second answer to what this data is called, and the
/// symptom would be a silently empty fold rather than an error.
///
/// It is deliberately NOT added to `DAEMON_OWN_NAMESPACES`: that would flip
/// the ring's roster to Derived and orphan the operator-written `roster.json`,
/// and `refuse_derived_roster` hardcodes `measurements`, so nothing would warn.
pub const WORK_NAMESPACE: &str = "work";

/// The tracing target every refusal, lost lease, expiry and unreadable row in
/// this crate is emitted under (ARCH §9.1).
///
/// It is the crate's own module-path root on purpose. A custom target is dark
/// unless somebody remembered to add it to an env-filter allowlist; this one
/// is caught by the `RUST_LOG=commonwealth_work=debug` a reader would have
/// typed anyway.
pub const TRACE_TARGET: &str = "commonwealth_work";
