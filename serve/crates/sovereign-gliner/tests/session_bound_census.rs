// SPDX-License-Identifier: AGPL-3.0-or-later
//! Structural ratchet: this crate builds no unbounded ONNX session.
//!
//! **Why a census and not a behavioural test.** The behavioural check
//! guard 3 wanted — "an over-size input errors instead of growing the
//! arena" — is not writable, because `ort 2.0.0-rc.9` exposes no arena
//! size limit at all (`session_bound.rs` module docs cite the search).
//! There is no input that makes an unbounded arena refuse. What CAN be
//! made falsifiable is the thing that actually decays: a new session built
//! somewhere that skips the policy. That is this file.
//!
//! **The failing input**, and it is the pre-2026-09-12 source: `gliner2.rs`
//! called `Session::builder()?.commit_from_file(..)` with no options, and
//! `gliner_ner.rs` handed `orp` a bare `RuntimeParameters::default()` whose
//! provider list is empty — so `DisableCpuMemArena` was never called and
//! both backends ran with ORT's arena on. Revert either and this test goes
//! red.
//!
//! A source census rather than a runtime one because the property is
//! "nobody else does it", which no single run can observe.

use std::path::{Path, PathBuf};

fn crate_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// `(file name, contents)` for every `.rs` under `src/`, excluding the
/// module that OWNS the policy.
fn source_files_outside_the_policy() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(crate_src()).expect("read src/") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("utf-8 file name")
            .to_string();
        if name == "session_bound.rs" {
            continue;
        }
        out.push((name, std::fs::read_to_string(&path).expect("read source")));
    }
    assert!(
        out.len() >= 5,
        "the census found only {} source files — it is scanning the wrong directory, \
         and an empty scan would pass vacuously",
        out.len()
    );
    out
}

/// Strip `//`-comments so a doc comment mentioning the forbidden call —
/// this crate's docs cite `Session::builder()` by name several times — is
/// not read as a call site.
fn code_only(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The gliner2 backend builds its own session; it must build it through
/// the policy. A bare `Session::builder()` anywhere else is a session with
/// the CPU arena ON and memory-pattern planning ON.
#[test]
fn no_session_is_built_outside_session_bound() {
    let offenders: Vec<String> = source_files_outside_the_policy()
        .into_iter()
        .filter(|(_, src)| code_only(src).contains("Session::builder()"))
        .map(|(name, _)| name)
        .collect();
    assert!(
        offenders.is_empty(),
        "these files call Session::builder() directly instead of \
         session_bound::bounded_session_builder(): {offenders:?}. A session built \
         that way runs with ORT's CPU memory arena ENABLED — the pooling allocator \
         whose 1/2/2/8/32 GB power-of-two blocks malloc_history caught on \
         2026-09-12."
    );
}

/// The v1 path does not own its builder — `orp::Model::new` does — so its
/// only lever is `RuntimeParameters`, and a default one carries an EMPTY
/// execution-provider list, which never calls `DisableCpuMemArena`. This
/// is the path the daemon actually runs.
#[test]
fn the_v1_path_never_passes_a_bare_runtime_parameters() {
    let offenders: Vec<String> = source_files_outside_the_policy()
        .into_iter()
        .filter(|(_, src)| code_only(src).contains("RuntimeParameters::default()"))
        .map(|(name, _)| name)
        .collect();
    assert!(
        offenders.is_empty(),
        "these files hand orp a bare RuntimeParameters::default() instead of \
         session_bound::v1_runtime_parameters(): {offenders:?}. Its provider list \
         is empty, so DisableCpuMemArena is never called and the v1 session runs \
         with the arena on."
    );
}

/// The census must be able to SEE an offender — otherwise both tests above
/// are asserting over an empty set for the wrong reason (a bad path, a
/// comment-stripper that eats the code). Positive control.
#[test]
fn the_census_detects_a_planted_offender() {
    let planted = vec![
        (
            "planted.rs".to_string(),
            "let s = Session::builder()?.commit_from_file(p)?;".to_string(),
        ),
        (
            "planted2.rs".to_string(),
            "GLiNER::new(Parameters::default(), RuntimeParameters::default(), t, m)".to_string(),
        ),
    ];
    assert!(planted
        .iter()
        .any(|(_, s)| code_only(s).contains("Session::builder()")));
    assert!(planted
        .iter()
        .any(|(_, s)| code_only(s).contains("RuntimeParameters::default()")));
    // ...and a doc comment naming it is NOT an offender.
    assert!(
        !code_only("//! calls `Session::builder()` with no options").contains("Session::builder()")
    );
}
