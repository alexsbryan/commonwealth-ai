// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn run --as chores -- python app.py` — the 1am hack, published while it
//! runs and gone when it stops.
//!
//! ```text
//! svrn run --as chores -- python app.py
//! ```
//!
//! One line, no config edited, no daemon restarted, and — the part that
//! matters six months later — no entry left behind. The registration is a
//! claim held by THIS process: renewed while the child lives, released when
//! it exits, and dropped by its TTL if this runner dies badly. A published
//! app cannot outlive the process serving it, so the house fan-out cannot
//! accumulate `connection refused` rows from apps that stopped existing in
//! March.
//!
//! # The port
//!
//! With no `--port`, the runner takes a free one, hands it to the child as
//! `PORT`, and publishes that. Most one-file web apps read `PORT`; one that
//! does not will bind its own hard-coded port instead, so nothing answers the
//! one we published — which is why the runner WAITS for the port to accept a
//! connection before it publishes anything, and says exactly that when it
//! does not. Naming `--port` is the answer for an app with a fixed port of
//! its own.
//!
//! Publishing before the child binds would be the easy version and the wrong
//! one: a housemate's fan-out would get `connection refused` from an app that
//! is simply still starting, and the row would read like a broken app rather
//! than a slow one.
//!
//! # When the daemon is not up
//!
//! The child still runs, and the runner says plainly that nothing is
//! published yet. It keeps trying on the heartbeat, so starting your app
//! before the daemon — or restarting the daemon under a running app — heals
//! within one tick instead of needing you to notice. The claim tier is
//! in-memory by construction: a daemon that restarts has forgotten every
//! claim, and re-claiming is the runner's job precisely because the runner is
//! the thing that still exists.

use std::process::Stdio;
use std::time::Duration;

use crate::mesh_cmd::daemon_client_port;
use crate::mesh_guest::{human_duration, parse_ttl};

/// How often the claim is renewed, and how often a lost claim is retaken.
/// A third of the TTL leaves room for two missed ticks, and the 30s ceiling
/// is about the retake rather than the renew: after a daemon restart an app
/// should be back in the house's fan-out in seconds, not in a third of an
/// hour.
fn heartbeat(ttl_secs: u64) -> Duration {
    Duration::from_secs((ttl_secs / 3).clamp(1, 30))
}

/// The default claim TTL, matching `commonwealth_media::apps::DEFAULT_CLAIM_TTL`.
/// Named here as the CLI's own default so `--help` can print it; the daemon
/// applies its own when the field is absent, and the answer says what was
/// granted either way.
const DEFAULT_TTL_SECS: u64 = 3600;

pub async fn run(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        help();
        return 0;
    }
    if args.is_empty() {
        eprintln!("Usage: svrn run --as <name> [--port <n>] [--ttl <duration>] -- <command> …");
        eprintln!("Run `svrn run --help` for the whole shape.");
        return 2;
    }
    let opts = match Options::parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("run: {e}");
            eprintln!();
            eprintln!("Usage: svrn run --as <name> [--port <n>] [--ttl 1h] -- <command> …");
            return 2;
        }
    };
    supervise(opts).await
}

/// What the verb was asked to do, once, so the rest of the flow reads
/// straight through.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    name: String,
    /// `None` means "take a free one and tell the child through `PORT`".
    port: Option<u16>,
    ttl_secs: u64,
    command: Vec<String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        // The command after `--` is taken VERBATIM, including anything that
        // looks like one of our flags: `svrn run --as x -- python app.py
        // --port 5000` passes `--port 5000` to python, which is what the
        // person wrote and what any other runner would do.
        let (ours, command) = match args.iter().position(|a| a == "--") {
            Some(i) => (&args[..i], args[i + 1..].to_vec()),
            None => (args, Vec::new()),
        };
        if command.is_empty() {
            return Err(
                "no command. Put it after `--`, e.g. `svrn run --as chores -- python app.py`"
                    .into(),
            );
        }
        let mut name = None;
        let mut port = None;
        let mut ttl_secs = DEFAULT_TTL_SECS;
        let mut it = ours.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--as" => {
                    name = Some(it.next().ok_or("--as needs a name")?.clone());
                }
                "--port" => {
                    let raw = it.next().ok_or("--port needs a number")?;
                    port = Some(
                        raw.parse::<u16>()
                            .map_err(|_| format!("--port: expected a port number, got '{raw}'"))?,
                    );
                }
                "--ttl" => {
                    ttl_secs = parse_ttl(it.next().ok_or("--ttl needs a duration")?)?;
                }
                other => return Err(format!("unknown flag '{other}'")),
            }
        }
        Ok(Self {
            name: name
                .ok_or("--as <name> is required — it is the name housemates reach the app by")?,
            port,
            ttl_secs,
            command,
        })
    }
}

