// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host's load average — ONE reader, shared by the check runner and the
//! lanes it drives.
//!
//! # What this is, and what it stopped being on 2026-09-08
//!
//! It is a COVARIATE. `svrn quality check` reads it when a row starts and
//! again when it finishes, and writes both onto that row in `summary.json`
//! (`quality_check_cmd::exec::InstrumentRun::load_start`/`load_end`). Nothing
//! branches on the number.
//!
//! Until 2026-09-08 this module also owned `host_quiet(max_load)` and a
//! three-state `HostQuietness`, and two surfaces gated on it: the check
//! runner's `Precondition::HostQuiet` refused to run a wall-clock lane above
//! a declared bound, and the `chat-ask` lane refused to judge its `per-stage
//! ceilings` row. Both are deleted. **A precondition may assert that the
//! SUBJECT of the measurement exists; it may not assert that the WORLD is
//! convenient** (ARCH §18.2). The bound was 4.0, nobody derived it, and on
//! the host that runs the check it was unmet most of the time — so three
//! consecutive proof runs recorded every wall-clock row as could-not-judge
//! naming the load, and the table read as careful while learning nothing.
//! The sharpest case: one `chat-ask` run at load 4.27-4.32 reported `failed`
//! on q1 and `could-not-judge` on q2, because the bound was evaluated per
//! question and the load crossed 4.0 between them.
//!
//! # Why a load average at all, then
//!
//! Because the wall-clock bars are wall-clock and this host's speed moves
//! under them. Measured on the authoring machine, same binary, same bank,
//! same bar: primary-slot decode 50.7 tok/s at 1-minute load 3.7, 45.7 tok/s
//! at load 22, and 17.8 tok/s at load 32 — a 2.8x spread (note `d596639c`).
//! That evidence is why the number is worth recording. It was never evidence
//! for a particular threshold, which is the step the deleted guard took.
//!
//! # What it does NOT see
//!
//! A decode in flight on the daemon that costs this host no CPU — a request
//! served by a mesh peer. The daemon serves no queue-depth or slot-busy
//! field to read instead (`/status` carries `inference.resident[]` and
//! `process.rss_mb`; there is no queue route), so load is a CORRELATED
//! instrument, not an equivalent one. As a covariate on a row that is a
//! caveat on the reading; as a gate it was a guard that could be wrong in
//! both directions.

/// The host's 1-minute load average.
///
/// `None` where the platform reports none — and callers record that absence
/// rather than substituting a zero, which would read as an idle host
/// (ARCH §18.3).
pub fn load_average_1m() -> Option<f64> {
    let mut avg = [0f64; 3];
    // SAFETY: `getloadavg` writes at most `nelem` doubles into the buffer and
    // returns how many it actually wrote. The buffer holds 3 and we pass 3.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
    if n >= 1 {
        Some(avg[0])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This host does report one, and a load average is never negative.
    ///
    /// The whole of what this module now promises: a reading, or a named
    /// absence. There is no threshold left to test.
    #[test]
    fn the_load_average_reads_on_this_platform() {
        let l = load_average_1m().expect("macOS and Linux both report getloadavg");
        assert!(l >= 0.0, "a load average is never negative, got {l}");
    }
}
