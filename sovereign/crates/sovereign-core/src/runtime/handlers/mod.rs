// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-intent dispatch handlers for `Runtime`.
//!
//! Each handler module declares its own `impl Runtime { ... }` block
//! holding the `handle_<intent>` async method for one intent class.
//! The top-level dispatch in `runtime.rs::Runtime::handle_message_stream_with_classification`
//! pattern-matches the intent and calls the corresponding method on
//! `self` — call-site syntax is unchanged from the pre-split shape.
//!
//! `impl Runtime`-across-files is the Rust-native form for this split
//! (no trait, no vtable hop on the dispatch hot path), and was the
//! load-bearing choice flagged in `SYSTEM_OVERVIEW.md` §10.1.

mod ask_move;
mod attached_doc;
mod code_query;
mod commissive;
mod complex_task;
mod conation;
mod document_op;
mod expressive;
mod generative;
mod knowledge_query;
mod metalingual;
mod recipe_author;
mod simple;
