// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A host that answers 200 to anything, on an ephemeral port. Raw
/// sockets rather than a test-server dependency: this crate is a
/// layer-0 membrane and its dep list is the contract (ARCH_LAYERS).
async fn serving_host() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                          content-length: 2\r\nconnection: close\r\n\r\n{}",
                    )
                    .await;
                let _ = sock.flush().await;
            });
        }
    });
    (port, handle)
}

/// A host that answers 200 on exactly ONE path and 404 on every other.
///
/// The shape of `sovereign-server`, which serves `/health` and has no
/// `/v1/models` at all — and the only fixture that can tell a probe
/// which door it knocked on. `serving_host()` above answers 200 to
/// anything, so a `ready_at` that silently ignored its argument would
/// pass against it (ARCH principle 5: assert on something the subject
/// cannot author).
async fn host_serving_only(path: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                // "GET /health HTTP/1.1" -> "/health"
                let asked = String::from_utf8_lossy(&buf[..n])
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let resp: &[u8] = if asked == path {
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
                } else {
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                };
                let _ = sock.write_all(resp).await;
                let _ = sock.flush().await;
            });
        }
    });
    (port, handle)
}

/// The default door is `/v1/models`, byte for byte.
///
/// Pinned because every existing caller inherits it by omission: a
/// `ready_at` that changed the default would move `attach_watch`, the
/// daemon's readiness wait and the desktop's startup reach all at once,
/// and each of them would simply stop finding a live daemon.
#[tokio::test]
async fn the_default_ready_path_is_v1_models() {
    let (port, srv) = host_serving_only("/v1/models").await;
    assert!(
        host_at(port).is_serving().await,
        "the default probe no longer asks /v1/models"
    );
    srv.abort();
}

/// A backend that answers a different door is reachable once it is named.
#[tokio::test]
async fn a_named_ready_path_reaches_a_host_the_default_would_miss() {
    let (port, srv) = host_serving_only("/health").await;
    // The regression, first: this is `sovereign-server` under the
    // default, and it is a LIVE host reported as absent.
    assert!(
        !host_at(port).is_serving().await,
        "the fixture answered /v1/models — it cannot prove anything about paths"
    );
    assert!(
        host_at(port).ready_at("/health").is_serving().await,
        "`ready_at` did not change the door the probe knocks on"
    );
    srv.abort();
}

/// A port nothing is listening on.
async fn dead_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn host_at(port: u16) -> ServingHost {
    ServingHost::at(format!("http://127.0.0.1:{port}"))
}

#[tokio::test]
async fn a_serving_host_answers_the_first_probe() {
    let (port, _srv) = serving_host().await;
    assert!(host_at(port).is_serving().await);
}

#[tokio::test]
async fn a_dead_port_is_not_serving() {
    let port = dead_port().await;
    assert!(!host_at(port).is_serving().await);
}

/// A failed probe names WHY: a silent port and a listener that is not
/// this daemon are different sentences to the person reading them.
#[tokio::test]
async fn a_failed_probe_says_why() {
    let why = host_at(dead_port().await).probe().await.unwrap_err();
    assert!(why.contains("did not answer"), "{why}");
    let (port, srv) = host_serving_only("/health").await;
    let why = host_at(port).probe().await.unwrap_err();
    assert!(why.contains("404"), "{why}");
    srv.abort();
}

#[tokio::test]
async fn an_answering_host_is_reached_without_bringing_anything_up() {
    let (port, _srv) = serving_host().await;
    let reached = host_at(port)
        .ensure_reachable(Duration::from_secs(2))
        .await
        .expect("a serving host is reachable");
    match reached {
        Reached::AlreadyServing { .. } => {}
        other => panic!("expected AlreadyServing, got {other:?}"),
    }
}

#[tokio::test]
async fn a_host_that_comes_up_late_is_waited_for_and_not_claimed_as_ours() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    // Something else starts serving 400ms in; this client started it
    // in no sense, and the outcome must not say otherwise.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let l = TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        while let Ok((mut sock, _)) = l.accept().await {
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
                .await;
        }
    });
    let reached = host_at(port)
        .ensure_reachable(Duration::from_secs(5))
        .await
        .expect("the late host is reachable");
    match reached {
        Reached::AlreadyServing { waited } => {
            assert!(
                waited >= Duration::from_millis(300),
                "waited {waited:?} — it cannot have answered before it bound",
            );
        }
        other => panic!("expected AlreadyServing, got {other:?}"),
    }
}

