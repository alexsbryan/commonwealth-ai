// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cross-corpus topic-to-topic ontological bridge (SEP ↔ Wikipedia).
//!
//! Promotes the meta-atlas from name-equality `Entity` clustering to a
//! **typed concept-alignment graph**. Where the rest of the meta-atlas
//! keys on per-name equality, the bridge aligns whole *topics* (article
//! units) across surface-form and granularity mismatch and emits typed,
//! reversible edges (`same` / `broader` / `narrower` / `related`) that
//! retrieval consumes for a "stereo" cross-corpus view.
//!
//! Pipeline:
//!   1. [`topic_node`] — fold each article's atoms into a [`BridgeTopic`]
//!      (file-driven, synchronous).
//!   2. `candidates` — embedding ANN + name probe → candidate WP topics.
//!   3. `signals` — graded [`AlignmentSignal`]s score each pair.
//!   4. `adjudicate` — an injected LLM forced-choice types the uncertain
//!      band.
//!   5. [`edges`] — persist typed edges + a reversible append-only oplog.
//!
//! This landing wires the file-driven core (topic nodes + edge store +
//! oplog); the async candidate/signal/adjudication stages follow.

pub mod adjudicate;
pub mod build;
pub mod edges;
pub mod lookup;
pub mod signals;
pub mod topic_node;

pub use adjudicate::{
    adjudication_schema, build_adjudication_prompt, parse_adjudication_response, AdjudicateFn,
    AdjudicationRequest, AdjudicationVerdict,
};
pub use build::{
    build_bridge, BridgeBuildConfig, BridgeBuildReport, BridgeBuildStats, DriverTopic,
};
pub use edges::{
    default_bridge_edges_path, read_bridge_edges, write_bridge_edges, BridgeAct, BridgeEdge,
    BridgeEdgesFile, BridgeOpKind, BridgeRelation, BridgeSignal, EdgeSource, TopicRef, ALIGNER,
};
pub use lookup::BridgeIndex;
pub use signals::{
    AlignmentBand, AlignmentScore, AlignmentSignal, SignalContext, SignalHit, SignalStack,
};
pub use topic_node::{topic_from_atlas, topic_from_chunk, BridgeTopic};
