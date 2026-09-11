// SPDX-License-Identifier: AGPL-3.0-or-later
//! How a host describes THE ENVIRONMENT WORK RAN IN when it reports it — one
//! reader, three callers.
//!
//! It said "describes ITSELF" until 2026-09-10, and that word was the bug. A
//! unit that runs inside `sandbox::Sandbox::Container` runs on the image's OS,
//! the image's architecture and the image's compiler; a donor that reported
//! its own kernel had every correct verdict thrown away by
//! [`ComputeAttribution::comparable_to`] — measured on a macOS host running a
//! Linux image. [`of_sandbox`] is the reading for work that ran in a boundary
//! and [`of_this_host`] for work that ran in this process; the executor that
//! ran it picks, because it is the only thing that knows.
//!
//! [`ComputeAttribution`] is the type; this is the one place that fills it in
//! from the machine actually running. It exists because there were three
//! fillers and they were drifting apart:
//!
//! - `sovereign-mesh::work_donor::attribution` — what the donor ran at.
//! - `sovereign-cli::quality_check_cmd::distribute::local_attribution` — what
//!   a local run would have been attributed to, the reference every donor's
//!   `provenance` is checked against.
//! - `commonwealth-work/examples/work_peer.rs` — a lifted peer's own, which
//!   cw-lift 5f recorded as the third and called it a hole in this crate's
//!   surface: "every donor invents its own `ComputeAttribution` — which is
//!   precisely the value `comparable_to` exists to compare."
//!
//! # Independent readings, one implementation
//!
//! The submitter's attribution and the donor's MUST be independent readings,
//! or the merge would be asserting on a field the subject supplied (ARCH
//! §18.1) — a donor that reported its toolchain and had that same value used
//! as the reference would always compare equal to itself. Sharing this
//! function does not weaken that: each side calls it on its OWN machine and
//! reads its OWN `rustc`. What is shared is the METHOD, not the value, which
//! is the distinction §10.6 is about.
//!
//! # Why the absences are named, and why one spelling is now safe
//!
//! A value that cannot be read is a named absence
//! ([`kernel_types::is_absent_marker`] recognises it), never an empty string
//! and never a plausible guess (ARCH §18.3). Until the rule landed in
//! [`ComputeAttribution::comparable_to`], the two readers were kept honest
//! only by spelling their absences DIFFERENTLY — "this checkout's PATH"
//! against "this donor's PATH" — so two unreadable hosts compared unequal by
//! luck rather than by rule. Converging them here would have turned "neither
//! of us knows" into "we agree". The type now refuses to compare a named
//! absence to anything, so one spelling is correct rather than merely tidier.

use kernel_types::{ComputeAttribution, Server};

/// The rev of a workdir that is not a checkout.
///
/// A named absence, so a reader can tell "no revision" from a revision. It
/// must never be the empty string: every unreadable host would then carry the
/// same empty rev, and before the comparability rule that read as agreement.
pub const ABSENT_REV: &str = "unknown (this host's workdir is not a git checkout)";

/// The toolchain absence, in the same vocabulary. "Where this work ran" and
/// not "this host": under a boundary the PATH that matters is the image's.
pub const ABSENT_TOOLCHAIN: &str = "unknown (rustc is not on the PATH where this work ran)";

/// A toolchain field from a `rustc --version` READING, whoever took it.
///
/// The one place a read becomes a field, so the host's answer and an image's
/// answer cannot disagree about what counts (§10.6). `None` is "could not
/// run"; `Some("")` is the subtler one — exit 0 with nothing on stdout is not
/// a toolchain, and recording it as one would be a substitution (§18.3).
pub fn toolchain_from(read: Option<String>) -> String {
    match read.map(|v| v.trim().to_string()) {
        Some(v) if !v.is_empty() => v,
        empty => {
            tracing::warn!(
                target: crate::TRACE_TARGET,
                read_something = empty.is_some(),
                "work: `rustc --version` gave no usable answer — reporting the toolchain as a named absence"
            );
            ABSENT_TOOLCHAIN.to_string()
        }
    }
}

/// The rev, with the one value that must never survive turned into a named
/// absence: an empty rev compares equal to every other unreadable host's.
fn named_rev(repo_rev: String) -> String {
    if repo_rev.trim().is_empty() {
        ABSENT_REV.to_string()
    } else {
        repo_rev
    }
}

/// The attribution for work that ran inside `sandbox` — the IMAGE's os, arch
/// and compiler when there is a boundary, this host's when there is not.
///
/// The compiler is read INSIDE the boundary, per report rather than once at
/// boot: it is a fact about where the work ran, and one container start
/// (0.1 s measured) beside a unit that just ran a test suite is not a cost
/// worth caching around.
pub fn of_sandbox(
    repo_rev: impl Into<String>,
    sandbox: &crate::sandbox::Sandbox,
) -> ComputeAttribution {
    // No boundary means this host, and this host's answer is already cached —
    // so there is one path for that case rather than two that could drift.
    if matches!(sandbox, crate::sandbox::Sandbox::Direct) {
        return of_this_host(repo_rev);
    }
    let (os, arch) = sandbox.platform();
    ComputeAttribution {
        repo_rev: named_rev(repo_rev.into()),
        os,
        arch,
        toolchain: toolchain_from(sandbox.capture(&["rustc", "--version"])),
        host: Server::Local,
    }
}

