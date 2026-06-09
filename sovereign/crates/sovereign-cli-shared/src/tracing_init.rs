// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tracing subscriber bootstrap. Three match arms in `main.rs` (mesh /
//! setup / daemon) each built the same subscriber with only the
//! `default_filter` string differing — same `.with_target(false)`,
//! same `RUST_LOG`-aware env filter, same `.init()`. This helper
//! collapses that boilerplate.

/// Install a tracing subscriber that writes to stderr with:
/// - `RUST_LOG` honoured when set,
/// - otherwise `default_filter` applied (e.g.
///   `"sovereign_cli=info,sovereign_mesh=info"`),
/// - no module target prefix (the subcommand name is already in the
///   log line preamble users care about),
/// - a single global init — calling twice is a no-op (tracing panics
///   otherwise in some versions; we swallow the result).
///
/// Call exactly once per process, early in whichever subcommand
/// decides tracing is warranted.
pub fn init_tracing(default_filter: &str) {
    // Silence lance's per-`Dataset::open` INFO event (`target:
    // lance::dataset_events`, e.g. `loading … chunks.lance … status=success`).
    // Opening an index is routine, and the daemon re-opens every installed
    // corpus's index repeatedly (status, search, mesh advertise), so at INFO
    // it floods the log with one line per corpus per pass. WARN still
    // surfaces real lance failures. Added on top of RUST_LOG / the default
    // filter so it holds whichever path built the base filter.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| default_filter.into())
        .add_directive(
            "lance::dataset_events=warn"
                .parse()
                .expect("static lance directive parses"),
        );
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        // `fmt()` defaults to stdout despite the historical docstring
        // claiming stderr. That mismatch broke `sovereign search-gym
        // run --json > out.json`: the JSON payload and tracing WARN
        // lines interleaved on stdout, leaving the file unparseable.
        // Force stderr so subcommands that emit machine-readable
        // stdout (gym, eval, future bench tools) get a clean channel
        // by default. Operators tailing logs see no change — they
        // were already reading stderr in the typical "2>&1 | grep"
        // shell shape.
        .with_writer(std::io::stderr)
        .try_init();
}
