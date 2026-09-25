// SPDX-License-Identifier: AGPL-3.0-or-later
//! Is a serving host reachable — and, if this build ships a backend, bring
//! one up.
//!
//! # Why this is the client's job
//!
//! sv-surface's `sv-no-daemon-management` bar (`quality/campaigns/
//! sv-surface.toml`), as the operator revised it on 2026-09-11:
//!
//! > "The daemon is the daemon and the desktop client is the client. Think
//! > of it like an api layer that ships a client package to handle
//! > networking. The app still does nothing daemon related. It invokes
//! > something and moves on."
//!
//! An application calls the client and moves on; establishing reachability
//! is the client's concern the way connection setup is the driver's and not
//! the caller's. The honest precedent is a language-server client or an
//! embedded database driver, **not** an HTTP API client — `libpq` does not
//! start Postgres — so the capability is NAMED rather than smuggled in as a
//! networking detail. It is the `bundled-backend` cargo feature, off by
//! default, and [`CAN_BRING_UP_A_BACKEND`] is the value that reports it.
//!
//! # What the feature asks
//!
//! "Does this build ship a backend it can bring up?" — a question about the
//! BUILD, never about the platform. A phone flips it on the day a model fits
//! (ARCH principle 12: a phone running on-device inference is a client whose daemon
//! is colocated, not an exception). Omitting it from one platform's build
//! would encode today's deployment as an invariant.
//!
//! # What this is NOT
//!
//! Not a supervisor. [`ServingHost::ensure_reachable`] brings a backend up
//! at most once per call, drops the `Child` at the spawn site, and returns a
//! pid as a *fact* rather than a handle. No restart policy, no health loop,
//! no shutdown budget — the daemon owns its own lifetime (ARCH principle 12), and a
//! client that held any of those three would be managing a daemon again.
//! The bar's instrument has two halves for exactly this reason: a
//! dependency-edge gate stays green while supervision is re-added one retry
//! at a time, so the census asks separately whether any surface retains a
//! process handle.
//!
//! On unix the brought-up process stays this process's child (it is put in
//! its own process group, so a Ctrl-C in the launching terminal does not
//! take it down, and it survives our exit by reparenting to init). Because
//! nothing retains the handle, nothing reaps it: a backend that exits while
//! we live leaves one zombie entry until we exit. That is the price of not
//! holding a handle, and it is the right side of the trade — the alternative
//! is a `try_wait` loop, which is a lifecycle this client must not track.
//!
//! # One decider
//!
//! "Is the daemon answering?" was written thirteen times before this module
//! — `git grep -nE 'fn (probe_daemon|daemon_reachable|wait_for_daemon|
//! wait_for_ready)\b'` — plus an inline loop in
//! `sovereign-desktop/src-tauri/src/attach_watch.rs:53`. Every one is a
//! `GET /v1/models`, with per-probe timeouts spread from 500 ms to 5 s and
//! no shared answer to "how long do I wait for a cold model load". This is
//! where that lives now (ARCH principle 8); the copies retire onto it as their
//! crates reach for it.

use std::time::{Duration, Instant};

/// How long one probe may take before it counts as "not answering".
///
/// 2 s is what the surfaces that wait on a *starting* daemon already use
/// (`attach_watch.rs:29`, `setup_cmd/finish.rs:525`); the 500 ms end of the
/// range belongs to callers that only wanted a liveness answer and can get
/// one from a short [`ServingHost::ensure_reachable`] window instead.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Gap between probes while waiting for a host to answer. The same 250 ms
/// `sovereign-cli-daemon/src/daemon_cmd/lifecycle.rs:1165` has polled at
/// since the daemon grew a readiness wait, so a surface that migrates onto
/// this module sees no change in behaviour.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The readiness endpoint, and the DEFAULT — see [`ServingHost::ready_at`]
/// for the backends that answer a different door.
///
/// Any 2xx means serving — the contract `attach_watch.rs`, the supervisor
/// heartbeat and `daemon start`'s readiness wait have all used since they
/// were written. This daemon has no `/healthz` (it 404s); `/v1/models` and
/// `/status` are the liveness doors.
pub const DEFAULT_READY_PATH: &str = "/v1/models";

