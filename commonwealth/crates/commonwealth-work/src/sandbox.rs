// SPDX-License-Identifier: AGPL-3.0-or-later
//! The boundary a `process:v1` unit runs inside, and the probe that decides
//! whether this build has one.
//!
//! # Why this is in the package and not in the daemon
//!
//! The isolation FLOOR came home to `commonwealth-work` on 2026-09-10 because
//! it had lived in `sovereign-mesh`'s boot decision and a donor built from
//! this crate alone therefore had none. The MECHANISM has the same
//! requirement for the same reason, and it is easy to get wrong in the same
//! way: the obvious image to reach for is the monorepo's own
//! (`sovereign/container/Containerfile`), the obvious registry is the
//! monorepo's `.cargo-container`, and a mechanism resting on either is one a
//! lifted peer cannot use. **This module assumes no repository.** It shells
//! out to a container runtime the HOST provides, into an image the DONOR
//! declares, and it knows nothing about what is being built.
//!
//! `scripts/cw-work-lift.sh --sandbox` is the falsification test and it is
//! already wired: it builds this crate's closure outside the monorepo with
//! nothing of it on the path. If anything here needs the monorepo, that lift
//! stays could-not-judge and says so without anyone having to notice.
//!
//! # Three parties, and only one of them may choose the image
//!
//! - The PACKAGE ships no image. It has no build system for one and no
//!   registry to fetch it from.
//! - The SUBMITTER must not choose it. A unit that names its own image
//!   chooses the contents of its own sandbox, and pulls an arbitrary
//!   reference onto somebody else's machine.
//! - The DONOR's operator declares it, beside the rest of their offer. It is
//!   their machine and their disk.
//!
//! # An offer describes where a unit RUNS, not who is hosting it
//!
//! Under a boundary the two differ, and every donor got this wrong until
//! 2026-09-10: `WorkOffer.os` / `.arch` were `std::env::consts`, so a
//! macOS/aarch64 laptop running a linux/amd64 image advertised `macos` and had
//! its Linux work refused on `UnmetRequirement::Os` — capable of the work,
//! describing itself as incapable. [`Sandbox::platform`] is now the one place
//! that answer comes from, and it is read off the image rather than assumed.
//!
//! # The declaration is still a probe, not a config key
//!
//! `[compute.work_offer]` supplying an image name is a PARAMETER. It is not
//! permission to claim isolation — [`Sandbox::probe`] still has to find a
//! runtime, confirm it is rootless, and confirm the image is present locally
//! before it will report [`Isolation::RootlessContainer`]. A config that
//! names an image on a host with no podman gets `Subprocess`, which offers
//! nothing, and the reason is on the boot trace. That is the §18.3 rule that
//! made `DONOR_ISOLATION` a constant in the first place: a donor that cannot
//! perform an isolation must not be able to assert it.

use std::ffi::OsStr;
use std::path::Path;

use oicp_types::Isolation;

/// The container runtimes this module knows how to drive, in preference
/// order. Both take the same flags for everything used below, which is why
/// there is one code path and not two.
const RUNTIMES: [&str; 2] = ["podman", "docker"];

/// How this build runs a unit's argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sandbox {
    /// Directly, as a child of the donor process — the donor's user, the
    /// donor's filesystem, the donor's network. `Isolation::Subprocess`, and
    /// the reason `process:v1` is not offerable on such a build.
    Direct,
    /// Inside a rootless container: no network, no capabilities, no
    /// new privileges, and nothing mounted but the unit's own workdir.
    Container {
        /// The runtime binary, resolved from `PATH` at probe time.
        runtime: String,
        /// The image the donor's operator declared.
        image: String,
        /// The image's OS, in Rust's spelling — the OS a unit will actually
        /// run on, which under a boundary is NOT this host's. Read off the
        /// image at probe time, never assumed.
        os: String,
        /// The image's architecture, in Rust's spelling. See [`rust_arch`]:
        /// the runtime reports OCI names and everything that compares this
        /// field speaks Rust's.
        arch: String,
    },
}

