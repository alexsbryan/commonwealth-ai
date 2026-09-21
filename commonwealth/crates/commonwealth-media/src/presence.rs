// SPDX-License-Identifier: AGPL-3.0-or-later
//! Is the holder using their own library right now?
//!
//! The holder's daemon reads its origin's live sessions once per poll and this
//! module turns that document into the one number the mesh carries,
//! `NodeCapabilities::media_available`. Nothing here dials anything or knows
//! what a title is — the same split the rest of this crate keeps, so the
//! decision has one implementation and a test can state it without a server.
//!
//! # The rule
//!
//! A session belongs to the HOLDER when its `UserId` is not the mesh viewer's
//! — the read-only user `svrn mesh media offer` created and declared. A holder
//! session with something in `NowPlayingItem` means the library is in use:
//! [`IN_USE`]. Anything else is [`FREE`].
//!
//! Viewers are deliberately NOT counted. Every member reaching this library
//! arrives as that one viewer user, so counting their playback would make a
//! second housemate's stream look like the holder's own and close the library
//! to the house the moment one person watched anything.
//!
//! # Why a refusal is not `FREE`
//!
//! An origin that cannot be asked — down, moved, credential rotated — answers
//! [`PresenceError`], and the caller publishes `None`. `None` is "nobody
//! answered", never "free": a viewer must not start a stream on the strength
//! of a missing answer (ARCH principle 6).

use serde::Deserialize;

/// The holder is watching their own library: a member is shown "in use right
/// now" and starts nothing.
pub const IN_USE: f32 = 0.0;

/// Nothing of the holder's is playing; the library is free.
pub const FREE: f32 = 1.0;

/// Why the sessions document could not be read into a verdict.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PresenceError {
    /// The body was not the JSON array of sessions the origin documents.
    #[error("sessions document is not a JSON array of sessions: {0}")]
    NotSessions(String),
}

/// One row of the origin's sessions document, with only the two fields the
/// rule reads. Unknown fields are ignored: the origin owns this shape and a
/// new field of its own must not turn a live library into a refusal.
#[derive(Debug, Deserialize)]
struct Session {
    #[serde(rename = "UserId", default)]
    user_id: Option<String>,
    #[serde(rename = "NowPlayingItem", default)]
    now_playing_item: Option<serde_json::Value>,
}

impl Session {
    /// Whether this row is the holder playing something.
    fn is_holder_playing(&self, viewer_user_id: &str) -> bool {
        let playing = !matches!(self.now_playing_item, None | Some(serde_json::Value::Null));
        let theirs = self.user_id.as_deref() != Some(viewer_user_id);
        playing && theirs
    }
}

/// [`IN_USE`] when a session that is not the mesh viewer's is playing
/// something, [`FREE`] otherwise.
///
/// `body` is the origin's sessions document verbatim; `viewer_user_id` is the
/// read-only user the offer declared. A body that is not an array of sessions
/// is a [`PresenceError`], not a `FREE` — see the module note.
pub fn media_available_from_sessions(
    body: &str,
    viewer_user_id: &str,
) -> Result<f32, PresenceError> {
    let sessions: Vec<Session> =
        serde_json::from_str(body).map_err(|e| PresenceError::NotSessions(e.to_string()))?;
    let holder_playing = sessions.iter().any(|s| s.is_holder_playing(viewer_user_id));
    // The user ids beside the count, because the count alone cannot tell
    // "the holder is idle" from "the credential we asked with is only
    // allowed to see itself" — the defect this line was added to make
    // readable from one run (ARCH principle 1).
    let saw: Vec<&str> = sessions
        .iter()
        .map(|s| s.user_id.as_deref().unwrap_or("<none>"))
        .collect();
    tracing::debug!(
        target: "transport",
        sessions = sessions.len(),
        ?saw,
        %viewer_user_id,
        holder_playing,
        "media presence: read the origin's sessions"
    );
    Ok(if holder_playing { IN_USE } else { FREE })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWER: &str = "1111111111111111111111111111aaaa";
    const HOLDER: &str = "2222222222222222222222222222bbbb";

    fn session(user: &str, playing: bool) -> String {
        let item = if playing {
            r#"{"Name":"Commonwealth Test Pattern (2026)","Id":"abc"}"#
        } else {
            "null"
        };
        format!(r#"{{"UserId":"{user}","UserName":"x","NowPlayingItem":{item},"IsActive":true}}"#)
    }

    /// Positive: the holder watching their own library closes it.
    #[test]
    fn a_holder_session_with_a_now_playing_item_is_in_use() {
        let body = format!("[{}]", session(HOLDER, true));
        assert_eq!(
            media_available_from_sessions(&body, VIEWER),
            Ok(IN_USE),
            "a holder session playing something must read 0.0"
        );
    }

    /// Positive: an idle holder session leaves the library free.
    #[test]
    fn a_holder_session_playing_nothing_is_free() {
        let body = format!("[{}]", session(HOLDER, false));
        assert_eq!(media_available_from_sessions(&body, VIEWER), Ok(FREE));
    }

    /// Negative: the mesh viewer's own playback is not the holder's. Every
    /// member arrives as this one user, so counting it would close the library
    /// to the house the moment one housemate pressed play.
    #[test]
    fn the_mesh_viewers_playback_is_not_the_holder_using_it() {
        let body = format!("[{}]", session(VIEWER, true));
        assert_eq!(media_available_from_sessions(&body, VIEWER), Ok(FREE));
    }

    /// Positive: one holder among several viewers still closes it.
    #[test]
    fn one_holder_among_viewers_is_enough() {
        let body = format!(
            "[{},{},{}]",
            session(VIEWER, true),
            session(VIEWER, true),
            session(HOLDER, true)
        );
        assert_eq!(media_available_from_sessions(&body, VIEWER), Ok(IN_USE));
    }

    /// Positive: no sessions at all is free, not a refusal — an empty array is
    /// an answer.
    #[test]
    fn no_sessions_is_free() {
        assert_eq!(media_available_from_sessions("[]", VIEWER), Ok(FREE));
    }

    /// Positive: a field this build does not name does not turn a live
    /// library into a refusal.
    #[test]
    fn an_unknown_field_is_ignored_not_refused() {
        let body = r#"[{"UserId":"x","NowPlayingItem":null,"SomethingNew":{"a":1}}]"#;
        assert_eq!(media_available_from_sessions(body, VIEWER), Ok(FREE));
    }

    /// Negative: a body that is not a sessions array REFUSES. The caller
    /// publishes `None`, and `None` is never read as "free".
    #[test]
    fn a_body_that_is_not_sessions_refuses_rather_than_reading_free() {
        let got = media_available_from_sessions(r#"{"error":"unauthorized"}"#, VIEWER);
        assert!(
            matches!(got, Err(PresenceError::NotSessions(_))),
            "an origin that answered something else must not read as FREE, got {got:?}"
        );
    }
}
