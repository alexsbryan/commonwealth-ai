// SPDX-License-Identifier: AGPL-3.0-or-later
//! The enrichment catalog — the on-disk store every enrichment host shares.
//!
//! Three processes touch `<data-root>/enrichment/<corpus-id>/`: the CLI
//! (`svrn enrich …`) reads and writes it, the daemon's watched-folder driver
//! synthesizes it, and the desktop lists it. Before this crate each of them
//! carried its own copy of the layout and the schema, because both lived
//! inside `sovereign-cli-llm` — a host binary, which nothing may depend on.
//!
//! What that cost, measured at `4c42e191`:
//!
//! - `EnrichConfigJson` in `sovereign-tools` claimed to mirror `EnrichConfig`
//!   "field-for-field" and was four fields short.
//! - `sovereign-desktop` read the file through `serde_json::Value` with the
//!   field names spelled out by hand, so a rename here could not reach it.
//! - The three disagreed on the ROOT. `sovereign-tools` and the desktop used
//!   `rebrand::data_dir()`; the CLI used `rebrand::svrnmesh_root()` via
//!   `sovereign_cli_shared::dirs`. Those are the same path until
//!   `SOVEREIGN_DATA_DIR` / `SVRNMESH_DATA_DIR` is set, at which point the
//!   daemon wrote the config where the CLI subprocess it spawned would not
//!   look. See [`paths`] for the accessor this crate settled on.
//!
//! Layer: `capabilities`. It may name `corpus-engine`, `sovereign-core` and
//! `sovereign-contracts`; nothing above it may be named from here, which is
//! what lets all three hosts consume it (`quality/ARCH_LAYERS.toml`).

pub mod catalog;
pub mod config;
pub mod corpus_state;
pub mod paths;

pub use catalog::{enriched_corpus_ids, list_enriched_corpora, EnrichedCorpusSummary};
pub use config::{EnrichConfig, PhaseOverride, TocMarkers, CONFIG_SCHEMA_VERSION};
