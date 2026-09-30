// SPDX-License-Identifier: AGPL-3.0-or-later
//! The leaf half of the CLI shared set (FIVE_PROGRAMS fp-98). Every module
//! here is re-exported at its historical `sovereign_cli_shared::<m>` path.
//!
//! - [`help`]: shared `Help` struct + `print` formatter.
//! - [`dirs`]: canonical filesystem layout (`~/.svrnmesh/…`).
//! - [`dispatcher`]: locating the `sovereign-cli` dispatcher from a sibling.
//! - [`guest_link`]: the guest-link record on disk.
//! - [`urls`]: the client daemon base URL.
//! - [`rail`] (`rail-client`): the operator-side rail client.
//! - [`repo`], [`prompts`], [`deprecation`], [`tracing_init`], [`models`],
//!   [`code_index`] (`tempfile_dir`) and [`mcp_client`] (`mcp-client`): moved
//!   from `sovereign-cli-shared` so the code program's CLI names no svrn CLI
//!   crate (pb-code-cli-base).
//! - [`flag_surface`] and [`host_load`]: moved from `sovereign-cli-shared` so
//!   bench's CLI names no svrn CLI crate (pb-cli-llm-bench-move); so did
//!   [`corpus_resolve`], the corpus-argument ladder every CLI verb shares.

pub mod code_index;
pub mod corpus_resolve;
pub mod deprecation;
pub mod dirs;
pub mod dispatcher;
pub mod flag_surface;
pub mod guest_link;
pub mod help;
pub mod host_load;
#[cfg(feature = "mcp-client")]
pub mod mcp_client;
pub mod models;
pub mod prompts;
#[cfg(feature = "rail-client")]
pub mod rail;
pub mod repo;
pub mod tracing_init;
pub mod urls;
