// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn slow_probe_error_names_budget_and_load_not_down() {
    let msg = daemon_probe_error("http://127.0.0.1:9740", DaemonProbe::Slow).unwrap();
    assert_eq!(
        msg,
        "error: daemon at http://127.0.0.1:9740 answered slower than 5s — daemon under load?"
    );
    // Slow is not DOWN: no start-it hint on a daemon that may be
    // serving — that hint is the DOWN case's alone.
    assert!(!msg.contains("svrn daemon start"));
}

#[test]
fn down_probe_error_keeps_the_start_it_hint_verbatim() {
    let msg = daemon_probe_error("http://127.0.0.1:9740", DaemonProbe::Down).unwrap();
    assert_eq!(
        msg,
        "error: daemon is not responding at http://127.0.0.1:9740 — start it with `svrn daemon start` or equivalent"
    );
}

#[test]
fn slow_and_down_never_share_a_message() {
    let base = "http://127.0.0.1:9740";
    assert_ne!(
        daemon_probe_error(base, DaemonProbe::Slow),
        daemon_probe_error(base, DaemonProbe::Down)
    );
    assert!(daemon_probe_error(base, DaemonProbe::Up).is_none());
}

#[test]
fn slow_message_budget_tracks_the_module_constant() {
    // The "5s" in the message must stay glued to the probe's actual
    // budget — one threshold, one name (the const at
    // shared/crates/corpus-index/src/v1_models.rs).
    let msg = daemon_probe_error("http://x", DaemonProbe::Slow).unwrap();
    let expected = format!(
        "error: daemon at http://x answered slower than {}s — daemon under load?",
        V1_MODELS_TIMEOUT.as_secs()
    );
    assert_eq!(msg, expected);
}
