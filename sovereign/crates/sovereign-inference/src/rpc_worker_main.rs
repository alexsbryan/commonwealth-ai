// SPDX-License-Identifier: AGPL-3.0-or-later
//! Entrypoint for the out-of-process ggml RPC worker — `current_exe()
//! --rpc-worker --bind <addr> [--cache-dir <path>] [--threads N]`.
//!
//! # Why this process exists
//!
//! The ggml RPC protocol is the ONE surface in this workspace that feeds
//! peer-supplied bytes into llama.cpp, and ggml enforces its bounds with
//! `GGML_ASSERT` — which expands to `ggml_abort()` unconditionally, with no
//! `NDEBUG` guard (`ggml.h:288`). It is not a rejected message; it is
//! `abort()`. Two of those sites take attacker-shaped input directly:
//! `deserialize_tensor`'s buffer-bounds check (`ggml-rpc.cpp:1103`) and
//! `graph_compute`'s `GGML_ASSERT(status == GGML_STATUS_SUCCESS)`
//! (`:1468`) — the second needs no malformed message at all, only a graph
//! large enough to fail allocation.
//!
//! Served from a thread inside the daemon, either one kills the process
//! holding the mesh secret key, the secret store and the conversation
//! database. Served from here, it kills a child that owns none of those and
//! that the supervisor in [`crate::embedded`] re-spawns.
//!
//! Upstream says the same thing in fewer words
//! (`llama.cpp/tools/rpc/README.md`): *"the functionality is fragile and
//! insecure."*
//!
//! # What this is NOT
//!
//! Not upstream's `ggml-rpc-server` binary. That one derives its tensor-cache
//! directory internally (`$LLAMA_CACHE/llama.cpp/rpc/`) from a boolean `-c`
//! flag, so it cannot be pointed at `~/.svrnmesh/rpc-cache` where
//! [`crate::embedded::warm_cache_from_gguf`] writes its shards — adopting it
//! would silently disable offline pre-warming. Re-execing our own binary
//! costs no packaging change (the release ships three binaries and none of
//! them is a llama.cpp tool) and keeps exact parameter parity, because this
//! calls the identical `ggml_backend_rpc_start_server` the in-process path
//! calls.

use std::time::Instant;

/// Parsed `--rpc-worker` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcWorkerArgs {
    /// The bind address, exactly as the daemon resolved it from
    /// `SOVEREIGN_RPC_SERVE`.
    pub bind: String,
    /// Tensor-cache directory, or `None` when the operator disabled caching.
    /// Passed explicitly by the parent rather than re-resolved here, so the
    /// bytes the host warms and the directory this worker reads cannot
    /// disagree (ARCH §10.6).
    pub cache_dir: Option<String>,
    /// CPU threads for the worker's own device.
    pub threads: usize,
}

/// Parse failure. Held as a value rather than a string so the caller decides
/// how to render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcWorkerArgsError {
    /// A flag that takes a value arrived last.
    MissingValue(&'static str),
    /// `--threads` was not a positive integer.
    BadThreads(String),
    /// No `--bind` given. There is no default: a worker that guessed its own
    /// bind would advertise a port the daemon never resolved.
    NoBind,
    /// An argument this entrypoint does not define.
    Unknown(String),
}

impl std::fmt::Display for RpcWorkerArgsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingValue(flag) => write!(f, "{flag} requires a value"),
            Self::BadThreads(v) => write!(f, "--threads must be a positive integer, got {v:?}"),
            Self::NoBind => write!(f, "--bind is required"),
            Self::Unknown(a) => write!(f, "unknown argument {a:?}"),
        }
    }
}

