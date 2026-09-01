//! `svrn update` — check for and install a newer CLI release.
//!
//! Unlike the desktop app (which has `tauri-plugin-updater` + an in-app
//! banner), the CLI has no background updater. This verb:
//!   1. reports the running version (the compiled-in workspace version),
//!   2. resolves the newest published `cli-v*` in the release repo by MAX
//!      SEMVER — never GitHub list order. Every release here shares
//!      an identical `created_at` (derived from the tagged commit's date), so
//!      GitHub's ordering is an unstable tiebreak; the desktop updater
//!      endpoint and `landing/install.sh` both hit — and fixed — this exact
//!      trap (2026-07-15). Trusting `[0]` handed users an OLDER version.
//!   3. when newer, re-runs the CANONICAL installer (`curl … | install.sh`)
//!      pinned to that version — so "download + checksum + place binaries"
//!      has exactly ONE implementation (install.sh) that this orchestrates
//!      rather than duplicates (DRY; ground in reuse before building).
//!
//! `--check` reports availability without installing. Unix-only (needs `sh`
//! + `curl`); the CLI ships only macOS + Linux targets.

use std::process::Stdio;

/// Where CLI tarballs live, in preference order.
///
/// The source repo first. `svrnmesh-releases` is the retired public shelf
/// that existed only while the source repo was private (its release assets
/// were not anonymously fetchable); it still carries the newest published
/// `cli-v*` until the next release is cut here, and the source repo still
/// carries a stale `cli-v0.1.19` from July. So the answer is MAX SEMVER
/// ACROSS BOTH — never "the first repo that answers", which would offer
/// 0.1.19 to someone running 0.6.0. `landing/install.sh` resolves the
/// download the same way; drop both entries together.
const REPOS: &[&str] = &["alexsbryan/commonwealth-ai", "alexsbryan/svrnmesh-releases"];
const INSTALL_URL: &str = "https://svrnme.sh/install.sh";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

pub async fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return 0;
    }
    let check_only = args.iter().any(|a| a == "--check");

    println!("Installed: cli-v{CURRENT}");
    let (latest, from_repo) = match fetch_latest().await {
        Ok(v) => v,
        Err(e) => {
            // Surface the failure — do not pretend we're up to date (the exact
            // masking that hid the desktop updater bugs). Non-zero exit so
            // scripts can tell a failed check from "already current".
            eprintln!("update: couldn't check for updates: {e}");
            return 1;
        }
    };
    println!("Latest:    cli-v{latest}");
    if from_repo != REPOS[0] {
        // Never substitute silently: name the shelf when it is what answered.
        println!("           (from the retired {from_repo} shelf)");
    }

    if !is_newer(&latest, CURRENT) {
        println!("You're on the latest version.");
        return 0;
    }

    println!("\nAn update is available: cli-v{CURRENT} -> cli-v{latest}");
    if check_only {
        println!("Run `svrn update` to install it.");
        return 0;
    }
    install(&latest)
}

/// Resolve the newest published `cli-v*` by max semver (NOT list order),
/// across every repo in [`REPOS`]. Returns the version and the repo it came
/// from. A repo that fails is reported, not defaulted: the error only
/// surfaces if EVERY repo failed, so one dead shelf cannot hide a live
/// primary — and vice versa.
async fn fetch_latest() -> Result<(String, &'static str), String> {
    let client = reqwest::Client::builder()
        .user_agent("svrn-updater/1")
        .build()
        .map_err(|e| e.to_string())?;

    let mut best: Option<(String, &'static str)> = None;
    let mut first_err: Option<String> = None;
    for repo in REPOS {
        let versions = match fetch_cli_versions(&client, repo).await {
            Ok(v) => v,
            Err(e) => {
                first_err.get_or_insert(format!("{repo}: {e}"));
                continue;
            }
        };
        for v in versions {
            // Strictly newer, so a tie keeps the earlier (preferred) repo.
            if best.as_ref().is_none_or(|(b, _)| is_newer(&v, b)) {
                best = Some((v, repo));
            }
        }
    }

    if let Some(found) = best {
        // A partial failure is still a failure to SEE, and swallowing it turns
        // "we could not read the primary" into "you are up to date". GitHub's
        // unauthenticated rate limit trips the primary while the retired shelf
        // still answers with an older tag; without this line the user is told
        // they are current against a shelf that stopped moving.
        if let Some(err) = &first_err {
            eprintln!("note: could not read every release shelf, so this answer may be stale.");
            eprintln!("      {err}");
        }
        return Ok(found);
    }
    Err(first_err
        .unwrap_or_else(|| format!("no published cli-v* release found in {}", REPOS.join(", "))))
}

/// Every non-draft `cli-v*` version published in one repo.
async fn fetch_cli_versions(client: &reqwest::Client, repo: &str) -> Result<Vec<String>, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=30");
    let resp = client
        .get(&url)
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("github api returned {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let arr = body.as_array().ok_or("unexpected github response shape")?;

    let mut out = Vec::new();
    for r in arr {
        if r.get("draft")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            continue; // never roll out a partial cut
        }
        let Some(tag) = r.get("tag_name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(v) = tag.strip_prefix("cli-v") else {
            continue; // skip desktop-v* / vscode-v* on the shared stream
        };
        out.push(v.to_string());
    }
    Ok(out)
}

