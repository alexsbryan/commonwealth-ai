// SPDX-License-Identifier: AGPL-3.0-or-later
//! How faithfully the daemon serves the turn it was given.
//!
//! `POST /v1/chat/completions` is advertised as OpenAI-compatible, and
//! a client on that route is entitled to assume the conversation it
//! sent is the conversation the model sees. Several passes in
//! [`crate::frontdoor`] do not hold that assumption: some APPEND a
//! synthetic message, some REWRITE an emitted tool call, and one
//! installs a token-level sampler constraint. Each was cut against a
//! real failure and each is defensible — but collectively they are the
//! difference between this daemon and bare llama.cpp, and an operator
//! running a shared anchor node should be able to see and set that
//! difference in one place rather than by reading a 5,800-line module.
//!
//! This module is that one place. Two switches, opposite defaults,
//! each named for what it governs.

/// Whether the daemon may SYNTHESISE a citation allowlist from the
/// conversation and install it as a sampler constraint. Default OFF;
/// opt in with `SOVEREIGN_FRONTDOOR_AUTO_ALLOWLIST=1`.
///
/// The accumulators this gates turn URLs and `ev-Tn-NNNN` handles seen
/// in prior `role: tool` messages into token masks. That is right for a
/// retrieval-synthesis turn and wrong for a general OpenAI client: the
/// constraint cannot tell "fabricating a sibling URL" from "writing the
/// URL the user just asked for", so one `docs.rust-lang.org` link in a
/// cargo error was enough to make every other URL unreachable — 200 OK,
/// wrong bytes, no signal (ARCH §18.3). Measurement, flip condition and
/// review date: `sovereign/DEFAULTS_LEDGER.md`.
///
/// A caller-supplied allowlist is unaffected either way — an explicit
/// one has always won over the synthesis. This flag governs only
/// whether the daemon invents one on the caller's behalf, which is why
/// deep-research and the search gym keep their constraint with it off.
pub fn auto_allowlist_enabled() -> bool {
    opt_in(
        std::env::var("SOVEREIGN_FRONTDOOR_AUTO_ALLOWLIST")
            .ok()
            .as_deref(),
    )
}

/// Pure half of [`auto_allowlist_enabled`]: absent or unrecognised is
/// OFF. Split out so the accepted spellings are tested without a test
/// mutating process env underneath every other test in the binary.
fn opt_in(raw: Option<&str>) -> bool {
    matches!(raw, Some(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Whether the runtime reshape passes may alter a chat turn. Default
/// ON; `SOVEREIGN_FRONTDOOR_RESHAPE=0` serves the conversation and the
/// model's output through unmodified.
///
/// Governs, on the request, the three runtime nudges — each APPENDS a
/// synthetic message the client never sent — and, on the response, the
/// heredoc and absolute-path canonicalizers, which REWRITE arguments of
/// a tool call the model already emitted.
///
/// Defaults on because all five key on the Codex/opencode contract
/// (`exec_command` calls, the literal `Process exited with code N`
/// result shape) and each was cut against a named gym fixture, so a
/// client speaking a different tool vocabulary never trips them. It is
/// nonetheless a switch because they are the only remaining passes that
/// make a locally-served turn differ from bare llama.cpp, and an
/// operator running a shared anchor node should not have to read this
/// file to discover they exist. Every firing logs at INFO, so ON is
/// auditable and OFF is total.
///
/// NOT governed: [`crate::frontdoor::promote_in_content_tool_call`], which lifts a tool
/// call the model emitted as content into the structured field. That
/// RECOVERS the model's intent rather than overriding it — off, the
/// call is silently lost, which is less faithful, not more.
pub fn reshape_enabled() -> bool {
    opt_out(std::env::var("SOVEREIGN_FRONTDOOR_RESHAPE").ok().as_deref())
}

/// Pure half of [`reshape_enabled`]: absent is ON, and only an
/// explicit off-spelling turns it off. An unrecognised value stays ON
/// rather than silently disabling the passes — a typo must not quietly
/// change what the daemon serves.
fn opt_out(raw: Option<&str>) -> bool {
    !matches!(
        raw,
        Some(v) if v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")
    )
}

#[cfg(test)]
mod switch_tests {
    use super::{opt_in, opt_out};

    #[test]
    fn allowlist_synthesis_is_off_unless_explicitly_asked_for() {
        assert!(!opt_in(None), "absent means off — see DEFAULTS_LEDGER.md");
        assert!(!opt_in(Some("")));
        assert!(!opt_in(Some("0")));
        assert!(
            !opt_in(Some("yes")),
            "only 1/true, so a typo cannot arm a sampler constraint"
        );
        assert!(opt_in(Some("1")));
        assert!(opt_in(Some("true")));
        assert!(opt_in(Some("TRUE")));
    }

    #[test]
    fn reshape_is_on_unless_explicitly_turned_off() {
        assert!(opt_out(None), "absent means on");
        assert!(!opt_out(Some("0")));
        assert!(!opt_out(Some("false")));
        assert!(!opt_out(Some("off")));
        assert!(!opt_out(Some("OFF")));
        assert!(opt_out(Some("1")));
        assert!(
            opt_out(Some("no")),
            "an unrecognised value must not silently change what the daemon serves"
        );
    }
}
