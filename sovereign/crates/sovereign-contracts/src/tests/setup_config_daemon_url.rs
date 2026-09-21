// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for `client_daemon_base`, which reads PROCESS-global env and so takes a
//! lock of its own — see `setup_config.rs`.
//!
//! Their own file because keeping them inline put `setup_config.rs` past
//! its arch-gate slack, and this file past the 1,200-line ceiling
//! (ARCH §3.1/§3.2). `#[path]`, so the names are unchanged.

use super::*;

// These mutate PROCESS-global env. nextest (the default engine) is
// process-per-test and would not need serialising; `--engine cargo` runs
// every test in this binary in one process, and a gate that passes only
// under one executor is not a gate (§18.1). Hence the lock.
static DAEMON_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const DAEMON_URL_KEYS: [&str; 2] = ["SOVEREIGN_DAEMON_URL", "SVRNMESH_DAEMON_URL"];

/// Clears BOTH spellings, applies `pairs`, and restores the prior values on
/// drop — so a developer running the suite with the knob exported in their
/// shell gets the same verdict as CI.
struct DaemonEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prior: Vec<(&'static str, Option<String>)>,
}

impl DaemonEnvGuard {
    fn set(pairs: &[(&'static str, &str)]) -> Self {
        let lock = DAEMON_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prior = DAEMON_URL_KEYS
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        for k in DAEMON_URL_KEYS {
            std::env::remove_var(k);
        }
        for (k, v) in pairs {
            std::env::set_var(k, v);
        }
        Self { _lock: lock, prior }
    }
}

impl Drop for DaemonEnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.prior {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

/// RED on the tree before 2026-08-25: `client_daemon_base` read the config
/// and nothing else, so the knob reached one call site out of ~30 and
/// `svrn enrich` silently drove the operator's daemon instead of the one
/// the operator had just pointed it at.
#[test]
fn client_daemon_base_honours_the_env_knob() {
    let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "http://127.0.0.1:19741")]);
    assert_eq!(client_daemon_base(), "http://127.0.0.1:19741");
}

#[test]
fn client_daemon_base_accepts_the_svrnmesh_spelling() {
    let _g = DaemonEnvGuard::set(&[("SVRNMESH_DAEMON_URL", "http://127.0.0.1:19742")]);
    assert_eq!(client_daemon_base(), "http://127.0.0.1:19742");
}

/// Both set is not a coin flip: SOVEREIGN_ wins, matching every other
/// reader of the pair.
#[test]
fn client_daemon_base_prefers_sovereign_over_svrnmesh() {
    let _g = DaemonEnvGuard::set(&[
        ("SOVEREIGN_DAEMON_URL", "http://127.0.0.1:19741"),
        ("SVRNMESH_DAEMON_URL", "http://127.0.0.1:19742"),
    ]);
    assert_eq!(client_daemon_base(), "http://127.0.0.1:19741");
}

/// A blank knob is UNSET, not an empty base URL — otherwise
/// `SOVEREIGN_DAEMON_URL= svrn enrich` would POST to `/v1/chat/completions`
/// with no host and fail somewhere unrecognisable.
#[test]
fn client_daemon_base_treats_blank_env_as_unset() {
    let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "   ")]);
    assert!(
        client_daemon_base().starts_with("http://localhost:"),
        "blank knob must fall through to the configured port, got {}",
        client_daemon_base()
    );
}

/// Callers append `/v1/…`; a trailing slash would build a double-slash
/// path that a strict router treats as a different route.
#[test]
fn client_daemon_base_trims_trailing_slash() {
    let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "http://127.0.0.1:19741/")]);
    assert_eq!(client_daemon_base(), "http://127.0.0.1:19741");
    assert_eq!(
        format!("{}/v1/models", client_daemon_base()),
        "http://127.0.0.1:19741/v1/models"
    );
}

/// The unchanged leg: with no knob the configured port still decides, and
/// the host spelling stays `localhost` for parity with `urls::v1_url`.
#[test]
fn client_daemon_base_without_env_is_the_configured_port() {
    let _g = DaemonEnvGuard::set(&[]);
    let base = client_daemon_base();
    assert!(
        base.starts_with("http://localhost:"),
        "expected the configured-port form, got {base}"
    );
}

/// The pure builder is the accessor for daemon-process MANAGEMENT, and it
/// must stay env-blind — `daemon restart` pointing at a rented pod would
/// otherwise kill the local daemon and report READY off the pod (§18.3).
#[test]
fn client_daemon_base_for_ignores_the_env_knob() {
    let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "http://a-rented-pod:9841")]);
    assert_eq!(client_daemon_base_for(9741), "http://localhost:9741");
}
