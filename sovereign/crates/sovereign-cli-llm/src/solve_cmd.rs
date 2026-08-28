// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn solve <workdir> "goal"` — hand the daemon a coding
//! goal, get a green tree back. Spec: `docs/specs/SOLVE_UX.md`.
//!
//! Thin client over the daemon's `/v1/solve/jobs` surface: submit,
//! then either print the job id (default) or stream the SSE round
//! events until done (`--watch`). No solver logic lives here.

use futures::StreamExt as _;
use serde_json::Value;

const HELP: &str = r#"svrn solve — give the daemon a coding goal, get a green tree back

USAGE:
  svrn solve <workdir> "<goal>" [--watch] [options]
  svrn solve --status <job_id>
  svrn solve --cancel <job_id>

The daemon makes the goal test-shaped (using your failing tests if
you have them; writing the one failing test that pins the goal if
you don't), then iterates until the tests pass. Review the result
with `git diff` in the workdir.

OPTIONS:
  --watch               stream rounds live until the job finishes
  --verb <v>            fix | pin | split — only when the default
                        inference isn't what you meant
  --max-lines <n>       with --verb split: per-file line budget
  --suite <s>           unit | e2e — steer to the browser (Playwright)
                        suite when the project has both
  --test-command <cmd>  override the auto-detected test command
  --model <id>          override the daemon's primary model
  --force               solve on a dirty tree (uncommitted changes)
  --daemon <url>        daemon base URL (default from setup config,
                        http://localhost:9741)

EXIT CODE: 0 on reached/improved, 1 on stalled/no_baseline/errored,
130 on cancelled.
"#;

/// Mirror of `commonwealth-tdd`'s Playwright default command (the
/// CLI is a thin HTTP client and doesn't link the engine crate).
/// If they drift the daemon still profiles correctly — it keys off
/// the `playwright test` substring, present in both.
const PLAYWRIGHT_TEST_COMMAND: &str =
    "CI=1 npx playwright test --reporter=line --retries=0 --workers=1";

struct Args {
    workdir: Option<String>,
    goal: Option<String>,
    watch: bool,
    verb: Option<String>,
    max_lines: Option<u64>,
    suite: Option<String>,
    test_command: Option<String>,
    model: Option<String>,
    force: bool,
    daemon: Option<String>,
    status: Option<String>,
    cancel: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        workdir: None,
        goal: None,
        watch: false,
        verb: None,
        max_lines: None,
        suite: None,
        test_command: None,
        model: None,
        force: false,
        daemon: None,
        status: None,
        cancel: None,
    };
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    let take_value = |args: &[String], i: usize, flag: &str| -> Result<String, String> {
        args.get(i + 1)
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => return Err(HELP.to_string()),
            "--watch" => {
                out.watch = true;
                i += 1;
            }
            "--force" => {
                out.force = true;
                i += 1;
            }
            "--verb" => {
                out.verb = Some(take_value(args, i, "--verb")?);
                i += 2;
            }
            "--max-lines" => {
                let v = take_value(args, i, "--max-lines")?;
                out.max_lines = Some(v.parse().map_err(|_| "--max-lines needs a number")?);
                i += 2;
            }
            "--suite" => {
                out.suite = Some(take_value(args, i, "--suite")?);
                i += 2;
            }
            "--test-command" => {
                out.test_command = Some(take_value(args, i, "--test-command")?);
                i += 2;
            }
            "--model" => {
                out.model = Some(take_value(args, i, "--model")?);
                i += 2;
            }
            "--daemon" => {
                out.daemon = Some(take_value(args, i, "--daemon")?);
                i += 2;
            }
            "--status" => {
                out.status = Some(take_value(args, i, "--status")?);
                i += 2;
            }
            "--cancel" => {
                out.cancel = Some(take_value(args, i, "--cancel")?);
                i += 2;
            }
            flag if flag.starts_with("--") => return Err(format!("unknown flag {flag}\n\n{HELP}")),
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }
    let mut positional = positional.into_iter();
    out.workdir = positional.next();
    out.goal = positional.next();
    Ok(out)
}

/// An explicit `--daemon` still wins; everything below it is the ONE decider
/// (§10.6). This used to re-roll that resolution — same config field, same
/// `http://localhost:{port}` shape, same trailing-slash trim — and so was
/// blind to `SOVEREIGN_DAEMON_URL` like every other copy of it.
fn daemon_base(explicit: Option<&str>) -> String {
    match explicit {
        Some(url) => url.trim_end_matches('/').to_string(),
        None => sovereign_core::setup_config::client_daemon_base(),
    }
}

pub async fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let base = daemon_base(parsed.daemon.as_deref());
    let http = reqwest::Client::new();

    if let Some(id) = &parsed.status {
        return print_status(&http, &base, id).await;
    }
    if let Some(id) = &parsed.cancel {
        return do_cancel(&http, &base, id).await;
    }

    let (Some(workdir), Some(goal)) = (&parsed.workdir, &parsed.goal) else {
        eprintln!("{HELP}");
        return 2;
    };
    // The daemon resolves paths on ITS filesystem — absolutize here
    // so `svrn solve . "goal"` means the caller's cwd.
    let workdir = match std::fs::canonicalize(workdir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: workdir {workdir}: {e}");
            return 2;
        }
    };

    // --suite translates to a test_command — the wire stays two
    // fields plus the same optional overrides for everyone.
    let test_command = match (parsed.suite.as_deref(), parsed.test_command.clone()) {
        (Some(_), Some(_)) => {
            eprintln!("error: pass one of --suite or --test-command, not both");
            return 2;
        }
        (Some("e2e"), None) => Some(PLAYWRIGHT_TEST_COMMAND.to_string()),
        (Some("unit"), None) | (None, _) => parsed.test_command.clone(),
        (Some(other), None) => {
            eprintln!("error: --suite {other:?} — valid: unit, e2e");
            return 2;
        }
    };
    let mut body = serde_json::json!({
        "workdir": workdir,
        "goal": goal,
        "force": parsed.force,
    });
    for (key, v) in [
        ("verb", parsed.verb.clone().map(Value::from)),
        ("test_command", test_command.map(Value::from)),
        ("model", parsed.model.clone().map(Value::from)),
        ("max_lines", parsed.max_lines.map(Value::from)),
    ] {
        if let Some(v) = v {
            body[key] = v;
        }
    }

    let resp = match http
        .post(format!("{base}/v1/solve/jobs"))
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            eprintln!("hint: is the daemon running? try `svrn daemon status`.");
            return 1;
        }
    };
    let status = resp.status();
    let v: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: unreadable daemon response: {e}");
            return 1;
        }
    };
    if status.as_u16() != 202 {
        eprintln!(
            "refused ({status}): {}",
            v["message"].as_str().unwrap_or(&v.to_string())
        );
        if v["error"] == "dirty_workdir" {
            eprintln!("hint: commit or stash first, or pass --force.");
        }
        return 1;
    }

    let job_id = v["job_id"].as_str().unwrap_or_default().to_string();
    let d = &v["detected"];
    println!(
        "job {job_id}\n  detected: {} · `{}` · {}",
        d["framework"].as_str().unwrap_or("?"),
        d["test_command"].as_str().unwrap_or("?"),
        d["model"].as_str().unwrap_or("?"),
    );
    if d["also_detected"].as_str() == Some("playwright") {
        println!("  note: playwright config present — unit suite is the default; pass --suite e2e for the browser tests");
    }

    if !parsed.watch {
        println!(
            "\nrunning in the background —\n  watch:  svrn solve --status {job_id}\n  cancel: svrn solve --cancel {job_id}"
        );
        return 0;
    }
    watch_events(&http, &base, &job_id, &workdir).await
}

