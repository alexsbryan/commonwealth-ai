// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two JSON forms `svrn setup` grew for first-run clients
//! (sv-surface svt-7): `--plan --json`, and `--yes --json`'s line stream.
//! A client that spawned this process has no terminal, so these shapes
//! are its whole view of the run.
use super::*;
use sovereign_contracts::daemon_wire::SetupProgressLine;

/// The emitter's mode and its capture are process-global — they have to
/// be, because the narration sites span six modules and several
/// `tokio::join!` arms. Test threads share that process, so the three
/// tests that install a capture take this lock first. Without it they
/// pass or fail by scheduling order, which is a test that proves nothing
/// (ARCH principle 5).
static EMIT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn emit_guard() -> std::sync::MutexGuard<'static, ()> {
    EMIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drive a download through a stand-in loader that speaks the probe's
/// `download` protocol (one `FetchedBytes` line per tick), and assert the
/// LINE SEQUENCE a client sees: an opening `downloading_primary` at zero
/// bytes, then progress lines naming the same file, then the terminal
/// `done` carrying the config path. The real loader's download is
/// `download_gguf`'s tests and the setup probe e2e.
///
/// Watched fail: delete the `narrator.emit` call in
/// `download.rs::download_with_progress_via` and the "more than the opening
/// line" assertion goes red; drop `emit::done`'s `config_path` and the
/// last assertion does.
#[cfg(unix)]
#[tokio::test]
async fn the_json_run_reports_a_download_as_a_line_sequence() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("real.gguf");
    let loader = tmp.path().join("loader");
    std::fs::write(
        &loader,
        "#!/bin/sh\n\
         echo '{\"downloaded\":0,\"total\":2097152}'\n\
         echo '{\"downloaded\":2097152,\"total\":2097152}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&loader, std::fs::Permissions::from_mode(0o755)).unwrap();

    let _guard = emit_guard();
    emit::set_json_mode(true);
    emit::start_capture();
    let narrator = emit::DownloadNarrator::new(
        SetupProgressPhase::DownloadingPrimary,
        "Downloading the main responder.",
        "real.gguf",
    );
    download::download_with_progress_via(
        &loader,
        "http://stand-in.invalid/real-model.gguf",
        &dest,
        "real",
        0.001,
        Some(&narrator),
    )
    .await
    .expect("stub download should succeed");
    emit::narrate(
        SetupProgressPhase::WritingConfig,
        "Writing your configuration.",
    );
    emit::done(std::path::Path::new("/tmp/svrnmesh/config.toml"));
    let lines = emit::take_capture();
    emit::set_json_mode(false);

    let parsed: Vec<SetupProgressLine> = lines
        .iter()
        .map(|l| {
            serde_json::from_str(l).unwrap_or_else(|e| panic!("not a progress line: {l} ({e})"))
        })
        .collect();
    assert!(
        parsed.len() >= 3,
        "expected an opening line, at least one progress line, and the \
         terminal pair — got {parsed:#?}"
    );
    assert_eq!(parsed[0].phase, SetupProgressPhase::DownloadingPrimary);
    assert_eq!(parsed[0].downloaded, Some(0));
    assert_eq!(parsed[0].file.as_deref(), Some("real.gguf"));
    assert!(
        parsed[1..]
            .iter()
            .filter(|l| l.phase == SetupProgressPhase::DownloadingPrimary)
            .any(|l| l.downloaded.unwrap_or(0) > 0),
        "the download must report bytes, not just announce itself: {parsed:#?}"
    );
    let last = parsed.last().expect("non-empty");
    assert_eq!(last.phase, SetupProgressPhase::Done);
    assert_eq!(
        last.config_path.as_deref(),
        Some("/tmp/svrnmesh/config.toml"),
        "the terminal line names what the client should read next"
    );
    assert!(last.error.is_none());
}

/// A refusal reaches the parser as a `failed` line carrying the reason,
/// not only as an exit code.
///
/// Watched fail: drop the `emit::failed(msg)` call from `fail` and the
/// capture is empty.
#[test]
fn a_refusal_reaches_the_parser_with_its_reason() {
    let _guard = emit_guard();
    emit::set_json_mode(true);
    emit::start_capture();
    let code = fail("bundled manifest is missing fast or embed slot for your profile");
    let lines = emit::take_capture();
    emit::set_json_mode(false);

    assert_eq!(code, 1, "a refusal is exit 1");
    assert_eq!(lines.len(), 1, "one terminal line: {lines:?}");
    let l: SetupProgressLine = serde_json::from_str(&lines[0]).expect("parses");
    assert_eq!(l.phase, SetupProgressPhase::Failed);
    assert_eq!(
        l.error.as_deref(),
        Some("bundled manifest is missing fast or embed slot for your profile")
    );
}

/// Nothing is delivered when the run was not asked for JSON — the
/// human-only path is unchanged, and a client that forgot `--json`
/// gets silence rather than half a stream.
#[test]
fn no_lines_are_delivered_without_json_mode() {
    let _guard = emit_guard();
    emit::set_json_mode(false);
    emit::start_capture();
    emit::narrate(SetupProgressPhase::DetectingHardware, "hello");
    emit::done(std::path::Path::new("/tmp/x"));
    assert!(emit::take_capture().is_empty());
}

// ── --primary ────────────────────────────────────────────────────

