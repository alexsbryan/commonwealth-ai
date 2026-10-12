// SPDX-License-Identifier: AGPL-3.0-or-later
//! This machine's GPUs, as the shipped llama-server reports them
//! (`llama-server --list-devices`), not through the vendored ggml (P dec 5).
//!
//! The probe runs a child, so it is taken once at startup ([`observe`] or the
//! first [`observed`]) and at setup, and published with its time. A status
//! field reads [`published`], which never spawns and never blocks on a probe.
//! A missing binary or a failed probe is published as a failure: its readers
//! report the GPU as unknown, never as 0 GB.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime};

/// The binary's name beside the daemon's own executable.
pub const SERVER_BINARY: &str = "llama-server";

/// `--list-devices` answers in about 50 ms on the Halo; a probe that has not
/// answered by this is a failed one.
const PROBE_DEADLINE: Duration = Duration::from_secs(20);

const MIB: u64 = 1024 * 1024;

/// One device `--list-devices` names: `Vulkan0`, its description, and its
/// memory as the server prints it, in whole MiB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuDevice {
    pub name: String,
    pub description: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// One probe: when it ran, which binary answered, and what it read.
#[derive(Debug, Clone)]
pub struct GpuObservation {
    pub at: SystemTime,
    pub binary: PathBuf,
    /// The devices, possibly none (a CPU-only host); `Err` names why the
    /// probe could not read them.
    pub devices: Result<Vec<GpuDevice>, String>,
}

impl GpuObservation {
    /// The first device with memory, the one the loader puts layers on.
    /// `Ok(None)` is a host with no GPU; `Err` is unknown.
    pub fn first_gpu(&self) -> Result<Option<&GpuDevice>, &str> {
        match &self.devices {
            Ok(devices) => Ok(devices.iter().find(|d| d.total_bytes > 0)),
            Err(why) => Err(why),
        }
    }

    /// Total device memory of the local GPUs in whole GB, the figure the mesh
    /// advertises. `Ok(None)` is a host with no GPU; `Err` is unknown.
    pub fn total_vram_gb(&self) -> Result<Option<u32>, &str> {
        let devices = self.devices.as_ref().map_err(String::as_str)?;
        let total: u64 = devices.iter().map(|d| d.total_bytes).sum();
        Ok((total > 0).then_some((total / (1024 * MIB)) as u32))
    }
}

static PUBLISHED: RwLock<Option<Arc<GpuObservation>>> = RwLock::new(None);
static PROBING: Mutex<()> = Mutex::new(());

/// `llama-server` beside this executable, whether or not it is there.
pub fn shipped_server_binary() -> Result<PathBuf, String> {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("locating this executable: {e}"))?
        .parent()
        .map(|dir| dir.join(SERVER_BINARY))
        .ok_or_else(|| "this executable has no directory".to_string())
}

/// The last published observation. Never spawns; `None` before the first.
pub fn published() -> Option<Arc<GpuObservation>> {
    PUBLISHED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// The published observation, probing the shipped binary first if none has
/// been published in this process. Blocks for one probe at most once.
pub fn observed() -> Arc<GpuObservation> {
    let _one = PROBING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(seen) = published() {
        return seen;
    }
    publish(probe_shipped())
}

/// Probe the shipped binary now and publish the reading (startup, setup).
pub fn observe() -> Arc<GpuObservation> {
    let _one = PROBING.lock().unwrap_or_else(PoisonError::into_inner);
    publish(probe_shipped())
}

fn publish(seen: GpuObservation) -> Arc<GpuObservation> {
    let seen = Arc::new(seen);
    *PUBLISHED.write().unwrap_or_else(PoisonError::into_inner) = Some(seen.clone());
    seen
}

fn probe_shipped() -> GpuObservation {
    match shipped_server_binary() {
        Ok(binary) => probe(&binary),
        Err(why) => failed(PathBuf::from(SERVER_BINARY), why),
    }
}

fn failed(binary: PathBuf, why: String) -> GpuObservation {
    tracing::warn!(target: "gpu_probe", binary = %binary.display(), %why, "GPU probe failed: GPU memory reads as unknown");
    GpuObservation {
        at: SystemTime::now(),
        binary,
        devices: Err(why),
    }
}

/// Run `binary --list-devices` and read its answer. Blocks for the child.
pub fn probe(binary: &Path) -> GpuObservation {
    if !binary.is_file() {
        let why = format!(
            "no {SERVER_BINARY} at {}: install it with the daemon",
            binary.display()
        );
        return failed(binary.to_path_buf(), why);
    }
    match run(binary).and_then(|out| parse(&out)) {
        Ok(devices) => {
            tracing::info!(target: "gpu_probe", binary = %binary.display(), ?devices, "GPU probe read the shipped server's devices");
            GpuObservation {
                at: SystemTime::now(),
                binary: binary.to_path_buf(),
                devices: Ok(devices),
            }
        }
        Err(why) => failed(binary.to_path_buf(), why),
    }
}

fn run(binary: &Path) -> Result<String, String> {
    let mut child = Command::new(binary)
        .arg("--list-devices")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("starting {} --list-devices: {e}", binary.display()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < PROBE_DEADLINE => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} --list-devices did not answer in {PROBE_DEADLINE:?}",
                    binary.display()
                ));
            }
            Err(e) => {
                return Err(format!(
                    "waiting on {} --list-devices: {e}",
                    binary.display()
                ))
            }
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("reading {} --list-devices: {e}", binary.display()))?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        false => Err(format!(
            "{} --list-devices exited {}",
            binary.display(),
            out.status
        )),
    }
}

/// `Available devices:` then one `  <name>: <description> (<total> MiB,
/// <free> MiB free)` per device. The description may hold parentheses, so the
/// memory is read from the last ` (`.
fn parse(out: &str) -> Result<Vec<GpuDevice>, String> {
    let mut lines = out.lines();
    if !lines.any(|l| l.trim() == "Available devices:") {
        return Err(format!("--list-devices printed no device list: {out:?}"));
    }
    lines
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let unread = || format!("--list-devices line unread: {line:?}");
            let (name, rest) = line.trim().split_once(": ").ok_or_else(unread)?;
            let (description, memory) = rest.rsplit_once(" (").ok_or_else(unread)?;
            let (total, free) = memory
                .strip_suffix(" MiB free)")
                .and_then(|m| m.split_once(" MiB, "))
                .ok_or_else(unread)?;
            let mib = |s: &str| s.parse::<u64>().map(|n| n * MIB).map_err(|_| unread());
            Ok(GpuDevice {
                name: name.to_string(),
                description: description.to_string(),
                total_bytes: mib(total)?,
                free_bytes: mib(free)?,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "gpu_probe_tests.rs"]
mod tests;
