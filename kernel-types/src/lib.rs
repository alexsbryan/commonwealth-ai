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
//! Ten types, which is the campaign's hard cap, and the cap is the design
//! constraint rather than an accident of what fitted:
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
//! The spec (`quality/TARGET_ARCHITECTURE.md` §2.1) writes `Origin` in a form
//! that implies sixteen types. Six of those are deliberately NOT minted here:
//! `DocumentId` and `AssetId` collapse into [`ContentHash`] (§7.5 — identity
//! from essence, never a counter or an address), `Url` and `ToolId` stay
//! `String` because a URL space and a tool registry are open sets and a
//! newtype over an open set buys nothing (ARCH §2/§4), `PeerName` is a display
//! string, and `Timestamp` stays a primitive because the kernel cannot take
//! `sovereign-time` without creating the exact backflow edge it exists to
//! forbid.

pub mod attribution;
pub mod custody;
pub mod hash;
pub mod ids;
pub mod origin;

pub use attribution::Attribution;
pub use custody::{join_custody, Custody};
pub use hash::ContentHash;
pub use ids::{CorpusId, NodeId};
pub use origin::{Grain, Locator, Origin, Server, Source};
