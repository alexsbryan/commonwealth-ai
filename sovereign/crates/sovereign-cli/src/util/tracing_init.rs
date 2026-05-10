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
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .with_target(false)
        .try_init();
}
