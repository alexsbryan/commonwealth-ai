// SPDX-License-Identifier: AGPL-3.0-or-later
//! What this machine is, in the terms a bug report needs: the facts that
//! decide whether a build runs here and whether the embedding model fits.
//! A fact this platform does not expose is reported absent, never guessed.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct Machine {
    pub os: &'static str,
    pub arch: &'static str,
    pub cpus: Option<usize>,
    pub memory_total_bytes: Option<u64>,
    pub version: &'static str,
}

pub(super) fn machine() -> Machine {
    Machine {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        cpus: std::thread::available_parallelism().ok().map(|n| n.get()),
        memory_total_bytes: memory_total_bytes(),
        version: env!("CARGO_PKG_VERSION"),
    }
}

impl Machine {
    pub(super) fn summary(&self) -> String {
        let cpus = self
            .cpus
            .map_or("unknown CPUs".to_string(), |n| format!("{n} CPUs"));
        let memory = self
            .memory_total_bytes
            .map_or("unknown memory".to_string(), |b| {
                format!("{:.1} GB memory", b as f64 / 1e9)
            });
        format!(
            "svrn {} on {} {}, {cpus}, {memory}",
            self.version, self.os, self.arch
        )
    }
}

#[cfg(target_os = "linux")]
fn memory_total_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    meminfo_total(&meminfo)
}

#[cfg(target_os = "macos")]
fn memory_total_bytes() -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn memory_total_bytes() -> Option<u64> {
    None
}

/// `MemTotal:  131072000 kB` → bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn meminfo_total(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_total_reads_kilobytes_and_refuses_what_it_cannot() {
        let sample = "MemFree:  10 kB\nMemTotal:       4000000 kB\n";
        assert_eq!(meminfo_total(sample), Some(4_096_000_000));
        assert_eq!(meminfo_total("MemFree: 10 kB\n"), None);
        let absent = Machine {
            os: "linux",
            arch: "aarch64",
            cpus: None,
            memory_total_bytes: None,
            version: "0.0.0",
        };
        assert_eq!(
            absent.summary(),
            "svrn 0.0.0 on linux aarch64, unknown CPUs, unknown memory"
        );
    }
}