/// `rustc --version`, read once per process.
///
/// The COMPILER's identity, which is what [`ComputeAttribution::toolchain`]
/// asks for — deliberately not this crate's own version, which is a fact
/// about the source and would compare equal across two machines running
/// different compilers.
///
/// Cached because it cannot change under a running process and because the
/// donor loop asks per completed unit.
pub fn toolchain() -> &'static str {
    static TOOLCHAIN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TOOLCHAIN.get_or_init(|| {
        let read = std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
        toolchain_from(read)
    })
}

/// This host's attribution for work computed at `repo_rev`.
///
/// `os` and `arch` come from `std::env::consts`, which are compile-time
/// constants of the binary actually running — the honest answer to "what
/// machine ran this", and not I/O.
///
/// The caller supplies `repo_rev` because only it knows which tree the work
/// ran in: a donor resolves it from the unit's pin or its own workdir, a
/// submitter from the checkout it is gating. Pass [`ABSENT_REV`] when it
/// genuinely cannot be read.
pub fn of_this_host(repo_rev: impl Into<String>) -> ComputeAttribution {
    let repo_rev = repo_rev.into();
    ComputeAttribution {
        repo_rev: named_rev(repo_rev),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        toolchain: toolchain().to_string(),
        host: Server::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A READING BECOMES A FIELD IN ONE PLACE, and two of the three inputs
    /// are absences.** The subtle one is `Some("")`: a `rustc --version` that
    /// exits 0 and prints nothing is not a toolchain, and recording it as one
    /// would compare equal to any other host that did the same (§18.3). The
    /// failing input for the first assertion is a reader that trusts exit 0.
    #[test]
    fn only_a_non_empty_reading_is_a_toolchain() {
        assert_eq!(
            toolchain_from(Some("rustc 1.97.1 (c980f4866)".into())),
            "rustc 1.97.1 (c980f4866)"
        );
        assert_eq!(
            toolchain_from(Some("  rustc 1.95.0  \n".into())),
            "rustc 1.95.0"
        );
        assert_eq!(toolchain_from(None), ABSENT_TOOLCHAIN);
        assert_eq!(toolchain_from(Some("   ".into())), ABSENT_TOOLCHAIN);
        assert!(kernel_types::is_absent_marker(&toolchain_from(None)));
    }

    /// **WORK IN A BOUNDARY IS ATTRIBUTED TO THE BOUNDARY.** The runtime name
    /// cannot exist, so the compiler read fails and lands on the named absence
    /// — which is the honest answer and keeps the test hermetic. What is being
    /// asserted is the os and arch: they are the IMAGE's, and they disagree
    /// with this host on purpose. Failing input: `of_sandbox` falling through
    /// to `of_this_host`, which is what every donor did until 2026-09-10.
    #[test]
    fn work_that_ran_in_an_image_is_attributed_to_the_image() {
        let contained = crate::sandbox::Sandbox::Container {
            runtime: "no-such-container-runtime".into(),
            image: "example:latest".into(),
            os: "plan9".into(),
            arch: "sparc64".into(),
        };
        let a = of_sandbox("deadbeef", &contained);
        assert_eq!(a.os, "plan9");
        assert_eq!(a.arch, "sparc64");
        assert_ne!(a.os, std::env::consts::OS, "the host must not leak in");
        assert_eq!(a.repo_rev, "deadbeef");
        assert_eq!(
            a.toolchain, ABSENT_TOOLCHAIN,
            "a compiler that could not be read is named, never guessed"
        );

        // The control: no boundary means this process, and then the host IS
        // where the work ran.
        let direct = of_sandbox("deadbeef", &crate::sandbox::Sandbox::Direct);
        assert_eq!(direct.os, std::env::consts::OS);
        assert_eq!(direct.arch, std::env::consts::ARCH);
    }

    /// Both absence markers must be readable AS absences by the one decider,
    /// or the comparability rule they exist to trigger never fires.
    ///
    /// Failing input: either constant rewritten to a bare descriptive phrase
    /// like `"no rustc"`, which reads as a real value.
    #[test]
    fn both_absences_are_recognised_by_the_one_decider() {
        assert!(kernel_types::is_absent_marker(ABSENT_REV), "{ABSENT_REV}");
        assert!(
            kernel_types::is_absent_marker(ABSENT_TOOLCHAIN),
            "{ABSENT_TOOLCHAIN}"
        );
    }

    /// An empty rev is upgraded to a named absence rather than carried.
    ///
    /// Failing input: `""`. Carried through, it would make every host that
    /// could not read its rev compare equal to every other — the defect the
    /// named-absence vocabulary exists to prevent.
    #[test]
    fn an_empty_rev_becomes_a_named_absence() {
        for blank in ["", "   "] {
            let a = of_this_host(blank);
            assert_eq!(a.repo_rev, ABSENT_REV);
            assert!(kernel_types::is_absent_marker(&a.repo_rev));
        }
    }

    /// Two readings on THIS host agree, which is what makes the shared method
    /// usable as a reference — and they are still not comparable when the
    /// toolchain could not be read.
    #[test]
    fn two_readings_of_one_host_agree_unless_the_toolchain_is_absent() {
        let a = of_this_host("aaaa111");
        let b = of_this_host("aaaa111");
        assert_eq!(a, b, "the same host reads the same way twice");
        if kernel_types::is_absent_marker(&a.toolchain) {
            // No rustc here: identical values, and still not evidence about
            // each other. This is the arm the comparability rule added.
            assert!(!a.comparable_to(&b));
        } else {
            assert!(a.comparable_to(&b));
        }
    }

    /// A different rev is never comparable, which is the guard the donor pin
    /// depends on (cw-lift 5e bar iii).
    #[test]
    fn a_different_rev_is_not_comparable() {
        assert!(!of_this_host("aaaa111").comparable_to(&of_this_host("bbbb222")));
    }
}