/// A port nothing is listening on right now.
///
/// Racy by nature — between this bind and the child's, something else could
/// take it. That race is why the runner waits for the child to actually
/// answer on the port before publishing it, rather than trusting this.
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("could not take a free port: {e}"))?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| format!("could not read the port we just took: {e}"))
}

async fn supervise(opts: Options) -> i32 {
    let port = match opts.port {
        Some(p) => p,
        None => match free_port() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("run: {e}");
                return 1;
            }
        },
    };
    let mut cmd = tokio::process::Command::new(&opts.command[0]);
    cmd.args(&opts.command[1..])
        .env("PORT", port.to_string())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("run: could not start {:?}: {e}", opts.command[0]);
            return 127;
        }
    };
    eprintln!(
        "run: started {} (PORT={port}) — waiting for it to answer before publishing it",
        opts.command.join(" ")
    );

    let publisher = Publisher::new(&opts.name, port, opts.ttl_secs);
    // Wait for the child to bind, unless it exits first. A child that dies in
    // its first second is the common case when the command is wrong, and
    // reporting that as "your app never bound the port" would send someone
    // debugging their Flask config instead of reading the traceback that just
    // scrolled past.
    let bound = tokio::select! {
        status = child.wait() => {
            return exit_of(status.map_err(|e| e.to_string()));
        }
        bound = wait_for_bind(port, Duration::from_secs(30)) => bound,
    };
    if bound {
        publisher.claim_now().await;
    } else {
        eprintln!(
            "run: nothing is listening on 127.0.0.1:{port} after 30s, so {:?} is NOT published.",
            opts.name
        );
        if opts.port.is_none() {
            eprintln!(
                "  The runner picked that port and passed it as PORT. An app that ignores PORT \
                 binds its own instead — pass `--port <the app's port>`."
            );
        }
        eprintln!("  The command keeps running; publishing retries every tick, as soon");
        eprintln!("  as something answers on that port.");
    }

    // From here the runner does three things until the child stops: renew the
    // claim, retake it if the daemon forgot it, and pass on a signal.
    let mut tick = tokio::time::interval(heartbeat(opts.ttl_secs));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // the first tick is immediate; the claim is fresh
    let status = loop {
        tokio::select! {
            status = child.wait() => break status.map_err(|e| e.to_string()),
            _ = tick.tick() => publisher.beat().await,
            _ = shutdown_signal() => {
                eprintln!();
                eprintln!("run: stopping {:?} and unpublishing it", opts.name);
                terminate(&mut child).await;
                break child.wait().await.map_err(|e| e.to_string());
            }
        }
    };
    // Release BEFORE returning, always: the TTL is the backstop for a runner
    // that was killed, not the mechanism for one that exited.
    publisher.release().await;
    exit_of(status)
}

fn exit_of(status: Result<std::process::ExitStatus, String>) -> i32 {
    match status {
        Ok(s) => s.code().unwrap_or(if s.success() { 0 } else { 1 }),
        Err(e) => {
            eprintln!("run: lost track of the child process: {e}");
            1
        }
    }
}

