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
        if !has_image(&runtime, image) {
            return (
                Sandbox::Direct,
                Some(NoSandbox::ImageAbsent {
                    runtime,
                    image: image.to_string(),
                }),
            );
        }
        (
            Sandbox::Container {
                runtime,
                image: image.to_string(),
            },
            None,
        )
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
            Sandbox::Container { runtime, image } => {
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

fn has_image(runtime: &str, image: &str) -> bool {
    // `image exists` is podman's; docker has no equivalent verb, so the
    // portable form is `image inspect`, which both accept and which touches
    // no network.
    run_ok(runtime, ["image", "inspect", image]).is_some()
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