/// Two rows in the shape the loader's `plan` answers (the catalog itself is
/// the planner's, tested beside it in sovereign-inference).
fn catalog() -> Vec<PrimaryOption> {
    ["Big-Q4.gguf", "Small-Q8.gguf"]
        .iter()
        .enumerate()
        .map(|(i, file)| PrimaryOption {
            profile: "very_high".into(),
            slot: SlotConfig {
                file: file.to_string(),
                hf_url: "https://huggingface.co/org/repo".into(),
                ..Default::default()
            },
            recommended: i == 0,
        })
        .collect()
}

/// The loader's download URL per catalog file, as its `plan` answer carries.
fn urls(cat: &[PrimaryOption]) -> BTreeMap<String, String> {
    cat.iter()
        .map(|o| {
            let url = format!(
                "https://huggingface.co/org/repo/resolve/main/{}",
                o.slot.file
            );
            (o.slot.file.clone(), url)
        })
        .collect()
}

/// The loader's `byom-url` answer for a pasted `/blob/` link.
fn byom(link: &str) -> Result<ByomSource, String> {
    Ok(ByomSource {
        url: link.replace("/blob/", "/resolve/"),
        file: link.rsplit('/').next().unwrap_or_default().to_string(),
    })
}

fn resolve(spec: &str, cat: &[PrimaryOption]) -> Result<PrimaryChoice, String> {
    resolve_primary_flag(spec, cat, &urls(cat), &byom)
}

fn args_for(flags: &[&str]) -> Result<Opts, String> {
    super::args::parse_args(&flags.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

/// A catalog file name picks that row and fetches it from the manifest's
/// own URL — the non-interactive form of typing a number at the picker.
#[test]
fn primary_flag_resolves_a_catalog_file() {
    let cat = catalog();
    let want = cat[0].slot.file.clone();
    match resolve(&want, &cat).expect("catalog row resolves") {
        PrimaryChoice::Download { slot, url } => {
            assert_eq!(slot.file, want);
            assert_eq!(Some(&url), urls(&cat).get(&want));
        }
        PrimaryChoice::InPlace { .. } => panic!("a catalog row is fetched, not used in place"),
    }
}

/// A pasted `/blob/` page link is rewritten to the raw `/resolve/` path
/// — the same rule the desktop's "Advanced — bring your own" affordance
/// applied, now in one place (`setup_planner::resolve_byom_url`).
#[test]
fn primary_flag_resolves_a_byom_url() {
    let cat = catalog();
    let spec = "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/blob/main/Q4.gguf";
    match resolve(spec, &cat).expect("url resolves") {
        PrimaryChoice::Download { slot, url } => {
            assert_eq!(slot.file, "Q4.gguf");
            assert!(
                url.contains("/resolve/") && !url.contains("/blob/"),
                "{url}"
            );
            assert_eq!(slot.hf_url, url, "the slot carries the URL it came from");
        }
        PrimaryChoice::InPlace { .. } => panic!("a URL is fetched"),
    }
}

/// A `.gguf` already on disk is used WHERE IT SITS. Watched fail: return
/// `Download` here and setup re-fetches a file the user already has.
#[test]
fn primary_flag_uses_a_local_gguf_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("mine.gguf");
    std::fs::write(&p, b"GGUF").unwrap();
    match resolve(p.to_str().unwrap(), &catalog()).expect("path resolves") {
        PrimaryChoice::InPlace { slot, path } => {
            assert_eq!(path, p);
            assert_eq!(slot.file, "mine.gguf");
        }
        PrimaryChoice::Download { .. } => panic!("a local file is not downloaded"),
    }
}

// ── flag parsing ─────────────────────────────────────────────────

/// `--plan` has one rendering and says so. Watched fail: drop the check
/// in `parse_args` and `svrn setup --plan` exits 0 having printed a
/// human banner a parser cannot read.
#[test]
fn plan_without_json_is_refused_by_name() {
    let e = args_for(&["--plan"]).unwrap_err();
    assert!(e.contains("--json"), "{e}");
    let o = args_for(&["--plan", "--json"]).expect("--plan --json parses");
    assert!(o.plan && o.json);
}

/// `--fim` and `--terminal` narrate their own way and emit no progress
/// line. Accepting `--json` there would hand a parser an empty stream
/// and exit 0 — "did not answer" read as "answered: no".
#[test]
fn json_is_refused_on_the_destinations_that_do_not_report_it() {
    assert!(args_for(&["--json", "--fim"]).is_err());
    assert!(args_for(&["--json", "--terminal", "http://halo:9741"]).is_err());
}

/// `--primary` takes its value, and refuses a bare trailing flag rather
/// than silently consuming the next one.
#[test]
fn primary_takes_a_value() {
    let o = args_for(&["--yes", "--primary", "Model-Q4.gguf"]).expect("parses");
    assert_eq!(o.primary.as_deref(), Some("Model-Q4.gguf"));
    assert!(args_for(&["--primary"]).is_err());
}

/// An unrecognised `--primary` is REFUSED by name. Watched fail: replace
/// the final `Err` with a fall-through to the recommendation and setup
/// installs a model nobody asked for and reports success (principle 6).
#[test]
fn an_unknown_primary_is_refused_rather_than_defaulted() {
    let e = resolve("something-else", &catalog()).unwrap_err();
    assert!(e.contains("is not a catalog file"), "{e}");
    let e = resolve("/no/such/model.gguf", &catalog()).unwrap_err();
    assert!(e.contains("no such file"), "{e}");
}