/// Stream `GET /v1/solve/jobs/{id}/events` and render each round as
/// one line. Returns the exit code derived from the done event.
async fn watch_events(
    http: &reqwest::Client,
    base: &str,
    job_id: &str,
    workdir: &std::path::Path,
) -> i32 {
    let resp = match http
        .get(format!("{base}/v1/solve/jobs/{job_id}/events"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            eprintln!("error: events stream refused ({})", r.status());
            return 1;
        }
        Err(e) => {
            eprintln!("error: events stream failed: {e}");
            return 1;
        }
    };
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: events stream dropped: {e}");
                return 1;
            }
        };
        buf.push_str(&String::from_utf8_lossy(&chunk));
        // SSE frames are separated by a blank line.
        while let Some(frame_end) = buf.find("\n\n") {
            let frame: String = buf.drain(..frame_end + 2).collect();
            let Some(data) = frame
                .lines()
                .find_map(|l| l.strip_prefix("data:").map(str::trim))
            else {
                continue; // keep-alive comment frame
            };
            let Ok(ev) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            match ev["event"].as_str() {
                Some("round") => print_round(&ev),
                Some("done") => return print_done(&ev, workdir),
                _ => {}
            }
        }
    }
    eprintln!("error: events stream ended without a done event");
    1
}

