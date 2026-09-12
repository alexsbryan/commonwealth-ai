// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which build is answering, and whether it is older than the caller.
//!
//! A CLI and the daemon it talks to are two binaries that are rebuilt
//! independently, and nothing in an HTTP answer says which code produced it.
//! When they disagree the symptom is a bare `404` on a running, healthy
//! daemon — the route exists in the tree, in the CLI, and in `git log`, and
//! the process serving :9741 predates all three. Seen for real on 2026-09-11:
//! the daemon started 23:06 the previous night, `mesh_http.rs` grew
//! `POST /v1/mesh/media/fanout` at 12:54, the binary was rebuilt at 15:22,
//! and the verb answered `404` with an empty body.
//!
//! Both sides read `0.6.0` in that incident — every member of this workspace
//! is `version.workspace = true` — so a version comparison would have called
//! it a match. `exe_mtime` is the field that separates them, and it is the
//! one this module decides on.

use serde::{Deserialize, Serialize};

/// Which build is answering, as published on `GET /status`
/// (`commonwealth_api::routes_status::ProcessStatus::build`) and minted by
/// `sovereign_core::run_identity::stamp`.
///
/// Published because only the answering process can know it: a port names a
/// listener, never the code behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildStamp {
    /// `CARGO_PKG_VERSION` of the crate that minted the stamp. Carried for
    /// the release case; see the module docs for why it decides nothing here.
    pub version: String,
    /// The answering generation's `run_id` — the join key to its log lines.
    pub run_id: String,
    /// `current_exe()`, or the reason it could not be read.
    pub exe: String,
    /// The binary's mtime AS LOADED, RFC 3339. Captured once at first use
    /// (daemon startup), so it names the code actually running, not whatever
    /// is on disk now. `None` when the metadata could not be read — reported
    /// as absent, never defaulted to "current" (ARCH §6).
    pub exe_mtime: Option<String>,
}

/// What comparing a far side's [`BuildStamp`] against the caller's own
/// concluded. Four verdicts, not two: the two that make no claim are
/// reported rather than collapsed into "fine" (ARCH §5, §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skew {
    /// The far side answered a stamp naming a binary built BEFORE the
    /// caller's, by `older_by_secs`.
    RemoteOlder {
        /// Seconds by which the far side's binary mtime precedes the caller's.
        older_by_secs: i64,
    },
    /// Both stamps parsed and the far side's binary is not older. A failure
    /// observed alongside this verdict is NOT explained by skew.
    RemoteNotOlder,
    /// The far side answered without a stamp at all — it predates this
    /// field, which is conclusive on its own: it is older than any binary
    /// that carries one.
    RemotePredatesTheStamp,
    /// No comparison was possible; `why` names the half that was missing.
    /// Kept distinct from [`Skew::RemoteNotOlder`] on purpose — "did not
    /// answer" is not "answered: no".
    Undetermined {
        /// Which half was missing, in a form a person can read.
        why: String,
    },
}

impl BuildStamp {
    /// Compare a far side's stamp against this one (the caller's).
    ///
    /// `remote: None` is [`Skew::RemotePredatesTheStamp`], not an error: an
    /// answer with no stamp is itself evidence about the answerer's age. An
    /// unreadable or unparseable mtime on either side is
    /// [`Skew::Undetermined`], never a silent pass.
    pub fn compare(&self, remote: Option<&BuildStamp>) -> Skew {
        let Some(remote) = remote else {
            return Skew::RemotePredatesTheStamp;
        };
        let parse = |side: &str, v: &Option<String>| match v {
            None => Err(format!("the {side}'s binary mtime was unreadable")),
            Some(s) => chrono::DateTime::parse_from_rfc3339(s)
                .map(|t| t.timestamp())
                .map_err(|e| format!("the {side}'s binary mtime did not parse ({e})")),
        };
        let (r, l) = match (
            parse("far side", &remote.exe_mtime),
            parse("caller", &self.exe_mtime),
        ) {
            (Ok(r), Ok(l)) => (r, l),
            (Err(why), _) | (_, Err(why)) => return Skew::Undetermined { why },
        };
        if r < l {
            Skew::RemoteOlder {
                older_by_secs: l - r,
            }
        } else {
            Skew::RemoteNotOlder
        }
    }