#[tokio::test]
async fn absence_is_reported_rather_than_defaulted() {
    let port = dead_port().await;
    let err = host_at(port)
        .ensure_reachable(Duration::from_millis(300))
        .await
        .expect_err("nothing is serving there");
    // Which variant depends on whether this build HAS the ability; both
    // say the same thing to a user, and neither is a silent success.
    match &err {
        NotReachable::NoBackendConfigured { .. } => assert!(CAN_BRING_UP_A_BACKEND),
        NotReachable::NoBackendInThisBuild { .. } => assert!(!CAN_BRING_UP_A_BACKEND),
        other => panic!("expected an absence report, got {other:?}"),
    }
    assert!(err.to_string().contains("no serving host answered"));
}

#[cfg(all(feature = "bundled-backend", unix))]
mod bring_up {
    use super::*;

    /// A backend that really serves: a python http server on `port`.
    /// Written to a file rather than `-c` so the source stays readable.
    fn python_backend(port: u16, dir: &std::path::Path) -> BundledBackend {
        let script = dir.join(format!("fake-backend-{port}.py"));
        std::fs::write(
            &script,
            format!(
                "import http.server\n\
                 class H(http.server.BaseHTTPRequestHandler):\n\
                 \x20   def do_GET(self):\n\
                 \x20       self.send_response(200)\n\
                 \x20       self.send_header('content-length', '2')\n\
                 \x20       self.end_headers()\n\
                 \x20       self.wfile.write(b'{{}}')\n\
                 \x20   def log_message(self, *a):\n\
                 \x20       pass\n\
                 http.server.HTTPServer(('127.0.0.1', {port}), H).serve_forever()\n",
            ),
        )
        .unwrap();
        BundledBackend::at("/usr/bin/env")
            .arg("python3")
            .arg(script.display().to_string())
    }

    fn kill(pid: u32) {
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .status();
    }

    #[tokio::test]
    async fn a_configured_backend_is_brought_up_and_waited_for() {
        let port = dead_port().await;
        let dir = std::env::temp_dir();
        let host = host_at(port).bringing_up(python_backend(port, &dir));
        let reached = host
            .ensure_reachable(Duration::from_secs(20))
            .await
            .expect("the backend should come up and answer");
        match reached {
            Reached::BroughtUp { pid, .. } => {
                assert!(host.is_serving().await, "it should still be serving");
                kill(pid);
            }
            other => panic!("expected BroughtUp, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_brought_up_process_is_in_its_own_process_group() {
        let port = dead_port().await;
        let dir = std::env::temp_dir();
        let host = host_at(port).bringing_up(python_backend(port, &dir));
        let Reached::BroughtUp { pid, .. } = host
            .ensure_reachable(Duration::from_secs(20))
            .await
            .expect("brought up")
        else {
            panic!("expected BroughtUp");
        };
        let pgid = |p: u32| -> String {
            let out = std::process::Command::new("ps")
                .args(["-o", "pgid=", "-p", &p.to_string()])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let ours = pgid(std::process::id());
        let theirs = pgid(pid);
        kill(pid);
        assert!(!theirs.is_empty(), "the child should still exist");
        assert_ne!(
            ours, theirs,
            "a backend in our process group dies with our terminal's Ctrl-C",
        );
    }

    #[tokio::test]
    async fn a_backend_that_never_answers_is_reported_not_defaulted() {
        let port = dead_port().await;
        let host =
            host_at(port).bringing_up(BundledBackend::at("/bin/sh").arg("-c").arg("sleep 30"));
        let err = host
            .ensure_reachable(Duration::from_millis(800))
            .await
            .expect_err("it never serves");
        match err {
            NotReachable::SilentAfterLaunch { pid, .. } => kill(pid),
            other => panic!("expected SilentAfterLaunch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_backend_path_that_does_not_exist_is_a_launch_failure() {
        let port = dead_port().await;
        let host = host_at(port).bringing_up(BundledBackend::at("/nonexistent/svrn-daemon-xyz"));
        let err = host
            .ensure_reachable(Duration::from_millis(500))
            .await
            .expect_err("there is no such binary");
        match err {
            NotReachable::LaunchFailed { program, .. } => {
                assert!(program.contains("svrn-daemon-xyz"));
            }
            other => panic!("expected LaunchFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_serving_host_is_never_duplicated_by_a_bring_up() {
        let (port, _srv) = serving_host().await;
        let host = host_at(port).bringing_up(
            BundledBackend::at("/bin/sh")
                .arg("-c")
                .arg("echo should-not-run > /dev/null"),
        );
        match host.ensure_reachable(Duration::from_secs(2)).await.unwrap() {
            Reached::AlreadyServing { .. } => {}
            other => panic!("a host was already serving; got {other:?}"),
        }
    }
}