fn print_round(ev: &Value) {
    let stage = ev["stage"].as_str().unwrap_or("?");
    let round = ev["round"].as_u64().unwrap_or(0);
    let passing = ev["passing_after"].as_u64().unwrap_or(0);
    let failing = ev["failed_after"].as_u64().unwrap_or(0);
    match ev["winner"].as_str() {
        Some(winner) => {
            println!(
                "[{stage}] round {round} — {winner} won · {passing} passing / {failing} failing"
            );
        }
        None => {
            let tried = ev["candidates"]
                .as_array()
                .map(|c| c.len())
                .unwrap_or_default();
            println!("[{stage}] round {round} — no improvement ({tried} candidates tried)");
        }
    }
}

fn print_done(ev: &Value, workdir: &std::path::Path) -> i32 {
    let status = ev["status"].as_str().unwrap_or("?");
    let rounds = ev["rounds"].as_u64().unwrap_or(0);
    let passed = ev["tests_passed"].as_u64().unwrap_or(0);
    let failed = ev["tests_failed"].as_u64().unwrap_or(0);
    match status {
        "reached" => {
            println!("\n✓ reached — {passed} passing / {failed} failing after {rounds} round(s)");
            println!("  review: git -C {} diff", workdir.display());
            0
        }
        "improved" => {
            println!("\n→ improved — {passed} passing / {failed} failing after {rounds} round(s)");
            println!("  progress landed; call solve again to continue.");
            0
        }
        "cancelled" => {
            println!("\n✗ cancelled");
            130
        }
        other => {
            let reason = ev["reason"].as_str().unwrap_or("");
            println!(
                "\n✗ {other}{}{reason}",
                if reason.is_empty() { "" } else { " — " }
            );
            1
        }
    }
}

async fn print_status(http: &reqwest::Client, base: &str, id: &str) -> i32 {
    match http.get(format!("{base}/v1/solve/jobs/{id}")).send().await {
        Ok(r) if r.status().is_success() => match r.json::<Value>().await {
            Ok(v) => {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                0
            }
            Err(e) => {
                eprintln!("error: unreadable status: {e}");
                1
            }
        },
        Ok(r) => {
            eprintln!("error: status refused ({})", r.status());
            1
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            eprintln!("hint: is the daemon running? try `svrn daemon status`.");
            1
        }
    }
}

async fn do_cancel(http: &reqwest::Client, base: &str, id: &str) -> i32 {
    match http
        .delete(format!("{base}/v1/solve/jobs/{id}"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            println!("cancelled {id}");
            0
        }
        Ok(r) => {
            eprintln!("error: cancel refused ({})", r.status());
            1
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_the_two_field_form() {
        let a = parse_args(&s(&["/repo", "add is_palindrome to utils.py"])).unwrap();
        assert_eq!(a.workdir.as_deref(), Some("/repo"));
        assert_eq!(a.goal.as_deref(), Some("add is_palindrome to utils.py"));
        assert!(!a.watch);
        assert!(a.verb.is_none());
    }

    #[test]
    fn parses_watch_and_verb_flags() {
        let a = parse_args(&s(&[
            "/repo",
            "split the parser",
            "--watch",
            "--verb",
            "split",
            "--max-lines",
            "300",
        ]))
        .unwrap();
        assert!(a.watch);
        assert_eq!(a.verb.as_deref(), Some("split"));
        assert_eq!(a.max_lines, Some(300));
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert!(parse_args(&s(&["/repo", "goal", "--frobnicate"])).is_err());
    }

    #[test]
    fn parses_suite_flag() {
        let a = parse_args(&s(&["/repo", "save shows a toast", "--suite", "e2e"])).unwrap();
        assert_eq!(a.suite.as_deref(), Some("e2e"));
        assert!(a.test_command.is_none());
    }

    #[test]
    fn status_flag_needs_no_positionals() {
        let a = parse_args(&s(&["--status", "abc"])).unwrap();
        assert_eq!(a.status.as_deref(), Some("abc"));
        assert!(a.workdir.is_none());
    }
}