/// Does this build ship a backend it can bring up?
///
/// The whole question the `bundled-backend` feature asks, as a value a
/// caller can log, render or assert on — so "this build cannot start one"
/// is *reported* rather than inferred from a failure (ARCH principle 6).
pub const CAN_BRING_UP_A_BACKEND: bool = cfg!(feature = "bundled-backend");

/// A serving host a surface wants reachable, and the one place that decides
/// whether it is.
///
/// Construct with [`ServingHost::at`]; in a build with the `bundled-backend`
/// feature, [`ServingHost::bringing_up`] names the backend this client may
/// start if nothing answers. Cheap to build and cheap to drop — it holds a
/// base URL and an HTTP client, never a process.
#[derive(Debug, Clone)]
pub struct ServingHost {
    base: String,
    /// The path a probe asks. [`DEFAULT_READY_PATH`] unless a caller named another
    /// with [`ServingHost::ready_at`].
    ready_path: String,
    /// How long one probe may take. [`PROBE_TIMEOUT`] unless a caller named
    /// another with [`ServingHost::probe_timeout`].
    probe_timeout: Duration,
    http: reqwest::Client,
    #[cfg(feature = "bundled-backend")]
    backend: Option<BundledBackend>,
}

/// What [`ServingHost::ensure_reachable`] found, or did.
///
/// A value rather than only a log line: which of the two happened decides
/// what a surface should say to its user (ARCH principle 1), and a caller that
/// wants to assert "we started nothing" needs to be able to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reached {
    /// A host this client did not start is answering. `waited` is zero when
    /// it answered on the first probe, and non-zero when it was already
    /// coming up — a service manager's daemon mid-start, or a peer's.
    AlreadyServing { waited: Duration },
    /// This build brought one up by path, and it answered. `pid` is a fact
    /// for logs and diagnostics, not a handle: nothing here can signal,
    /// wait on or restart it.
    BroughtUp { pid: u32, ready_after: Duration },
}

/// Why no host is reachable. Every variant names what was missing, because
/// an unreachable backend reported as a default (an empty model list, a
/// silent local fallback) is the failure ARCH principle 6 exists to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotReachable {
    /// Nothing answered, and this build ships no backend it can bring up.
    /// Only constructible where the `bundled-backend` feature is off.
    NoBackendInThisBuild { base: String, waited: Duration },
    /// Nothing answered; this build can bring one up but was given none —
    /// the caller never called [`ServingHost::bringing_up`].
    NoBackendConfigured { base: String, waited: Duration },
    /// The bring-up failed before a process existed.
    LaunchFailed { program: String, reason: String },
    /// A backend was brought up and never answered inside the window. The
    /// process is still running; this client does not stop it, because it
    /// does not own it.
    SilentAfterLaunch {
        pid: u32,
        base: String,
        waited: Duration,
    },
}

impl std::fmt::Display for NotReachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoBackendInThisBuild { base, waited } => write!(
                f,
                "no serving host answered at {base} after {:.1}s, and this build ships no backend it can bring up",
                waited.as_secs_f32()
            ),
            Self::NoBackendConfigured { base, waited } => write!(
                f,
                "no serving host answered at {base} after {:.1}s, and no backend was configured to bring one up",
                waited.as_secs_f32()
            ),
            Self::LaunchFailed { program, reason } => {
                write!(f, "could not bring up {program}: {reason}")
            }
            Self::SilentAfterLaunch { pid, base, waited } => write!(
                f,
                "brought up pid {pid} but {base} did not answer within {:.1}s (a cold model load can take 30-60s)",
                waited.as_secs_f32()
            ),
        }
    }
}

