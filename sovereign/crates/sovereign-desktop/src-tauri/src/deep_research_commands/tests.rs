// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the deep-research desktop client.
//!
//! The run-dir readers' tests (snapshot, stage, constitution, consent
//! class) moved with the readers to `sovereign-mesh/src/research_http.rs`
//! on 2026-09-11; what stays here is what the client still owns — the
//! demo override, and the two structural scans.

use super::*;

/// ONE lock for the ONE process-global these tests mutate. Both of them
/// declared a `static ENV_LOCK` INSIDE their own fn — two distinct
/// statics, so they serialized against nothing, and the pair raced on
/// `SOVEREIGN_DEMO_DR_FLAGS` whenever they shared a process. Under
/// nextest each test gets its own process and the bug was invisible; a
/// plain `cargo test -p sovereign-desktop` fails about one run in two.
/// A lock is shared state or it is decoration.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn demo_backend_override_is_absent_unless_the_demo_var_is_set() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
    assert!(
        demo_backend_override().is_none(),
        "unset = the live port; real flows must not gain a mock backend"
    );
    unsafe {
        std::env::set_var(
            "SOVEREIGN_DEMO_DR_FLAGS",
            "--backend mock --mock-deck /tmp/deep-research-deck",
        )
    };
    assert_eq!(
        demo_backend_override(),
        Some((
            "mock".to_string(),
            Some(PathBuf::from("/tmp/deep-research-deck"))
        )),
        "the demo pass-through lands in typed request fields, not an argv"
    );
    unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
}

/// A token the closed set does not name is IGNORED rather than
/// forwarded: with no second process to parse it, passing it on
/// would mean pretending it did something.
#[test]
fn demo_backend_override_ignores_tokens_it_does_not_name() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe {
        std::env::set_var(
            "SOVEREIGN_DEMO_DR_FLAGS",
            "--backend mock --nonsense 7 --mock-deck /tmp/d",
        )
    };
    assert_eq!(
        demo_backend_override(),
        Some(("mock".to_string(), Some(PathBuf::from("/tmp/d"))))
    );
    unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
}

/// THE SHIPPING TEST, structurally (§7: make it structural, not
/// remembered). Deep research used to run by spawning `svrn
/// deep-research`, found by probing PATH — so a desktop-only install got
/// zero runs, and an install that DID have one could bind a different
/// version than the app was built against. Then it ran by LINKING the
/// loop into the app — a second in-process copy beside the CLI's, with
/// the daemon serving none. Both are visible here and neither is allowed:
/// the driver reaches for no binary, and it names no loop symbol.
///
/// The scan is scoped to the PRODUCTION half of the file — everything
/// above `#[cfg(test)]`. This test necessarily spells the forbidden
/// tokens, and an instrument that trips on its own prose measures
/// nothing (note 8714cf3c). The driver is one file again since the
/// readers moved down, so one `include_str!` covers all of it.
#[test]
fn the_driver_starts_no_subprocess_probes_no_path_and_links_no_loop() {
    let src = include_str!("mod.rs");
    let body = src.split_once("#[cfg(test)]").map_or(src, |(head, _)| head);
    for forbidden in [
        "Command::new",
        "SOVEREIGN_CLI_PATH",
        "sovereign-cli",
        ".local/bin/sovereign",
        "sovereign_core::deep_research",
        "deep_research::run",
        "deep_research::resume",
    ] {
        assert!(
            !body.contains(forbidden),
            "the deep-research driver is a client of the daemon's job surface, but mod.rs \
             names `{forbidden}` — a binary on PATH can be a different version than this \
             build, and a loop linked here is a second copy of the daemon's"
        );
    }
}