/// Ask the child to stop, the way a person would.
///
/// SIGTERM first, because an app that writes anything on the way out deserves
/// the chance — a `kill()` here is SIGKILL and takes that away. The child gets
/// a moment, then SIGKILL if it is still up, so a wedged app cannot make
/// ctrl-C hang.
async fn terminate(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // Safety: `kill(2)` with a pid tokio still owns. The child is reaped
        // by the `wait` that follows, so the pid cannot have been recycled
        // under us.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        for _ in 0..50 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        eprintln!("run: it did not stop on SIGTERM after 5s — killing it");
    }
    let _ = child.kill().await;
}

/// Ctrl-C, and SIGTERM where there is one.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Poll until something accepts on the port, or the budget runs out.
///
/// A TCP connect rather than an HTTP request: the app may answer 404 or 500
/// at `/` and still be exactly what its author meant to publish. What is
/// being waited for is "a server is there", and connect is the question that
/// asks only that.
async fn wait_for_bind(port: u16, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The claim this runner holds with the local daemon, and the three things it
/// does with it.
struct Publisher {
    name: String,
    port: u16,
    ttl_secs: u64,
    base: String,
    http: reqwest::Client,
    claim_id: tokio::sync::Mutex<Option<String>>,
}

impl Publisher {
    fn new(name: &str, port: u16, ttl_secs: u64) -> Self {
        let daemon = daemon_client_port();
        Self::at(
            &format!("http://127.0.0.1:{daemon}/v1/mesh/publish"),
            name,
            port,
            ttl_secs,
        )
    }

    /// The same publisher against a named base — how the tests point one at a
    /// stand-in daemon without a config on disk.
    fn at(base: &str, name: &str, port: u16, ttl_secs: u64) -> Self {
        Self {
            name: name.to_string(),
            port,
            ttl_secs,
            base: base.to_string(),
            // `unwrap_or_default()` here would hand back a client with NO
            // timeout, so a wedged daemon would hang the runner's heartbeat
            // forever instead of printing a retry — a failure wearing the
            // shape of the thing that failed. The builder only errors on a
            // broken TLS backend, which is fatal for every call this makes.
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("a loopback HTTP client with a timeout"),
            claim_id: tokio::sync::Mutex::new(None),
        }
    }

    /// Take the claim, reporting either outcome to the person watching. A
    /// failure here is never silent: an app the house cannot see is the one
    /// thing this verb exists to prevent.
    ///
    /// The bind check is HERE rather than at the call sites, because there
    /// are two of them — the first publish and every retry — and only one of
    /// them is obvious. Publishing a port nothing answers on puts a
    /// `connection refused` row in a housemate's fan-out for an app that is
    /// merely still starting, and a guard that each caller has to remember is
    /// a guard that the second caller does not have (ARCH principle 10). The
    /// first publish has already waited up to 30s, so this costs it nothing.
    async fn claim_now(&self) {
        if !wait_for_bind(self.port, Duration::from_millis(200)).await {
            return;
        }
        let body = serde_json::json!({
            "name": self.name,
            "port": self.port,
            "ttl_secs": self.ttl_secs,
        });
        match self.http.post(&self.base).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                // A 200 carrying no usable claim id is NOT a publish. Taking
                // the empty string here would print success and leave the
                // runner holding an id it can never renew or release — the
                // app then vanishes from the house at its TTL with nobody
                // told, which is worse than a refusal (ARCH principle 6).
                let id = match resp.json::<serde_json::Value>().await {
                    Ok(claim) => claim
                        .get("claim_id")
                        .and_then(|v| v.as_str())
                        .filter(|id| !id.is_empty())
                        .map(str::to_string),
                    Err(e) => {
                        eprintln!("run: the daemon accepted the publish but its answer did not parse ({e})");
                        None
                    }
                };
                let Some(id) = id else {
                    eprintln!(
                        "run: NOT published — the daemon answered success with no claim id, so \
                         there is nothing to renew or release. This is a daemon older than this \
                         CLI, or a route that is not the publish route."
                    );
                    return;
                };
                *self.claim_id.lock().await = Some(id);
                eprintln!(
                    "run: published {:?} on 127.0.0.1:{} for {} — housemates reach it now",
                    self.name,
                    self.port,
                    human_duration(self.ttl_secs)
                );
                eprintln!("  they run:  svrn mesh app <you> {}", self.name);
                eprintln!(
                    "  or ask everyone at once:  svrn mesh app fanout {} /",
                    self.name
                );
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let why = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                    .unwrap_or(body);
                eprintln!("run: NOT published ({status}): {why}");
            }
            Err(e) => {
                eprintln!(
                    "run: NOT published — the daemon on 127.0.0.1 did not answer ({e}). \
                     Start it with `svrn daemon start`; this keeps retrying."
                );
            }
        }
    }

    /// One heartbeat: renew what we hold, or take it if we hold nothing.
    ///
    /// A renew that comes back 404 means the daemon forgot the claim — it
    /// restarted, or the TTL beat us — and the right answer is to take it
    /// again rather than to keep renewing a claim that is gone. Reporting
    /// "renewed" on a 404 would be the substitution that leaves an app
    /// invisible to the house while its runner prints success (ARCH §6).
    ///
    /// Retaking goes through `claim_now`, so it re-checks the port for the
    /// same reason the first publish waits for it.
    async fn beat(&self) {
        let held = self.claim_id.lock().await.clone();
        let Some(id) = held else {
            self.claim_now().await;
            return;
        };
        let url = format!("{}/{}/renew", self.base, id);
        let body = serde_json::json!({ "ttl_secs": self.ttl_secs });
        match self.http.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                eprintln!("run: the daemon no longer holds our claim — republishing");
                *self.claim_id.lock().await = None;
                self.claim_now().await;
            }
            Ok(resp) => eprintln!("run: renewing the claim failed ({})", resp.status()),
            Err(e) => eprintln!("run: renewing the claim failed ({e}) — will retry"),
        }
    }

    async fn release(&self) {
        let Some(id) = self.claim_id.lock().await.take() else {
            return;
        };
        let url = format!("{}/{}", self.base, id);
        match self.http.delete(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                eprintln!("run: unpublished {:?}", self.name);
            }
            Ok(resp) => eprintln!(
                "run: could not unpublish {:?} ({}) — its TTL drops it within {}",
                self.name,
                resp.status(),
                human_duration(self.ttl_secs)
            ),
            Err(e) => eprintln!(
                "run: could not unpublish {:?} ({e}) — its TTL drops it within {}",
                self.name,
                human_duration(self.ttl_secs)
            ),
        }
    }
}