impl std::error::Error for NotReachable {}

impl ServingHost {
    /// `base` is the host root, e.g. `http://127.0.0.1:9741` — no `/v1`,
    /// the same spelling [`crate::TurnClient::new`] takes.
    pub fn at(base: impl Into<String>) -> Self {
        let mut base = base.into();
        while base.ends_with('/') {
            base.pop();
        }
        Self {
            base,
            ready_path: DEFAULT_READY_PATH.to_string(),
            probe_timeout: PROBE_TIMEOUT,
            http: reqwest::Client::new(),
            #[cfg(feature = "bundled-backend")]
            backend: None,
        }
    }

    /// The base URL this host is asked for.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Probe a different readiness door than [`DEFAULT_READY_PATH`].
    ///
    /// Not every backend a surface brings up is a svrn daemon.
    /// `sovereign-server` — the phone-facing mobile host — serves no
    /// `/v1/models` at all; its unauthenticated liveness door is `/health`
    /// (`sovereign-server/src/main.rs:778`), and probing the default against
    /// it returns 404 forever, which this module would report as "brought up
    /// a backend that has not answered" for a host that is serving fine.
    ///
    /// The alternative was a private probe loop in the mobile toggle, which
    /// is the fourteenth copy of "is it answering" this module exists to
    /// retire (ARCH principle 8). A path is a parameter; a second
    /// implementation is a decider.
    pub fn ready_at(mut self, path: impl Into<String>) -> Self {
        self.ready_path = path.into();
        self
    }

    /// Give one probe longer than [`PROBE_TIMEOUT`] before it counts as not
    /// answering. For a caller that probes ONCE and then tells a person
    /// something: a daemon busy loading a model answers late, and late is not
    /// absent (`svrn mesh grant` asks with 3 s). A caller that can wait
    /// should use a [`ServingHost::ensure_reachable`] window instead.
    pub fn probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// Name the backend this client may bring up if nothing answers.
    ///
    /// Only exists in a build with the `bundled-backend` feature — which is
    /// the point of the feature: where it is off there is no way to give a
    /// client this ability, and [`ServingHost::ensure_reachable`] can only
    /// wait and report.
    #[cfg(feature = "bundled-backend")]
    pub fn bringing_up(mut self, backend: BundledBackend) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Is a host answering right now? One probe, [`PROBE_TIMEOUT`] at most.
    pub async fn is_serving(&self) -> bool {
        self.probe().await.is_ok()
    }

    /// The same one probe as [`ServingHost::is_serving`], with the REASON it
    /// did not count as serving — for a caller that has to tell a person
    /// something. "No daemon" is the wrong sentence for a refused
    /// connection, an answer slower than the timeout, or a listener that is
    /// not this daemon (a non-2xx on the ready door); each comes back in its
    /// own words. `is_serving` is this collapsed to a bool, so the two can
    /// never disagree about what serving means.
    pub async fn probe(&self) -> Result<(), String> {
        let url = format!("{}{}", self.base, self.ready_path);
        match self.http.get(&url).timeout(self.probe_timeout).send().await {
            Ok(resp) => {
                let status = resp.status();
                tracing::debug!(base = %self.base, path = %self.ready_path, %status, "reach: probe answered");
                if status.is_success() {
                    Ok(())
                } else {
                    Err(format!(
                        "{url} answered {status} — something is listening, but it is not a serving daemon"
                    ))
                }
            }
            Err(e) => {
                tracing::debug!(base = %self.base, path = %self.ready_path, error = %e, "reach: probe did not answer");
                Err(format!("the daemon at {url} did not answer: {e}"))
            }
        }
    }

