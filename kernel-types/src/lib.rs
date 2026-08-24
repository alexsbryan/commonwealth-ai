// SPDX-License-Identifier: AGPL-3.0-or-later
//! The neutral kernel — identity and provenance, owned by no product domain.
//!
//! Layer 0, beside `oicp-types`. This crate exists because of a measurement:
//! on 2026-08-19 the types all three product domains spoke numbered 23, and 22
//! of them were owned by `corpus-engine` — including `CorpusEngine` (102
//! methods), `CorpusIndex` (112 methods), and corpus-engine's own `Error` and
//! `Result`. Three systems depending on one system's implementation is not a
//! contract, and it is why none of the three could be released, reasoned about
//! or reused independently.
//!
//! # The dependency rule
//!
//! This crate names NOTHING from `sovereign`, `corpus-engine`, `commonwealth`
//! or `studio`. It is below all of them and the arrow only ever points down.
//! That is enforced two ways: `quality/ARCH_LAYERS.toml` puts it in the bottom
//! `contract` layer, and `scripts/nc-boundary.py` classifies it as its own
//! layer-0 domain (`kernel`), so an edge from here into a product domain
//! renders as a BACKFLOW violation rather than as ordinary traffic.
//!
//! # Two membranes at layer 0, not one
//!
//! `oicp-types` is the FEDERATION contract — `Capability`, `ProviderManifest`,
//! `LatencyClass`: what one node advertises to another. This crate is the
//! IDENTITY and PROVENANCE contract — what a piece of content *is* and where
//! it came from. They are deliberately separate crates. Conflating them would
//! make every consumer of an id also a consumer of the wire protocol.
//!
//! # What is here
//!
//! Two families. **Identity and provenance** — what a piece of content is and
//! where it came from:
//!
//! | Type | Answers |
//! |---|---|
//! | [`ContentHash`] | which bytes are these — identity from essence (ARCH §7.5) |
//! | [`CorpusId`] | which corpus |
//! | [`NodeId`] | which node on the mesh |
//! | [`Origin`] | where did this content come from |
//! | [`Source`] | ...through which store or channel (a CLOSED sum) |
//! | [`Server`] | ...served by which machine |
//! | [`Grain`] | ...and may it ground a claim verbatim, or is it derived |
//! | [`Locator`] | ...at which span inside its document |
//! | [`Custody`] | where the content stands for sharing |
//! | [`Attribution`] | which engine computed a piece of text |
//!
//! **Trust and freshness** — how much a result is worth, and how old it is
//! (minted 2026-08-20, rung nc-10-judgement; see [`judgement`] for why it is
//! here and not in `sovereign-contracts`):
//!
//! | Type | Answers |
//! |---|---|
//! | [`Verdict`] | passed, failed, could-not-judge, never-ran — four, not two |
//! | [`Reason`] | why it reads that way, with the placeholders refused |
//! | [`Freshness`] | is the artifact behind it still worth quoting |
//! | [`Judgement`] | all of the above about one named subject, and it renders |
//!
//! **The released turn** — what the user is shown and what it stands on
//! (minted 2026-08-20, rung nc-11-answer; see [`answer`] for why these are
//! here rather than in `sovereign-contracts`, which sits BELOW `corpus-engine`
//! and so cannot hold a type in this family):
//!
//! | Type | Answers |
//! |---|---|
//! | [`Seal`] | the one question a sealed body of evidence must answer |
//! | [`Citation`] | a quote the seal vouched for, and where it came from |
//! | [`Draft`] | composed text whose only exit is release — it cannot be read |
//! | [`Answer`] | text + citations + provenance + judgement, and no door without one |
//! | [`PeerAnswer`] | an answer the custody sweep cleared to leave this machine |
//! | [`Refused`] | why a citation or a peer release said no — a value, not a log line |
//!
//! # The "ten types" cap, and what the bar actually measures
//!
//! The rung-1 header of this file read *"Ten types, which is the campaign's
//! hard cap"* and this crate stood at exactly ten. That conflated two
//! different numbers and the conflation would have blocked the rung that
//! completes the crate. The `nc-kernel` bar's instrument is
//! `nc-boundary.py --json | .kernel_size`, and `kernel_size` counts **types
//! that all three product domains speak** — 22 today, every one of them owned
//! by `corpus-engine`, target ≤ 10 *in a crate no domain owns*. It does not
//! count this crate's public surface, and adding a type here can only lower
//! it, never raise it. The cap is on shared vocabulary owned by a product
//! domain, which is the disease; a neutral crate holding fourteen types is
//! the cure wearing a larger number.
//!
//! The spec (`quality/TARGET_ARCHITECTURE.md` §2.1) writes `Origin` in a form
//! that implies sixteen types. Six of those are deliberately NOT minted here:
//! `DocumentId` and `AssetId` collapse into [`ContentHash`] (§7.5 — identity
//! from essence, never a counter or an address), `Url` and `ToolId` stay
//! `String` because a URL space and a tool registry are open sets and a
//! newtype over an open set buys nothing (ARCH §2/§4), `PeerName` is a display
//! string, and `Timestamp` stays a primitive because the kernel cannot take
//! `sovereign-time` without creating the exact backflow edge it exists to
//! forbid.

pub mod answer;
pub mod attribution;
pub mod custody;
pub mod hash;
pub mod ids;
pub mod judgement;
pub mod origin;
#[cfg(any(test, feature = "wire-fixture"))]
pub mod wire;

pub use answer::{Answer, Citation, Draft, PeerAnswer, Refused, Seal, TURN_SUBJECT};
pub use attribution::Attribution;
pub use custody::{join_custody, Custody};
pub use hash::ContentHash;
pub use ids::{CorpusId, NodeId};
pub use judgement::{honesty_footer, render_rows, Freshness, Judgement, Reason, Verdict};
pub use origin::{Grain, Locator, Origin, Server, Source};
