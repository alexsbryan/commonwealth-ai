// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn daemon status`'s serving line. Kept out of lifecycle.rs, which sits
//! at its size ceiling (pb-stock-binary, where serve_stop.rs's stop half went:
//! svrn brings up no serve, so it has none to stop).

/// Where the daemon serves from, as it decided at boot (`/status`'s
/// `serving`), and, when it dials a serve on this host, whether serve is
/// listening on its port. A hosted serve is the daemon's own process.
pub(super) async fn print_serving(client: &reqwest::Client, base: &str) {
    let serving = match client.get(format!("{base}/status")).send().await {
        Ok(r) if r.status().is_success() => {
            r.json::<serde_json::Value>().await.ok().and_then(|v| {
                v.get("serving")
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
            })
        }
        _ => None,
    };
    let Some(serving) = serving else {
        println!("  serving: not reported by this daemon");
        return;
    };
    println!("  serving: {serving}");
    if serving != "serve" {
        return;
    }
    let port = sovereign_contracts::venue::serve_port();
    #[cfg(unix)]
    match super::lifecycle::find_daemon_pid_by_port(port) {
        Some(pid) => println!("  serve: running (pid {pid}, :{port})"),
        None => println!("  serve: nothing listening on :{port}"),
    }
}
