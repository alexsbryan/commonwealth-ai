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
//! The brief assembler (`brief.rs`) renders a `TraversalResult`
//! into prose. Every atom carries an `enrichment_depth` tag and
//! the assembler calibrates language on it: `Extracted` atoms
//! (the only kind today's pipelines produce) get interpretive
//! framing — "the atlas records that…", "attributed to…" — so the
//! consumer knows this is extraction, not structural fact.
//!
//! Not yet covered in this module (follow-ups): cross-corpus
//! traversal, query battery runner, benchmarks, manifest writes.

pub mod brief;
pub mod classifier;
pub mod engine;
pub mod spans;

pub use brief::{assemble_brief, Brief};
pub use classifier::{classify_query, QueryPlan, QueryTarget};
pub use engine::{traverse, TraversalResult};
pub use spans::{detect_atom_spans, AtomSpan};