impl RpcWorkerArgs {
    /// Parse the args that follow `--rpc-worker`.
    ///
    /// Pure, so the whole surface is unit-testable without spawning anything.
    pub fn parse(args: &[String]) -> Result<Self, RpcWorkerArgsError> {
        let mut bind: Option<String> = None;
        let mut cache_dir: Option<String> = None;
        let mut threads: Option<usize> = None;

        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();
            match arg {
                "--bind" => {
                    i += 1;
                    bind = Some(
                        args.get(i)
                            .ok_or(RpcWorkerArgsError::MissingValue("--bind"))?
                            .clone(),
                    );
                }
                "--cache-dir" => {
                    i += 1;
                    cache_dir = Some(
                        args.get(i)
                            .ok_or(RpcWorkerArgsError::MissingValue("--cache-dir"))?
                            .clone(),
                    );
                }
                "--threads" => {
                    i += 1;
                    let raw = args
                        .get(i)
                        .ok_or(RpcWorkerArgsError::MissingValue("--threads"))?;
                    let n = raw
                        .parse::<usize>()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| RpcWorkerArgsError::BadThreads(raw.clone()))?;
                    threads = Some(n);
                }
                other => return Err(RpcWorkerArgsError::Unknown(other.to_string())),
            }
            i += 1;
        }

        Ok(Self {
            bind: bind.ok_or(RpcWorkerArgsError::NoBind)?,
            cache_dir,
            threads: threads.unwrap_or_else(crate::embedded::rpc_worker_threads),
        })
    }
}

/// Entrypoint: `<binary> --rpc-worker --bind <addr> …`. Returns a process exit
/// code. The success path does not return — it serves until killed.
pub fn run(args: &[String]) -> i32 {
    let cfg = match RpcWorkerArgs::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rpc-worker: {e}");
            return 2;
        }
    };

    // Die if the daemon dies — even on an uncatchable daemon crash, so the
    // worker never outlives its parent holding :50052. Without this an
    // orphan keeps the port and every later daemon start fails to bind while
    // the supervisor backs off forever. Linux only; the macOS half is the
    // parent-pid poll below, since there is no PDEATHSIG equivalent there.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    spawn_parent_death_watch();

    init_worker_tracing();

    // The ggml device registry is empty until a backend is initialised, and
    // `LlamaBackend` is how this workspace does that everywhere else — it also
    // installs the ggml→tracing log bridge, so the child's ggml output reaches
    // the same subscriber shape the in-process worker used.
    let mut backend = match crate::llama::cpp::llama_backend::LlamaBackend::init() {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "rpc-worker: cannot init llama backend");
            return 1;
        }
    };
    crate::llama_logs::LlamaLogs::from_env().install(&mut backend);

    let mut devices = crate::embedded::collect_local_gpu_devices();
    if devices.is_empty() {
        tracing::error!(
            bind = %cfg.bind,
            "rpc-worker: no local GPU device found — nothing to serve"
        );
        return 1;
    }

    let c_bind = match std::ffi::CString::new(cfg.bind.clone()) {
        Ok(c) => c,
        Err(_) => {
            tracing::error!(bind = %cfg.bind, "rpc-worker: bind contains an interior NUL");
            return 2;
        }
    };
    let c_cache = cfg
        .cache_dir
        .as_ref()
        .and_then(|p| std::ffi::CString::new(p.as_bytes()).ok());
    let cache_ptr = c_cache.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let n_devices = devices.len();

    tracing::info!(
        bind = %cfg.bind,
        n_devices,
        threads = cfg.threads,
        cache = cfg.cache_dir.as_deref().unwrap_or("off"),
        "rpc-worker: serving local GPU to mesh peers (out of process)"
    );

    // Inner supervision, same shape and the same backoff schedule as the
    // in-process path: ggml `return`s from its accept loop on a SINGLE failed
    // `accept()` (a peer that connects then resets), which is transient and
    // must not cost a process restart — re-entering the loop here is ~100ms,
    // where bouncing the process pays backend init again. The OUTER
    // supervision, in the parent, is for what this loop cannot catch: the
    // `GGML_ASSERT` aborts and any SEGV in the backend.
    let mut consecutive_fast_exits: u32 = 0;
    loop {
        let started = Instant::now();
        // SAFETY: device pointers are owned by ggml's registry and live for
        // the process; `start_server` blocks until its accept loop tears down.
        unsafe {
            crate::llama::sys::ggml_backend_rpc_start_server(
                c_bind.as_ptr(),
                cache_ptr,
                cfg.threads,
                n_devices,
                devices.as_mut_ptr(),
            );
        }
        let ran_for = started.elapsed();
        consecutive_fast_exits = if ran_for >= std::time::Duration::from_secs(30) {
            0
        } else {
            consecutive_fast_exits.saturating_add(1)
        };
        let backoff = crate::embedded::rpc_worker_restart_backoff(consecutive_fast_exits);
        tracing::warn!(
            bind = %cfg.bind,
            ran_secs = ran_for.as_secs_f64(),
            consecutive_fast_exits,
            backoff_ms = backoff.as_millis() as u64,
            "rpc-worker: ggml accept loop exited — restarting in place"
        );
        std::thread::sleep(backoff);
    }
}