    /// One line naming this build, for an error a person reads.
    ///
    /// The mtime is trimmed to seconds HERE rather than at the source: the
    /// wire carries whatever precision `current_exe()`'s metadata had (nine
    /// fractional digits on Linux), and the same string is what
    /// [`BuildStamp::compare`] parses. Rounding it upstream would trade a
    /// comparison's input for a reader's comfort.
    pub fn one_line(&self) -> String {
        let mtime = match &self.exe_mtime {
            None => "<mtime unreadable>".to_string(),
            Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
                Ok(t) => t
                    .with_timezone(&chrono::Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                // Unparseable is shown verbatim: it is what the far side
                // actually said, and `compare` will abstain on it anyway.
                Err(_) => s.clone(),
            },
        };
        format!(
            "{} built {} · run {} · {}",
            self.version, mtime, self.run_id, self.exe
        )
    }
}

/// Why a route that exists in this binary was not found on the far side, as
/// far as the build stamps can say — the four verdicts rendered for a person
/// staring at a `404`.
///
/// Every arm names what was observed before it names a repair, and the two
/// that cannot blame skew say so rather than suggesting a restart that will
/// not help. Suggesting `daemon restart` for a route that genuinely does not
/// exist sends the reader round a loop that ends where it started.
pub fn explain_route_missing(
    route: &str,
    local: &BuildStamp,
    remote: Option<&BuildStamp>,
) -> String {
    let mut out = format!("the daemon has no route {route}.\n");
    out.push_str(&format!("  this CLI: {}\n", local.one_line()));
    match remote {
        Some(r) => out.push_str(&format!("  daemon:   {}\n", r.one_line())),
        None => out.push_str("  daemon:   no build stamp on /status — it predates the field\n"),
    }
    match local.compare(remote) {
        Skew::RemotePredatesTheStamp => out.push_str(
            "The daemon is running code from before this CLI's, by at least as long as \
             the build stamp has existed. Restart it: `svrn daemon restart`.\n",
        ),
        Skew::RemoteOlder { older_by_secs } => out.push_str(&format!(
            "The daemon's binary is {} older than this one, so it is running code from \
             before this route existed. Restart it: `svrn daemon restart`.\n",
            humanize_secs(older_by_secs)
        )),
        Skew::RemoteNotOlder => out.push_str(
            "The daemon's binary is not older than this one, so this is NOT version skew \
             — that daemon genuinely has no such route. Check the path.\n",
        ),
        Skew::Undetermined { why } => out.push_str(&format!(
            "Whether this is version skew could not be determined: {why}. That is not the \
             same as ruling it out — `svrn daemon restart` and retry before looking further.\n"
        )),
    }
    out
}

