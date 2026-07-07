// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hand-rolled argument parsing — no `clap`, to keep the dependency budget at
//! exactly `oicp-types + serde/reqwest/tokio`.

/// Parsed CLI options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    /// `--host <url>` — the OICP host base URL (e.g. `http://127.0.0.1:9741`).
    pub host: String,
    /// `--token <bearer>` — bearer token for a non-loopback host.
    pub token: Option<String>,
    /// `--fixture-recipe <id>` — a recipe id the host can install; unlocks the
    /// `ingest.state_machine` and `ingest.recipe_test` checks.
    pub fixture_recipe: Option<String>,
    /// `--report <path>` — write the JSON report artifact here.
    pub report: Option<String>,
    /// `--baseline <dir>` — diff against `<dir>/latest.json`; a regression fails.
    pub baseline: Option<String>,
    /// `--update-baseline` — write the run as the new baseline (dated + latest).
    pub update_baseline: bool,
    /// `--strict` — a `should`-level failure also fails the run.
    pub strict: bool,
    /// `--check <prefix>` — only run checks whose id starts with this prefix.
    pub check_prefix: Option<String>,
}

pub const USAGE: &str = "\
oicp-conformance — certify an OICP v0.4 host

USAGE:
    oicp-conformance --host <url> [options]

OPTIONS:
    --host <url>            OICP host base URL (required), e.g. http://127.0.0.1:9741
    --token <bearer>        Bearer token for a non-loopback host
    --fixture-recipe <id>   Recipe id to exercise the ingest checks
    --report <path>         Write the JSON report artifact to <path>
    --baseline <dir>        Diff against <dir>/latest.json; a regression fails the run
    --update-baseline       Write this run as the new baseline (dated + latest.json)
    --strict                A `should`-level failure also fails the run
    --check <prefix>        Only run checks whose id starts with <prefix>
    -h, --help              Print this help
";

/// Parse args (already skipping argv[0]). `Err` carries a user-facing message;
/// `Ok(None)` means help was requested.
pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Option<Args>, String> {
    let mut a = Args::default();
    let mut it = argv.into_iter();
    while let Some(arg) = it.next() {
        let mut take = |flag: &str| -> Result<String, String> {
            it.next().ok_or_else(|| format!("{flag} requires a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--host" => a.host = take("--host")?,
            "--token" => a.token = Some(take("--token")?),
            "--fixture-recipe" => a.fixture_recipe = Some(take("--fixture-recipe")?),
            "--report" => a.report = Some(take("--report")?),
            "--baseline" => a.baseline = Some(take("--baseline")?),
            "--update-baseline" => a.update_baseline = true,
            "--strict" => a.strict = true,
            "--check" => a.check_prefix = Some(take("--check")?),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.host.is_empty() {
        return Err("--host <url> is required".to_string());
    }
    // Normalize: drop any trailing slash so URL joins are clean.
    a.host = a.host.trim_end_matches('/').to_string();
    Ok(Some(a))
}

/// A host is loopback when it targets localhost — the auth checks skip there
/// (loopback is unauthenticated by the standard client-port posture).
pub fn is_loopback(host: &str) -> bool {
    let h = host
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    h.starts_with("127.0.0.1") || h.starts_with("localhost") || h.starts_with("[::1]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Result<Option<Args>, String> {
        parse(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_a_full_command() {
        let a = args(&[
            "--host",
            "http://peer:9741/",
            "--token",
            "secret",
            "--fixture-recipe",
            "sample",
            "--baseline",
            "base",
            "--update-baseline",
            "--strict",
            "--check",
            "manifest",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(a.host, "http://peer:9741"); // trailing slash stripped
        assert_eq!(a.token.as_deref(), Some("secret"));
        assert_eq!(a.fixture_recipe.as_deref(), Some("sample"));
        assert_eq!(a.baseline.as_deref(), Some("base"));
        assert!(a.update_baseline);
        assert!(a.strict);
        assert_eq!(a.check_prefix.as_deref(), Some("manifest"));
    }

    #[test]
    fn host_is_required() {
        assert!(args(&["--strict"]).is_err());
    }

    #[test]
    fn help_returns_none() {
        assert!(args(&["--help"]).unwrap().is_none());
    }

    #[test]
    fn a_flag_missing_its_value_errors() {
        assert!(args(&["--host"]).is_err());
    }

    #[test]
    fn unknown_arg_errors() {
        assert!(args(&["--host", "h", "--nope"]).is_err());
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("http://127.0.0.1:9741"));
        assert!(is_loopback("http://localhost:9741"));
        assert!(!is_loopback("http://peer.local:9741"));
        assert!(!is_loopback("https://example.com"));
    }
}
