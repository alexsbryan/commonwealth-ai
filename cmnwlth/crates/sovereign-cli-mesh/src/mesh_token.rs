// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh token` — retired 2026-10-08. Refuses by name and points at the
//! verb that replaced it.
//!
//! A named client token and an on-prem API key were two record kinds, two
//! CLIs and two resolution arms for one thing: a credential with a name
//! (ADDRESSED_TEXT §5.5 rule 2, D5). They are one record now,
//! `{name, token, groups}`, and `svrn daemon key` is its one verb. The
//! credential belongs to the daemon's client API, not to the mesh: a coding
//! harness on this machine or a lawyer behind an on-prem box involves no mesh
//! at all. `svrn daemon key` also works with no daemon running, which an
//! on-prem install needs before its first keyed start.
//!
//! This is a refusal rather than an alias: a verb that silently did the other
//! verb's work would keep the old name alive in every script that calls it.

/// What the retired verb says, every time, whatever it was given.
const RETIRED: &str = "`svrn mesh token` is retired: a named credential is the daemon's now, \
and `svrn daemon key` is its one verb.\n\
\n  svrn daemon key --add <name>        (was: svrn mesh token --new <label>)\
\n  svrn daemon key --list              (was: svrn mesh token --list)\
\n  svrn daemon key --revoke <name>     (was: svrn mesh token --revoke <label>)\n\
\nEvery token minted with `svrn mesh token` still works and is listed there under its label.";

pub(crate) async fn cmd_token(args: &[String]) -> i32 {
    tracing::debug!(?args, "mesh token: retired verb invoked");
    eprintln!("{RETIRED}");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal names the survivor for every flag the old verb took.
    #[tokio::test]
    async fn the_retired_verb_refuses_and_names_its_successor() {
        for args in [
            &["--new", "laptop"][..],
            &["--list"],
            &["--revoke", "x"],
            &["--help"],
        ] {
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            assert_eq!(cmd_token(&args).await, 2, "{args:?}");
        }
        for successor in [
            "daemon key --add",
            "daemon key --list",
            "daemon key --revoke",
        ] {
            assert!(RETIRED.contains(successor), "{successor}");
        }
    }
}