/// `93784` → `1d 2h`. Coarse on purpose: the reader is deciding whether a
/// daemon is a rebuild behind, not timing anything.
pub fn humanize_secs(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(mtime: Option<&str>) -> BuildStamp {
        BuildStamp {
            version: "0.6.0".into(),
            run_id: "4f2a1b9c".into(),
            exe: "/t/sovereign-cli-daemon".into(),
            exe_mtime: mtime.map(str::to_string),
        }
    }

    /// The 2026-09-11 incident, as the failing input this check exists for:
    /// both sides say 0.6.0, the daemon was loaded 16h 16m before the CLI was
    /// built, and the verdict has to be RemoteOlder anyway (ARCH §5).
    #[test]
    fn the_incident_that_minted_this_reads_as_skew_with_both_versions_equal() {
        let cli = at(Some("2026-09-11T15:22:07+00:00"));
        let daemon = at(Some("2026-09-10T23:06:14+00:00"));
        assert_eq!(cli.version, daemon.version);
        assert_eq!(
            cli.compare(Some(&daemon)),
            Skew::RemoteOlder {
                older_by_secs: 58_553
            }
        );
        let msg = explain_route_missing("POST /v1/mesh/media/fanout", &cli, Some(&daemon));
        assert!(msg.contains("16h 15m older"), "{msg}");
        assert!(msg.contains("svrn daemon restart"), "{msg}");
    }

    /// An answer with no stamp is evidence, not an error: it can only come
    /// from a daemon older than the field itself.
    #[test]
    fn a_daemon_with_no_stamp_is_older_not_unknown() {
        let cli = at(Some("2026-09-11T15:22:07+00:00"));
        assert_eq!(cli.compare(None), Skew::RemotePredatesTheStamp);
        let msg = explain_route_missing("POST /v1/mesh/media/fanout", &cli, None);
        assert!(msg.contains("predates the field"), "{msg}");
        assert!(msg.contains("svrn daemon restart"), "{msg}");
    }

    /// The arm that must NOT blame skew. Same build on both sides: the 404 is
    /// real, and telling the reader to restart would send them in a circle.
    #[test]
    fn a_daemon_no_older_than_the_cli_is_not_blamed_for_skew() {
        let cli = at(Some("2026-09-11T15:22:07+00:00"));
        let daemon = at(Some("2026-09-11T15:22:09+00:00"));
        assert_eq!(cli.compare(Some(&daemon)), Skew::RemoteNotOlder);
        let msg = explain_route_missing("GET /v1/mesh/nope", &cli, Some(&daemon));
        assert!(msg.contains("NOT version skew"), "{msg}");
        assert!(!msg.contains("svrn daemon restart"), "{msg}");
    }

    /// An unreadable mtime abstains; it does not pass. "Could not judge" is
    /// its own verdict and the message says the question is still open.
    #[test]
    fn an_unreadable_mtime_abstains_rather_than_passing() {
        let cli = at(Some("2026-09-11T15:22:07+00:00"));
        assert!(matches!(
            cli.compare(Some(&at(None))),
            Skew::Undetermined { .. }
        ));
        assert!(matches!(
            at(None).compare(Some(&at(Some("2026-09-11T15:22:07+00:00")))),
            Skew::Undetermined { .. }
        ));
        assert!(matches!(
            cli.compare(Some(&at(Some("not-a-date")))),
            Skew::Undetermined { .. }
        ));
        let msg = explain_route_missing("GET /x", &cli, Some(&at(None)));
        assert!(msg.contains("could not be determined"), "{msg}");
    }

    /// The mtime a real `current_exe()` carries has nine fractional digits;
    /// the line a person reads must not. Verbatim for anything that does not
    /// parse — that is what the far side said.
    #[test]
    fn one_line_trims_the_mtime_to_seconds_and_keeps_the_wire_precision() {
        let b = at(Some("2026-09-12T16:06:52.084289973+00:00"));
        assert!(
            b.one_line().contains("built 2026-09-12T16:06:52Z"),
            "{}",
            b.one_line()
        );
        assert_eq!(
            b.exe_mtime.as_deref(),
            Some("2026-09-12T16:06:52.084289973+00:00"),
            "the wire value is untouched — `compare` parses it"
        );
        assert!(at(Some("not-a-date")).one_line().contains("not-a-date"));
        assert!(at(None).one_line().contains("<mtime unreadable>"));
    }

    #[test]
    fn humanize_reads_at_the_granularity_a_rebuild_is_judged_at() {
        assert_eq!(humanize_secs(0), "0s");
        assert_eq!(humanize_secs(45), "45s");
        assert_eq!(humanize_secs(600), "10m");
        assert_eq!(humanize_secs(58_553), "16h 15m");
        assert_eq!(humanize_secs(93_784), "1d 2h");
        assert_eq!(humanize_secs(-5), "0s");
    }

    /// The wire is the contract: a daemon that grows a field must not break a
    /// client built before it, and a client must not silently invent one.
    #[test]
    fn an_older_daemons_status_parses_with_the_stamp_absent() {
        #[derive(Deserialize)]
        struct ProcessHead {
            #[serde(default)]
            build: Option<BuildStamp>,
        }
        let old: ProcessHead =
            serde_json::from_str(r#"{"pid":1,"run_id":"ab","uptime_seconds":3}"#).unwrap();
        assert!(old.build.is_none());
        let new: ProcessHead = serde_json::from_str(
            r#"{"pid":1,"build":{"version":"0.6.0","run_id":"ab","exe":"/t/d","exe_mtime":null}}"#,
        )
        .unwrap();
        assert_eq!(new.build.unwrap().version, "0.6.0");
    }
}