/// Why a probe did not reach a container. Each names the thing to fix; a
/// donor's boot trace prints it, because "this node offers nothing" with no
/// reason is the shape of a misconfiguration nobody finds (§18.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoSandbox {
    /// The offer declared no image, so there is nothing to run a unit in.
    #[error("[compute.work_offer] declares no `image`, and this package ships none — name an image this host already has, or this node cannot isolate a unit")]
    NoImageDeclared,
    /// Neither runtime is on `PATH`.
    #[error("no container runtime on PATH (looked for {})", RUNTIMES.join(", "))]
    NoRuntime,
    /// A runtime exists but is not rootless. Root-in-container on a rootful
    /// daemon is a different and much weaker claim, so it is refused rather
    /// than quietly accepted.
    #[error("`{runtime}` is not rootless on this host, and a rootful runtime is a weaker boundary than the one this would claim")]
    NotRootless { runtime: String },
    /// The image is not present locally. Deliberately NOT pulled: a pull is
    /// network I/O at boot, on the operator's machine, of a reference they
    /// may have typo'd.
    #[error("`{runtime}` does not have image `{image}` locally — pull it once yourself; a donor does not fetch images on your behalf")]
    ImageAbsent { runtime: String, image: String },
    /// The image is present and this host cannot EXECUTE it. Present and
    /// runnable are two facts, and reading the first as the second is how a
    /// donor comes to advertise work it will then fail (§18.3).
    #[error("`{runtime}` has image `{image}` but cannot run it here — {detail}. Present is not runnable: this is what an arch mismatch looks like (an arm64 image on an x86_64 host, or the reverse, with no emulation registered)")]
    ImageNotRunnable {
        runtime: String,
        image: String,
        detail: String,
    },
}

impl Sandbox {
    /// What this build PROVIDES. The one place that answer is derived.
    pub fn provides(&self) -> Isolation {
        match self {
            Sandbox::Direct => Isolation::Subprocess,
            Sandbox::Container { .. } => Isolation::RootlessContainer,
        }
    }

    /// Look for a usable boundary, given the image the donor declared.
    ///
    /// Every failure returns [`Sandbox::Direct`] WITH the reason, rather than
    /// an `Err` the caller might discard: a donor still boots and still runs
    /// the kinds it can isolate, it simply does not offer the ones it cannot.
    /// The reason exists so the boot trace can say which of four things to
    /// fix.
    pub fn probe(image: Option<&str>) -> (Sandbox, Option<NoSandbox>) {
        let Some(image) = image.filter(|i| !i.trim().is_empty()) else {
            return (Sandbox::Direct, Some(NoSandbox::NoImageDeclared));
        };
        let Some(runtime) = RUNTIMES.iter().find(|r| on_path(r)) else {
            return (Sandbox::Direct, Some(NoSandbox::NoRuntime));
        };
        let runtime = (*runtime).to_string();
        if !is_rootless(&runtime) {
            return (Sandbox::Direct, Some(NoSandbox::NotRootless { runtime }));
        }
        // ONE inspect answers both "is it local" and "what is it". Presence
        // and platform were two questions in one command already; asking them
        // apart would let a donor pass the first and guess the second.
        let Some((os, arch)) = image_platform(&runtime, image) else {
            return (
                Sandbox::Direct,
                Some(NoSandbox::ImageAbsent {
                    runtime,
                    image: image.to_string(),
                }),
            );
        };
        // THE FOURTH LEG, and it cost 0.1 s to add. Watched failing
        // 2026-09-10: an arm64 image pulled onto this x86_64 host passed every
        // check above — present, inspectable, platform `linux/aarch64` — so
        // the peer advertised `linux/aarch64` and then failed all three units
        // with `Exec format error`. An offer that cannot be honoured is worse
        // than no offer: it converts a refusal into three failed verdicts
        // about a program that never ran.
        if let Err(detail) = image_runs(&runtime, image) {
            return (
                Sandbox::Direct,
                Some(NoSandbox::ImageNotRunnable {
                    runtime,
                    image: image.to_string(),
                    detail,
                }),
            );
        }
        (
            Sandbox::Container {
                runtime,
                image: image.to_string(),
                os,
                arch,
            },
            None,
        )
    }

    /// The platform a unit will actually RUN on — the IMAGE's under a
    /// boundary, this host's without one.
    ///
    /// **This is what a donor must advertise, and until 2026-09-10 every
    /// donor advertised its own process instead** (`std::env::consts`, in
    /// `work_donor::resolve_offer`'s caller and in the lifted peer). The
    /// failing input is a whole machine: a macOS/aarch64 host running a
    /// linux/amd64 image can run a Linux-submitted unit perfectly, and
    /// `may_take` refused it on `UnmetRequirement::Os` — because
    /// `JobRequirements::accepts_host` is string equality against what the
    /// offer SAYS, and what it said was the donor's own kernel. The container
    /// changed what a unit runs in without changing what the donor claimed to
    /// be, which is the same substitution as claiming an isolation you cannot
    /// perform (§18.3) pointed the other way.
    pub fn platform(&self) -> (String, String) {
        match self {
            Sandbox::Direct => (
                std::env::consts::OS.to_string(),
                std::env::consts::ARCH.to_string(),
            ),
            Sandbox::Container { os, arch, .. } => (os.clone(), arch.clone()),
        }
    }