/// macOS (and any non-Linux **unix**) stand-in for `PR_SET_PDEATHSIG`: watch the
/// parent pid and exit when it changes, which is what re-parenting to
/// `launchd`/`init` looks like from here. One-second cadence — an orphan holding
/// the port for up to a second is invisible next to the backend init a restart
/// pays anyway.
///
/// `unix` and not merely `not(linux)`: `libc::getppid` does not exist on
/// Windows, and a `not(linux)` gate compiles fine on this host while breaking
/// the desktop's Windows cross-compile with E0425 — a leg no local gate runs.
/// Windows therefore has NO parent-death guard; an orphaned worker there is
/// reaped by the supervisor failing to re-bind the port, which is worse but is
/// not silently wrong. Closing it needs a Job Object, which is Phase 1 work.
#[cfg(all(unix, not(target_os = "linux")))]
fn spawn_parent_death_watch() {
    let original = unsafe { libc::getppid() };
    std::thread::Builder::new()
        .name("rpc-worker-ppid".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if unsafe { libc::getppid() } != original {
                // The daemon is gone. Exit hard rather than unwinding: ggml
                // destructors on a live RPC context are exactly the teardown
                // path that has aborted here before, and there is no state
                // worth flushing — this process owns no data root.
                std::process::exit(0);
            }
        })
        .ok();
}

/// Stderr-only subscriber. The parent drains this pipe and re-emits every line
/// under its own `rpc_worker` target, so `RUST_LOG` still explains the worker
/// even though the worker is no longer inside the daemon.
fn init_worker_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("sovereign_inference=info,rpc_worker=info"));
    let _ = fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_a_full_argument_set() {
        let got = RpcWorkerArgs::parse(&v(&[
            "--bind",
            "127.0.0.1:50052",
            "--cache-dir",
            "/tmp/rpc-cache",
            "--threads",
            "6",
        ]))
        .expect("well-formed args parse");
        assert_eq!(
            got,
            RpcWorkerArgs {
                bind: "127.0.0.1:50052".into(),
                cache_dir: Some("/tmp/rpc-cache".into()),
                threads: 6,
            }
        );
    }

    /// Caching off is an ABSENT `--cache-dir`, not the string "off". The
    /// in-process path already had a bug where a disabled cache became a
    /// directory literally named `off`; the wire between parent and child must
    /// not be able to re-express it.
    #[test]
    fn absent_cache_dir_means_caching_off() {
        let got = RpcWorkerArgs::parse(&v(&["--bind", "0.0.0.0:50052"])).unwrap();
        assert_eq!(got.cache_dir, None);
    }

    #[test]
    fn bind_is_required() {
        assert_eq!(
            RpcWorkerArgs::parse(&v(&["--threads", "4"])),
            Err(RpcWorkerArgsError::NoBind)
        );
    }

    #[test]
    fn a_flag_without_its_value_is_an_error_not_a_default() {
        assert_eq!(
            RpcWorkerArgs::parse(&v(&["--bind"])),
            Err(RpcWorkerArgsError::MissingValue("--bind"))
        );
        assert_eq!(
            RpcWorkerArgs::parse(&v(&["--bind", "x:1", "--cache-dir"])),
            Err(RpcWorkerArgsError::MissingValue("--cache-dir"))
        );
    }

    #[test]
    fn zero_and_garbage_threads_are_refused() {
        for bad in ["0", "-1", "many", ""] {
            assert_eq!(
                RpcWorkerArgs::parse(&v(&["--bind", "x:1", "--threads", bad])),
                Err(RpcWorkerArgsError::BadThreads(bad.to_string())),
                "--threads {bad:?} must be refused, not silently defaulted"
            );
        }
    }

    #[test]
    fn an_unknown_argument_is_refused_rather_than_ignored() {
        assert_eq!(
            RpcWorkerArgs::parse(&v(&["--bind", "x:1", "--serve-everything"])),
            Err(RpcWorkerArgsError::Unknown("--serve-everything".into()))
        );
    }
}
