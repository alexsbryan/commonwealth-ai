// SPDX-License-Identifier: AGPL-3.0-or-later
//! First-run setup asks the program that loads the model (five-programs
//! fp-25, HUMAN-fp25-setup-host (a); pb-distribution-setup). A machine with no
//! config has no serve to dial, so setup execs the loader, the stock binary
//! `svrn daemon run` execs, on its `--setup-probe` launch and reads one JSON
//! answer. This crate links no planner, no hardware probe and no downloader.
//!
//! The loader is found by the one locator (`daemon_bin::locate`,
//! `SOVEREIGN_DAEMON_BIN`). Absent, setup refuses by name (principle 6).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::de::DeserializeOwned;
use sovereign_contracts::daemon_wire::FetchedBytes;
use sovereign_contracts::launch::SETUP_PROBE_FLAG;

/// The loader the probe execs, or the refusal naming what to build.
pub(crate) fn loader() -> Result<PathBuf, String> {
    crate::daemon_bin::locate().ok_or_else(|| {
        "setup asks the loader to probe this machine and plan its models, and \
         'sovereign-stock' was not found. Build it with `cargo build -p sovereign-stock`, \
         or set SOVEREIGN_DAEMON_BIN to its path."
            .to_string()
    })
}

/// Ask the loader `question` and parse the one JSON line it answers. A
/// refusal comes back as the loader's own reason (its stderr).
pub(crate) fn ask<T: DeserializeOwned>(question: &str, args: &[&str]) -> Result<T, String> {
    ask_via(&loader()?, question, args)
}

pub(super) fn ask_via<T: DeserializeOwned>(
    bin: &Path,
    question: &str,
    args: &[&str],
) -> Result<T, String> {
    let out = std::process::Command::new(bin)
        .arg(SETUP_PROBE_FLAG)
        .arg(question)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("setup probe: cannot run {}: {e}", bin.display()))?;
    tracing::debug!(
        target: "setup",
        question,
        loader = %bin.display(),
        code = ?out.status.code(),
        "setup probe answered"
    );
    if !out.status.success() {
        return Err(refusal(question, out.status.code(), &out.stderr));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| format!("setup probe `{question}` answered nothing"))?;
    serde_json::from_str(line)
        .map_err(|e| format!("setup probe `{question}`: unreadable answer ({e}): {line}"))
}

/// Fetch `url` to `dest` through the loader (resumed, validated against the
/// slot's size floor), calling `on_progress` for each progress line.
pub(super) async fn download(
    url: &str,
    dest: &Path,
    size_gb: f64,
    on_progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
) -> Result<(), String> {
    download_via(&loader()?, url, dest, size_gb, on_progress).await
}

pub(super) async fn download_via(
    bin: &Path,
    url: &str,
    dest: &Path,
    size_gb: f64,
    on_progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
) -> Result<(), String> {
    use tokio::io::AsyncBufReadExt as _;
    let mut child = tokio::process::Command::new(bin)
        .arg(SETUP_PROBE_FLAG)
        .arg("download")
        .arg(url)
        .arg(dest)
        .arg(size_gb.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("setup probe: cannot run {}: {e}", bin.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "setup probe download: no stdout".to_string())?;
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| format!("setup probe download: {e}"))?
    {
        match serde_json::from_str::<FetchedBytes>(&line) {
            Ok(p) => on_progress(p.downloaded, p.total),
            Err(e) => {
                tracing::debug!(target: "setup", %line, error = %e, "setup probe download: not a progress line")
            }
        }
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| format!("setup probe download: {e}"))?;
    tracing::debug!(target: "setup", url, code = ?out.status.code(), "setup probe download finished");
    if out.status.success() {
        Ok(())
    } else {
        Err(refusal("download", out.status.code(), &out.stderr))
    }
}

/// The loader's reason, or its exit code when it gave none.
fn refusal(question: &str, code: Option<i32>, stderr: &[u8]) -> String {
    let why = String::from_utf8_lossy(stderr).trim().to_string();
    if why.is_empty() {
        format!("setup probe `{question}` exited {code:?} with no reason")
    } else {
        why
    }
}
