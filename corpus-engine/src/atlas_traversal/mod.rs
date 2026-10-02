// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas query traversal + brief assembly.
//!
//! This module is the read side of the v2 enrichment stack — it
//! takes a natural-language query, classifies it into a traversal
//! plan, walks the resolved atlas, and assembles a brief the
//! caller can hand to an LLM (or display directly).
//!
//! The classifier is intentionally simple. It's a keyword +
//! known-entity-name pattern matcher, not an LLM. A classifier
//! mistake returns `QueryPlan::Unknown` with the raw query; the
//! caller can then fall back to a generic retrieval path. When
//! the classifier *does* match, the plan captures everything the
//! traversal engine needs to walk the atlas deterministically.
//!
//! Its sibling `question_kind.rs` answers a DIFFERENT question and
//! by a different method: which row of the navigation table a
//! reader's question selects, by centroid over the map's own
//! exemplars (ARCH §2.4). The two do not overlap — this one names a
//! target entity for the brief assembler, that one names a walk —
//! and neither replaces the other.
//!
//! The brief assembler (`brief.rs`) renders a `TraversalResult`
//! into prose. Every atom carries an `enrichment_depth` tag and
//! the assembler calibrates language on it: `Extracted` atoms
//! (the only kind today's pipelines produce) get interpretive
//! framing — "the atlas records that…", "attributed to…" — so the
//! consumer knows this is extraction, not structural fact.
//!
//! Not yet covered in this module (follow-ups): cross-corpus
//! traversal, query battery runner, benchmarks, manifest writes.

// `brief`, `classifier`, `engine` and `spans` are PURE and moved to
// `understanding-atlas` by domains `dm-understanding-pure-1` (the batch row
// for the `pure` tier). Re-exported at the historical paths so every
// in-engine reach keeps resolving. `question_kind` joined the
// `corpus-engine-atlas-reader` leaf 2026-09-21 (FIVE_PROGRAMS §12 decision 1
// — the ground walk's row selection classifies through it); re-exported at
// the historical path.
pub use corpus_engine_atlas_reader::question_kind;
pub use understanding_atlas::atlas_traversal::{brief, classifier, engine, spans};

pub use brief::{assemble_brief, depth_frame_records, Brief};
pub use classifier::{classify_query, classify_query_with, QueryPlan, QueryTarget};
pub use engine::{traverse, TraversalResult};
pub use question_kind::{kind_space_embedding, KindScore, KindSource, QuestionKindClassifier};
pub use spans::{detect_atom_spans, AtomSpan};
