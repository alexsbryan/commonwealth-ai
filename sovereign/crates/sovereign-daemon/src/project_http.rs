// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/v1/projects/*` moved to the code program with the Reindexer (phase-b
//! pb-code-freshness); every item stays reachable at this path. The daemon
//! mounts it through its sovereign-code edge until pb-code-daemon-exit.

pub use sovereign_code::project_http::*;
