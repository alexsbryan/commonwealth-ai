// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recipe-authoring contract helpers shared between the daemon's bespoke ingest
//! path and the extractable workflow/recipe package.
//!
//! These are pure-CPU building blocks (no LanceDB, no llama.cpp, no document
//! I/O) that BOTH sides must agree on bit-for-bit — the workflow `SectionTool`
//! and the `enrich` ingest pipeline segment text with the *same* detector, so a
//! recipe authored against the package behaves identically when run by the
//! daemon. Relocating them here (from `corpus-engine`) makes that agreement a
//! contract rather than a coincidence, and lets the package compute them
//! locally instead of shipping whole documents over MCP.

pub mod sections;
