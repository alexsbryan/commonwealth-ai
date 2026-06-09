//! `DesktopError` — the structured error for Tauri command handlers
//! (§2.1 typed dispatch, §9 glassbox). It replaces the ad-hoc
//! `.map_err(|e| e.to_string())` pattern that left ~295 handlers
//! returning bare `String`s the frontend could only render verbatim.
//!
//! A migrated handler returns `Result<T, DesktopError>`; the error
//! serialises to a stable `{ code, message, suggested_action }` wire
//! shape (pinned by the tests below) that the frontend can *branch* on
//! — e.g. show a "still loading" affordance for `not_ready` vs a toast
//! for `upstream`. `From<String>` / `From<&str>` map any legacy stringly
//! error to `Internal`, so a handler can flip to `DesktopError` while its
//! neighbours still return `String` and `?` keeps compiling across the
//! seam. Migration is therefore per-handler and incremental, never a
//! single ~295-site sweep.

use serde::Serialize;

/// Machine-branchable error category. snake_case on the wire (mirrors
/// `knowledge_view::view_kind`'s id convention) so the frontend's
/// `ErrorCode` union can match string-for-string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// A subsystem (runtime / store / corpus) isn't ready yet — almost
    /// always transient during bootstrap. The UI should suggest waiting.
    NotReady,
    /// The request was malformed or named something that doesn't exist
    /// (unknown id, disabled feature). Not retryable as-is.
    InvalidRequest,
    /// A downstream dependency failed — a mesh peer, the web-search
    /// backend, a model. Often retryable.
    Upstream,
    /// Catch-all / not-yet-categorised. `From<String>`/`From<&str>` land
    /// legacy stringly errors here so they keep flowing through one path.
    Internal,
}

/// Structured handler error. `suggested_action` is a short, user-facing
/// next step (empty when there's nothing actionable to say).
#[derive(Debug, Clone, Serialize)]
pub struct DesktopError {
    pub code: ErrorCode,
    pub message: String,
    pub suggested_action: String,
}

impl DesktopError {
    /// Construct with an explicit code and no suggested action.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            suggested_action: String::new(),
        }
    }

    /// `NotReady` with the standard "wait" affordance.
    pub fn not_ready(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotReady, message)
            .with_action("Wait for setup to finish, then try again.")
    }

    /// `InvalidRequest` — the request named something missing/disabled.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message)
    }

    /// `Upstream` — a downstream dependency (peer, search, model) failed.
    pub fn upstream(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Upstream, message)
    }

    /// `Internal` — uncategorised failure.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// Builder: attach a user-facing suggested next step.
    pub fn with_action(mut self, action: impl Into<String>) -> Self {
        self.suggested_action = action.into();
        self
    }
}

impl std::fmt::Display for DesktopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Display is the message alone — the code/action are structured
        // fields for the UI, not part of the human sentence.
        f.write_str(&self.message)
    }
}

impl std::error::Error for DesktopError {}

impl From<String> for DesktopError {
    fn from(message: String) -> Self {
        Self::internal(message)
    }
}

impl From<&str> for DesktopError {
    fn from(message: &str) -> Self {
        Self::internal(message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape is a contract with the frontend `DesktopError`
    /// type + `invokeChecked` wrapper — pin it so a field rename or a
    /// serde-casing change can't drift silently.
    #[test]
    fn serializes_to_the_stable_wire_shape() {
        let v = serde_json::to_value(DesktopError::not_ready("loading")).unwrap();
        assert_eq!(v["code"], "not_ready");
        assert_eq!(v["message"], "loading");
        assert_eq!(
            v["suggested_action"], "Wait for setup to finish, then try again.",
            "suggested_action must always be present (stable shape)"
        );
    }

    #[test]
    fn every_code_serializes_to_snake_case() {
        let code = |e: DesktopError| serde_json::to_value(e).unwrap()["code"].clone();
        assert_eq!(code(DesktopError::not_ready("")), "not_ready");
        assert_eq!(code(DesktopError::invalid_request("")), "invalid_request");
        assert_eq!(code(DesktopError::upstream("")), "upstream");
        assert_eq!(code(DesktopError::internal("")), "internal");
    }

    #[test]
    fn legacy_string_errors_become_internal() {
        // The migration seam: `?` on a String error in a DesktopError fn
        // routes through here. Must stay Internal with the text intact.
        let e: DesktopError = "boom".to_string().into();
        assert_eq!(e.code, ErrorCode::Internal);
        assert_eq!(e.message, "boom");
        assert_eq!(e.suggested_action, "");

        let e2: DesktopError = "bare str".into();
        assert_eq!(e2.code, ErrorCode::Internal);
        assert_eq!(e2.message, "bare str");
    }

    #[test]
    fn display_is_the_message_only() {
        let e = DesktopError::upstream("peer offline").with_action("retry");
        assert_eq!(e.to_string(), "peer offline");
    }
}
