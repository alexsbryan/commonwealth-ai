// SPDX-License-Identifier: AGPL-3.0-or-later
//! # sovereign-compute — the supervised compute-child process boundary
//!
//! `DISTRIBUTED_PILOT_READINESS.md` P1: inference compute runs in a
//! supervised **child process**, so a ggml `SIGABRT` (worker loss,
//! version mismatch, OOM) kills only the child — the daemon keeps
//! gossip, `/status`, the client API, and the desktop bridge alive and
//! observes the child's exit as an *event* it can re-plan around.
//!
//! This crate is the runtime-layer home for that boundary. It was seeded
//! by extracting the child-process supervisor out of `sovereign-desktop`
//! (which daemon crates cannot depend on) so both the desktop daemon
//! supervisor and the daemon's compute-child manager share one, tested
//! supervision state machine.
//!
//! ## Modules
//! - [`supervisor`] — spawn / heartbeat / backoff / crash-loop-budget /
//!   crash-log state machine, publishing every transition over a
//!   `broadcast` channel. Reused verbatim by the desktop.
//! - [`wire`] — the native lossless wire contract (route constants, body
//!   types, NDJSON codec, error envelope).
//! - [`server`] — the child's axum router over an `Arc<dyn
//!   InferenceProvider>`.
//! - [`client`] — [`client::ComputeChildClient`], the daemon-side typed
//!   HTTP client for a child.
//! - [`mock`] — the model-free provider (`--role mock`, `serve`'s `mock` engine).
//! - [`child_main`] — the child process entrypoint (`--compute-child`),
//!   reached by re-executing the daemon binary.
//! - [`assembly`] — the one serving assembly: config in, the provider a
//!   serving process installs out; [`containment`] is its admission guard.

pub mod assembly;
pub mod child;
pub mod child_main;
pub mod client;
pub mod containment;
pub mod discovery_policy;
pub mod distributed_discovery;
pub mod distributed_respawn;
pub mod distribution;
pub mod manager;
pub mod mock;
pub mod ner;
pub mod preflight;
pub mod server;
pub mod setup_reads;
pub mod supervisor;
pub mod wire;

/// Run `f` under a subscriber with `filter` — a directive from the daemon's
/// own tracing filter — and return what reached the log, so a test proves an
/// event is visible in a deployed daemon, not merely emitted.
#[cfg(test)]
pub(crate) fn logged_under<T>(filter: &str, f: impl FnOnce() -> T) -> (T, String) {
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned();
    (out, text)
}
