// SPDX-License-Identifier: AGPL-3.0-or-later
//! The env leg of a client's daemon dial: which daemon it talks to, and the
//! key it presents. A sibling of `setup_config.rs`, which is at its arch-gate
//! ceiling; re-exported there, so every path and name is unchanged.

/// The env leg of [`super::client_daemon_base`], isolated so the precedence is
/// testable without a config file and so there is ONE answer to "which
/// spelling of the knob counts, and what counts as set".
///
/// `SOVEREIGN_` first, then `SVRNMESH_`, for parity with every other reader of
/// the pair (the boot bridge maps the legacy prefix forward, so both arrive).
///
/// A set-but-blank value is treated as UNSET rather than as an empty base URL:
/// `SOVEREIGN_DAEMON_URL= svrn enrich` should fall through to the config, not
/// dispatch at a bare `/v1/chat/completions`. Trailing slashes are trimmed
/// because every caller appends `/v1/…`, and `http://h:9841//v1/models` is a
/// different route to a strict router than `http://h:9841/v1/models`.
pub fn daemon_url_override() -> Option<String> {
    first_set_env(["SOVEREIGN_DAEMON_URL", "SVRNMESH_DAEMON_URL"], |v| {
        v.trim_end_matches('/')
    })
}

/// The first of `keys` whose value, trimmed then `normalize`d, is non-empty:
/// the one rule [`daemon_url_override`] and [`client_credential`] read by.
fn first_set_env(keys: [&str; 2], normalize: fn(&str) -> &str) -> Option<String> {
    keys.iter().find_map(|key| {
        let raw = std::env::var(key).ok()?;
        let trimmed = normalize(raw.trim());
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

/// The credential a CLI client presents to the daemon, or `None`. THE one
/// accessor for the CLI's credential: a daemon under `loopback = "none"` (an
/// on-prem box) admits no caller without a key, loopback included, so IT
/// exports its admin key here rather than driving the API with curl; a named
/// client presents its own (`svrn daemon key --add <name>`).
///
/// `SOVEREIGN_API_KEY`, then `SVRNMESH_API_KEY`, blank counting as unset, by
/// the same rule as [`daemon_url_override`]. Distinct from the daemon-side
/// `SOVEREIGN_CLIENT_TOKEN`, which is the token a daemon ADMITS, not one a
/// client presents.
pub fn client_credential() -> Option<String> {
    first_set_env(["SOVEREIGN_API_KEY", "SVRNMESH_API_KEY"], |v| v)
}