/// Re-run the canonical installer, pinned to the resolved version so we
/// install exactly what we checked (install.sh would otherwise do its own
/// 'latest' lookup — a second, racy resolution). Install into the directory
/// the running binary lives in, so the update lands where the CLI is actually
/// installed rather than the installer's `~/.local/bin` default.
fn install(version: &str) -> i32 {
    println!("Installing cli-v{version} …\n");
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(format!("curl -fsSL {INSTALL_URL} | sh"))
        .env("SVRNMESH_VERSION", format!("cli-v{version}"))
        .stdin(Stdio::null());
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cmd.env("SVRNMESH_INSTALL_DIR", dir);
        }
    }
    match cmd.status() {
        Ok(s) if s.success() => {
            println!(
                "\nUpdated to cli-v{version}. Restart any running `svrn` sessions \
                 or the daemon (`svrn daemon restart`) to pick it up."
            );
            0
        }
        Ok(s) => {
            eprintln!("update: installer exited with status {s}");
            1
        }
        Err(e) => {
            eprintln!("update: failed to launch installer (need `sh` + `curl` on PATH): {e}");
            1
        }
    }
}

fn is_newer(a: &str, b: &str) -> bool {
    parse(a) > parse(b)
}

/// Parse the semver CORE (`major.minor.patch`) into a comparable tuple.
/// Pre-release suffixes are ignored — the CLI ships only release tags, and
/// tuple ordering matches install.sh's numeric per-component sort so the two
/// resolvers never disagree on which `cli-v*` is newest.
fn parse(v: &str) -> (u64, u64, u64) {
    let core = v.split('-').next().unwrap_or(v);
    let mut it = core.split('.');
    let n = |x: Option<&str>| x.and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    (n(it.next()), n(it.next()), n(it.next()))
}

fn print_help() {
    println!(
        "svrn update — check for and install a newer CLI release\n\n\
         Usage:\n  \
         svrn update            Check, then install if a newer version exists\n  \
         svrn update --check    Report availability without installing\n\n\
         Resolves the newest cli-v* in the public release repo by version and\n\
         re-installs via the same checksum-verified installer as `curl … | sh`,\n\
         into wherever the running binary lives. Unix only (needs sh + curl)."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The source repo must come FIRST: `fetch_latest` breaks ties in favour
    /// of the earlier entry, and the shelf is the one being retired.
    #[test]
    fn source_repo_is_the_preferred_release_repo() {
        assert_eq!(REPOS[0], "alexsbryan/commonwealth-ai");
        assert!(
            REPOS.len() <= 2,
            "REPOS is a two-entry transition list, not a registry: {REPOS:?}"
        );
    }

    #[test]
    fn semver_core_ordering() {
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("0.1.20", "0.1.9")); // numeric, not lexical
        assert!(is_newer("0.2.0", "0.1.20"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.2.0", "0.2.1"));
    }

    #[test]
    fn parse_tolerates_prerelease_and_short() {
        assert_eq!(parse("0.2.1"), (0, 2, 1));
        assert_eq!(parse("0.2.1-rc1"), (0, 2, 1));
        assert_eq!(parse("1.2"), (1, 2, 0));
    }
}