    /// The argv that actually gets spawned, for a unit whose own argv is
    /// `argv` and whose workdir is `cwd`.
    ///
    /// **The hardening is here and not in a config**, so a donor cannot be
    /// talked into a weaker boundary while still claiming this one:
    ///
    /// - `--entrypoint=""`. THE UNIT'S ARGV IS THE UNIT. An image with an
    ///   `ENTRYPOINT` would otherwise run ITS program with the unit's argv
    ///   handed over as arguments — so the donor would execute something the
    ///   submitter never sealed and report a verdict about it. Measured
    ///   2026-09-10 on the first sandboxed lift run: three units, three
    ///   exit-127s, and a `failed` verdict for a program that never ran
    ///   (§18.3 — the substitution has to be impossible, not documented).
    /// - `--network=none`. The posture that is verifiable rather than
    ///   asserted — a unit's failure to open a socket is a thing you can
    ///   watch. A kind that genuinely needs egress has to say so and does not
    ///   have a way to yet, which is the honest state.
    /// - `--cap-drop=ALL` and `--security-opt=no-new-privileges`.
    /// - NOTHING is mounted but `cwd`. The donor's data directory — which
    ///   holds its mesh private key — is not reachable by construction rather
    ///   than by an exclusion somebody has to maintain.
    /// - `--userns=keep-id`, so files the unit writes into its workdir belong
    ///   to the donor's user afterwards rather than to a mapped root.
    /// - `--rm`, because a donor that accumulates stopped containers is a
    ///   donor that fills a disk it does not own.
    ///
    /// `env` is passed with `--env`, and the caller has already ordered it so
    /// the colour normalization lands last.
    pub fn command_line<'a>(
        &self,
        argv: &'a [String],
        cwd: &Path,
        env: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<String> {
        match self {
            Sandbox::Direct => argv.to_vec(),
            Sandbox::Container { runtime, image, .. } => {
                let mut out = vec![
                    runtime.clone(),
                    "run".into(),
                    "--rm".into(),
                    "--network=none".into(),
                    "--cap-drop=ALL".into(),
                    "--security-opt=no-new-privileges".into(),
                    "--userns=keep-id".into(),
                    // Empty, not absent: absent means "keep the image's",
                    // and the image's is the one thing here the donor's
                    // operator picked but did not write.
                    "--entrypoint=".into(),
                ];
                // The workdir is the ONLY thing that crosses. `:Z` asks the
                // runtime to relabel for SELinux; on a host without it the
                // suffix is ignored rather than an error.
                out.push("-v".into());
                out.push(format!("{}:/work:Z", cwd.display()));
                out.push("-w".into());
                out.push("/work".into());
                for (k, v) in env {
                    out.push("--env".into());
                    out.push(format!("{k}={v}"));
                }
                out.push(image.clone());
                out.extend(argv.iter().cloned());
                out
            }
        }
    }
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// `podman info` and `docker info` both answer this, in different shapes, so
/// the check is on the substring both emit rather than on a parsed document —
/// this module refuses to grow a JSON dependency for one boolean.
fn is_rootless(runtime: &str) -> bool {
    run_ok(runtime, ["info", "--format", "{{.Host.Security.Rootless}}"])
        .map(|out| out.trim() == "true")
        .unwrap_or(false)
}

/// The image's `(os, arch)` in Rust's spelling, or `None` when the image is
/// not present locally.
///
/// `image exists` is podman's; docker has no equivalent verb, so the portable
/// form is `image inspect`, which both accept and which touches no network.
fn image_platform(runtime: &str, image: &str) -> Option<(String, String)> {
    let out = run_ok(
        runtime,
        ["image", "inspect", "--format", "{{.Os}} {{.Architecture}}", image],
    )?;
    let mut parts = out.split_whitespace();
    let os = parts.next()?.to_string();
    let arch = rust_arch(parts.next()?);
    Some((os, arch))
}

/// An OCI architecture name in Rust's spelling.
///
/// **THE FAILING INPUT IS A SPELLING, and it refuses everything.** A runtime
/// reports `amd64`; `std::env::consts::ARCH` is `x86_64`; and
/// `JobRequirements::accepts_host` compares the two with `==`. So an
/// untranslated platform makes a perfectly capable donor refuse every unit
/// with `UnmetRequirement::Arch { required: "x86_64", host: "amd64" }` — a
/// donor that looks broken because two vocabularies met. One translation, in
/// one place (§10.6).
///
/// An unknown name passes through rather than defaulting to anything: a wrong
/// arch that compares equal is worse than one that compares unequal and says
/// what it saw (§18.3).
fn rust_arch(oci: &str) -> String {
    match oci {
        "amd64" => "x86_64",
        "arm64" => "aarch64",
        "386" => "x86",
        "ppc64le" => "powerpc64",
        "riscv64" => "riscv64",
        other => other,
    }
    .to_string()
}

/// Can this host EXECUTE that image? `Ok(())` or the runtime's own complaint.
///
/// One container, `--rm`, no network, running `true`. The cost is 0.1 s
/// measured, paid once at boot, and it is the difference between an image a
/// donor HAS and an image a donor can USE.
fn image_runs(runtime: &str, image: &str) -> Result<(), String> {
    let out = std::process::Command::new(runtime)
        .args(["run", "--rm", "--entrypoint=", "--network=none", image, "true"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("`{runtime} run` could not be spawned: {e}"))?;
    if runnable(out.status.code()) {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find(|l| l.contains("rror"))
        .or_else(|| stderr.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or("the runtime said nothing")
        .trim();
    Err(line.chars().take(160).collect())
}

/// Does that exit code mean the image ran?
///
/// **127 counts, and leaving it out would be a false refusal.** Measured on
/// this host: an image whose platform cannot execute exits **1** with `exec
/// container process: Exec format error`, while a runnable image missing the
/// command exits **127** from the OCI runtime — the container STARTED, which
/// is the question being asked. A single-static-binary image with no coreutils
/// must not be refused for lacking `true`.
fn runnable(code: Option<i32>) -> bool {
    matches!(code, Some(0) | Some(127))
}

/// Run a probe command and hand back its stdout if it exited 0. Blocking and
/// on purpose: `probe` runs once, at boot, before anything is published.
fn run_ok<I, S>(runtime: &str, args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = std::process::Command::new(runtime)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE DECLARATION IS NOT THE CAPABILITY (ARCH §18.3).**
    ///
    /// The failing input is the one an operator will actually produce: a
    /// config naming an image on a host where the probe cannot confirm a
    /// runtime. It must report `Direct` — and therefore offer nothing —
    /// rather than take the config's word for it. A `Sandbox::Container`
    /// built from a config alone would be a claim with no mechanism, which is
    /// what `DONOR_ISOLATION` was a constant to prevent.
    #[test]
    fn an_absent_image_declaration_is_direct_and_says_which() {
        let (sandbox, why) = Sandbox::probe(None);
        assert_eq!(sandbox, Sandbox::Direct);
        assert_eq!(sandbox.provides(), Isolation::Subprocess);
        assert_eq!(why, Some(NoSandbox::NoImageDeclared));

        // Blank is the same fact spelled differently, and a config file is
        // exactly where a blank string comes from.
        let (sandbox, why) = Sandbox::probe(Some("   "));
        assert_eq!(sandbox, Sandbox::Direct);
        assert_eq!(why, Some(NoSandbox::NoImageDeclared));
    }

    /// The two isolations this module can report, and the ladder position of
    /// each. The control on the test above: if `provides` returned
    /// `Subprocess` for both, that test would pass while the mechanism was
    /// dead.
    #[test]
    fn a_container_provides_more_than_a_direct_child() {
        let contained = Sandbox::Container {
            runtime: "podman".into(),
            image: "example:latest".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        assert_eq!(contained.provides(), Isolation::RootlessContainer);
        assert!(contained.provides().covers(Isolation::RootlessContainer));
        assert!(!Sandbox::Direct
            .provides()
            .covers(Isolation::RootlessContainer));
    }

    /// **THE HARDENING IS THE POINT, so it is asserted rather than trusted to
    /// a code review.** The failing input for each of these is a dropped
    /// flag, and the one that matters most is `--network=none`: without it
    /// every other flag still passes review and a unit still reaches the
    /// internet from the donor's address.
    #[test]
    fn the_container_line_carries_every_flag_the_boundary_depends_on() {
        let s = Sandbox::Container {
            runtime: "podman".into(),
            image: "example:latest".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        let argv = vec!["sh".to_string(), "-c".to_string(), "echo hi".to_string()];
        let line = s.command_line(&argv, Path::new("/tmp/unit-workdir"), [("NO_COLOR", "1")]);

        for flag in [
            "--entrypoint=",
            "--network=none",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--rm",
        ] {
            assert!(line.iter().any(|a| a == flag), "missing {flag} in {line:?}");
        }
        assert_eq!(line[0], "podman");
        assert!(
            line.iter().any(|a| a == "/tmp/unit-workdir:/work:Z"),
            "the workdir must be the only mount, and it must be mounted: {line:?}"
        );
        assert_eq!(
            line.iter().filter(|a| *a == "-v").count(),
            1,
            "exactly one mount — a second one is how the donor's data dir, and \
             its mesh private key, would get in: {line:?}"
        );
        assert!(
            line.iter().any(|a| a == "NO_COLOR=1"),
            "the unit's environment must still reach it: {line:?}"
        );
        // The image separates the runtime's flags from the unit's argv, and
        // everything after it is the unit's. A flag appended after the image
        // would be silently handed to the unit instead of to the runtime.
        let at = line
            .iter()
            .position(|a| a == "example:latest")
            .expect("image");
        assert_eq!(&line[at + 1..], &argv[..], "the unit's argv comes last");
    }

    /// **A CONTAINED DONOR ADVERTISES THE IMAGE, NOT ITSELF.** The failing
    /// input is the machine this was found on: a macOS/aarch64 host with a
    /// linux/amd64 image can run a Linux-submitted unit and was refused on
    /// `Os`, because the offer described the donor's kernel. So this asserts
    /// the platform follows the image even when it disagrees with the host —
    /// which it does here on purpose, whichever host runs the test.
    #[test]
    fn a_contained_donor_advertises_the_images_platform_not_the_hosts() {
        let elsewhere = Sandbox::Container {
            runtime: "podman".into(),
            image: "example:latest".into(),
            os: "plan9".into(),
            arch: "sparc64".into(),
        };
        assert_eq!(
            elsewhere.platform(),
            ("plan9".to_string(), "sparc64".to_string()),
            "the offer must describe the image; nothing here may fall back to this host"
        );
        assert_ne!(elsewhere.platform().0, std::env::consts::OS);

        // The control: with no boundary there is no image, and the host IS
        // where the unit runs. A `platform` that returned the host in both
        // arms would pass the test above only by accident of the fixture.
        assert_eq!(
            Sandbox::Direct.platform(),
            (
                std::env::consts::OS.to_string(),
                std::env::consts::ARCH.to_string()
            )
        );
    }

    /// **THE TRANSLATION, TIED TO THE REAL CONSTANT.** A runtime says `amd64`
    /// where Rust says `x86_64`, and `accepts_host` compares them with `==` —
    /// so an untranslated name refuses every unit and names a spelling as the
    /// reason. The last assertion is the one that cannot rot: on whichever
    /// machine runs it, the OCI name for THIS host must translate to this
    /// host's own `ARCH`.
    #[test]
    fn an_oci_arch_name_is_translated_because_a_spelling_refuses_everything() {
        assert_eq!(rust_arch("amd64"), "x86_64");
        assert_eq!(rust_arch("arm64"), "aarch64");
        // Unknown passes through: an arch that compares unequal and says what
        // it saw beats one that was defaulted into comparing equal (§18.3).
        assert_eq!(rust_arch("loongarch64"), "loongarch64");

        let oci_for_this_host = match std::env::consts::ARCH {
            "x86_64" => Some("amd64"),
            "aarch64" => Some("arm64"),
            _ => None,
        };
        if let Some(oci) = oci_for_this_host {
            assert_eq!(
                rust_arch(oci),
                std::env::consts::ARCH,
                "an image built for this very host must advertise as this host"
            );
        }
    }

    /// **PRESENT IS NOT RUNNABLE, and 127 is the line between two failures.**
    /// The failing input was watched rather than reasoned: an arm64 image on
    /// this x86_64 host exits 1 with `Exec format error`, so 1 must refuse.
    /// 127 comes from the OCI runtime AFTER the container starts, so it must
    /// not — refusing it would turn every single-binary image into a donor
    /// that offers nothing and blames its own coreutils.
    #[test]
    fn only_a_container_that_started_counts_as_runnable() {
        assert!(runnable(Some(0)), "it ran");
        assert!(
            runnable(Some(127)),
            "the container started and the image has no `true` — that answers the platform question"
        );
        assert!(!runnable(Some(1)), "1 is `Exec format error` on this host");
        assert!(!runnable(Some(125)), "the runtime refused the container");
        assert!(!runnable(None), "killed by a signal is not a yes");
    }

    /// A direct run is the unit's own argv and nothing else — no wrapper, no
    /// surprise. The failing input would be a `Direct` that still prefixed a
    /// runtime, which would break every host without one.
    #[test]
    fn a_direct_run_is_the_units_own_argv() {
        let argv = vec!["true".to_string()];
        assert_eq!(
            Sandbox::Direct.command_line(&argv, Path::new("/tmp"), []),
            argv
        );
    }
}