    /// Poll until a host answers or `within` elapses. `true` means one is
    /// serving; this starts nothing.
    pub async fn wait_until_serving(&self, within: Duration) -> bool {
        let started = Instant::now();
        loop {
            if self.is_serving().await {
                return true;
            }
            if started.elapsed() >= within {
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Ensure a host is reachable: probe, bring one up if this build ships
    /// one and none is answering, then wait for it inside `within`.
    ///
    /// `within` is the whole budget, spanning the probe, the bring-up and
    /// the readiness wait. Size it for what the backend has to do — a cold
    /// model load is 30-60 s on this workspace's default weights, which is
    /// why `daemon start` waits far longer than a liveness probe would.
    ///
    /// Two callers racing to bring one up is the daemon's question, not
    /// this client's: a svrn daemon takes
    /// [`sovereign_contracts::run_lock::RunLock`] on its data root, so the
    /// loser exits and the winner serves both. Adding a lock here would be
    /// this client deciding something the daemon already owns (ARCH
    /// principle 12).
    pub async fn ensure_reachable(&self, within: Duration) -> Result<Reached, NotReachable> {
        let started = Instant::now();

        if self.is_serving().await {
            tracing::info!(base = %self.base, "reach: a host is already serving");
            return Ok(Reached::AlreadyServing {
                waited: started.elapsed(),
            });
        }

        #[cfg(feature = "bundled-backend")]
        if let Some(backend) = self.backend.as_ref() {
            tracing::info!(
                base = %self.base,
                program = %backend.program().display(),
                "reach: nothing answered — bringing up the bundled backend",
            );
            let pid = backend.bring_up().map_err(|reason| {
                tracing::warn!(
                    program = %backend.program().display(),
                    %reason,
                    "reach: bring-up failed",
                );
                NotReachable::LaunchFailed {
                    program: backend.program().display().to_string(),
                    reason,
                }
            })?;
            let remaining = within.saturating_sub(started.elapsed());
            return if self.wait_until_serving(remaining).await {
                let ready_after = started.elapsed();
                tracing::info!(
                    base = %self.base,
                    pid,
                    ready_ms = ready_after.as_millis() as u64,
                    "reach: brought up a backend and it is serving",
                );
                Ok(Reached::BroughtUp { pid, ready_after })
            } else {
                let waited = started.elapsed();
                tracing::warn!(
                    base = %self.base,
                    pid,
                    waited_ms = waited.as_millis() as u64,
                    "reach: brought up a backend that has not answered",
                );
                Err(NotReachable::SilentAfterLaunch {
                    pid,
                    base: self.base.clone(),
                    waited,
                })
            };
        }

        // Nothing to bring up — but something else may be starting one (a
        // launchd job, a systemd unit, an operator's `svrn daemon start`),
        // so the window is still worth spending before reporting absence.
        let remaining = within.saturating_sub(started.elapsed());
        if self.wait_until_serving(remaining).await {
            let waited = started.elapsed();
            tracing::info!(
                base = %self.base,
                waited_ms = waited.as_millis() as u64,
                "reach: a host we did not start came up",
            );
            return Ok(Reached::AlreadyServing { waited });
        }

        let waited = started.elapsed();
        let err = if CAN_BRING_UP_A_BACKEND {
            NotReachable::NoBackendConfigured {
                base: self.base.clone(),
                waited,
            }
        } else {
            NotReachable::NoBackendInThisBuild {
                base: self.base.clone(),
                waited,
            }
        };
        tracing::warn!(base = %self.base, reason = %err, "reach: unreachable");
        Err(err)
    }
}

/// Find the program named `bin` that a client brings up or execs.
///
/// A client locates the program it reaches (ARCH principle 12), so the one
/// order lives here: `env_var` if it names a file, then `bin` beside the
/// canonical `current_exe()`, then `PATH`. Seven copies of this body lived in
/// `sovereign-cli`, `sovereign-cli-daemon` and `serve_cmd` before it.
/// Not feature-gated: exec'ing a sibling is not bringing up a backend.
pub fn locate_sibling(bin: &str, env_var: &str) -> Option<std::path::PathBuf> {
    locate_from(
        bin,
        std::env::var_os(env_var),
        std::env::current_exe().ok(),
        std::env::var_os("PATH"),
    )
}

/// [`locate_sibling`] with its three inputs supplied, so a test can pin the
/// order without mutating the process environment.
fn locate_from(
    bin: &str,
    env_override: Option<std::ffi::OsString>,
    exe: Option<std::path::PathBuf>,
    path: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    if let Some(p) = env_override.map(std::path::PathBuf::from) {
        if p.is_file() {
            tracing::debug!(bin, found = %p.display(), "locate_sibling: env override");
            return Some(p);
        }
    }
    if let Some(cand) = exe
        .and_then(|e| std::fs::canonicalize(e).ok())
        .and_then(|real| real.parent().map(|d| d.join(bin)))
    {
        if cand.is_file() {
            tracing::debug!(bin, found = %cand.display(), "locate_sibling: beside current_exe");
            return Some(cand);
        }
    }
    let found = which::which_in_global(bin, path)
        .ok()
        .and_then(|mut hits| hits.next());
    tracing::debug!(bin, found = ?found, "locate_sibling: PATH");
    found
}

/// A backend binary this build ships and may bring up.
///
/// The client does not decide the path — a surface knows where its sidecar
/// was installed and passes it in. Supplying a path grants no ownership
/// (ARCH principle 12: "calling something is not owning it"); what would grant it is
/// a handle, a retry or a dependency, and this type hands back none of them.
#[cfg(feature = "bundled-backend")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledBackend {
    program: std::path::PathBuf,
    args: Vec<String>,
    log_to: Option<std::path::PathBuf>,
}

#[cfg(feature = "bundled-backend")]
impl BundledBackend {
    /// `program` should be an ABSOLUTE path. A sidecar copied to a stable
    /// location beats one addressed inside a bundle: a path into a `.app`
    /// breaks the moment the user moves the app.
    pub fn at(program: impl Into<std::path::PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            log_to: None,
        }
    }

    /// Append one argument, e.g. `.arg("daemon").arg("run")`.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append stdout and stderr to this file instead of discarding them.
    ///
    /// Worth passing on every real bring-up: a detached process's output
    /// goes nowhere by default, and a backend that fails to start is then
    /// invisible — the failure ARCH principle 1 calls a decision you cannot see.
    pub fn log_to(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.log_to = Some(path.into());
        self
    }

    /// The binary this would bring up.
    pub fn program(&self) -> &std::path::Path {
        &self.program
    }

    /// Start it, detached, and drop the handle. Returns the pid as a fact.
    ///
    /// The detach is what makes this a bring-up rather than a supervision:
    /// on unix a new process group (so the launching terminal's Ctrl-C does
    /// not reach it — `daemon_cmd/lifecycle.rs:748` is the precedent this
    /// matches), on Windows `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`
    /// (no inherited console, no Ctrl-C group). Tauri's own sidecar spawn
    /// sets neither, which is why this does not use it.
    fn bring_up(&self) -> Result<u32, String> {
        use std::process::{Command, Stdio};

        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).stdin(Stdio::null());

        match self.log_to.as_ref() {
            Some(path) => {
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|e| format!("open log {}: {e}", path.display()))?;
                let err = file
                    .try_clone()
                    .map_err(|e| format!("clone log handle: {e}"))?;
                cmd.stdout(Stdio::from(file)).stderr(Stdio::from(err));
            }
            None => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }

        let child = cmd.spawn().map_err(|e| e.to_string())?;
        let pid = child.id();
        // The whole bar in one line: the handle dies here. Holding it is
        // how a client becomes a supervisor one `try_wait` at a time.
        drop(child);
        Ok(pid)
    }
}

#[cfg(test)]
#[path = "reach/tests.rs"]
mod tests;