fn help() {
    println!("Usage: svrn run --as <name> [--port <n>] [--ttl <duration>] -- <command> …");
    println!();
    println!("Run a local app and publish it to the house for as long as it runs.");
    println!("Nothing is written to config and no daemon is restarted; when the command");
    println!("exits, the app stops being published.");
    println!();
    println!("  svrn run --as chores -- python app.py");
    println!();
    println!("Housemates then reach it with `svrn mesh app <you> chores`, and requests");
    println!("arrive carrying their verified identity in X-Mesh-Member — no login page,");
    println!("no port forwarded, no VPN.");
    println!();
    println!("Flags:");
    println!("  --as <name>    Required. What housemates reach it by (letters, digits, _ -).");
    println!("  --port <n>     The port the app listens on. Default: take a free one and");
    println!("                 pass it to the command as PORT.");
    println!("  --ttl <dur>    How long a claim survives without a heartbeat (default 1h,");
    println!("                 max 24h). 30s / 10m / 2h / 1d, or bare seconds.");
    println!();
    println!("For an always-on service, `svrn publish <name> <port>` writes it to config");
    println!("instead — a durable assertion for something somebody keeps true.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_command_after_the_separator_is_taken_verbatim() {
        let o = Options::parse(&args(&[
            "--as", "chores", "--", "python", "app.py", "--port", "5000",
        ]))
        .unwrap();
        assert_eq!(o.name, "chores");
        assert_eq!(o.port, None, "--port after `--` belongs to the child");
        assert_eq!(o.command, args(&["python", "app.py", "--port", "5000"]));
    }

    #[test]
    fn our_flags_are_read_before_the_separator() {
        let o = Options::parse(&args(&[
            "--as", "chores", "--port", "5000", "--ttl", "2h", "--", "flask", "run",
        ]))
        .unwrap();
        assert_eq!(o.port, Some(5000));
        assert_eq!(o.ttl_secs, 7200);
        assert_eq!(o.command, args(&["flask", "run"]));
    }

    /// Without `--`, everything after the flags would have to be guessed at,
    /// and a guess that swallows a flag is the failure this refuses.
    #[test]
    fn a_command_with_no_separator_is_refused_rather_than_guessed() {
        assert!(Options::parse(&args(&["--as", "chores", "python", "app.py"])).is_err());
        assert!(Options::parse(&args(&["--as", "chores", "--"])).is_err());
    }

    #[test]
    fn the_name_is_required_because_it_is_what_the_house_reaches() {
        assert!(Options::parse(&args(&["--", "python", "app.py"])).is_err());
    }

    #[test]
    fn an_unknown_flag_is_refused_rather_than_passed_along() {
        assert!(Options::parse(&args(&["--as", "x", "--wat", "--", "true"])).is_err());
    }

    /// The retake interval is what decides how long an app is invisible after
    /// a daemon restart, so it is capped independently of the TTL.
    #[test]
    fn the_heartbeat_is_a_third_of_the_ttl_capped_at_thirty_seconds() {
        assert_eq!(heartbeat(60), Duration::from_secs(20));
        assert_eq!(heartbeat(3600), Duration::from_secs(30));
        assert_eq!(heartbeat(1), Duration::from_secs(1), "never zero");
    }

    #[tokio::test]
    async fn wait_for_bind_returns_when_something_accepts() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(wait_for_bind(port, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn wait_for_bind_gives_up_on_a_port_nothing_takes() {
        let port = free_port().unwrap();
        assert!(!wait_for_bind(port, Duration::from_millis(300)).await);
    }

    /// A stand-in for the daemon's publish surface that counts what it was
    /// asked to publish. Returns `(base_url, counter)`.
    async fn fake_daemon() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use axum::routing::post;
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = seen.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/v1/mesh/publish",
            post(move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(serde_json::json!({
                        "claim_id": "chores-abc123",
                        "name": "chores",
                        "addr": "127.0.0.1:5000",
                        "expires_in_secs": 3600,
                    }))
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1/mesh/publish"), seen)
    }

    /// The invariant the whole wait exists for: a port nothing answers on is
    /// never published, on the FIRST attempt or on any retry.
    ///
    /// The failing input is a runner that publishes optimistically — a
    /// housemate's fan-out then shows `connection refused` for an app that is
    /// merely still starting, which reads as broken rather than slow. There
    /// are two call sites and only one is obvious, which is why the guard
    /// lives in `claim_now` and this test drives the retry path.
    #[tokio::test]
    async fn a_port_nothing_answers_on_is_never_published() {
        let (base, seen) = fake_daemon().await;
        let dead = free_port().unwrap();
        let publisher = Publisher::at(&base, "chores", dead, 60);

        publisher.beat().await;
        publisher.beat().await;
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "nothing is listening on that port, so nothing may be published"
        );
        assert!(publisher.claim_id.lock().await.is_none());
    }

    /// The other half, so the test above cannot pass by never publishing at
    /// all: bind the port and the same heartbeat takes the claim.
    #[tokio::test]
    async fn a_port_that_answers_is_published_by_the_heartbeat() {
        let (base, seen) = fake_daemon().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alive = listener.local_addr().unwrap().port();
        let publisher = Publisher::at(&base, "chores", alive, 60);

        publisher.beat().await;
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            publisher.claim_id.lock().await.as_deref(),
            Some("chores-abc123"),
            "the runner must hold the claim it was handed, or it can never release it"
        );
    }
}
