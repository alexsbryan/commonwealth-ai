// SPDX-License-Identifier: AGPL-3.0-or-later
//! How a host describes ITSELF when it reports work — one reader, three
//! callers.
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

/// The toolchain absence, in the same vocabulary.
pub const ABSENT_TOOLCHAIN: &str = "unknown (rustc is not on this host's PATH)";

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
        match std::process::Command::new("rustc").arg("--version").output() {
            Ok(out) if out.status.success() => {
                let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if v.is_empty() {
                    // Exit 0 with nothing on stdout is not a toolchain, and
                    // recording it as one would be a substitution.
                    tracing::warn!(
                        target: crate::TRACE_TARGET,
                        "work: `rustc --version` succeeded with empty output — reporting the toolchain as a named absence"
                    );
                    ABSENT_TOOLCHAIN.to_string()
                } else {
                    v
                }
            }
            _ => {
                tracing::warn!(
                    target: crate::TRACE_TARGET,
                    "work: `rustc --version` could not be read — reporting the toolchain as a named absence"
                );
                ABSENT_TOOLCHAIN.to_string()
            }
        }
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
        repo_rev: if repo_rev.trim().is_empty() {
            // An empty rev is the one value that must never survive: it
            // compares equal to every other unreadable host's.
            ABSENT_REV.to_string()
        } else {
            repo_rev
        },
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        toolchain: toolchain().to_string(),
        host: Server::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
