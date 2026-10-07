// SPDX-License-Identifier: AGPL-3.0-or-later
//! How faithfully the daemon serves the turn it was given.
//!
//! `POST /v1/chat/completions` is advertised as OpenAI-compatible, and
//! a client on that route is entitled to assume the conversation it
//! sent is the conversation the model sees. Several passes in
//! `frontdoor` do not hold that assumption: some APPEND a
//! synthetic message, one DELETES history and REPLACES the caller's
//! system prompt, some REWRITE an emitted tool call, and one installs
//! a token-level sampler constraint. Each was cut against a real
//! failure and each is defensible — but collectively they are the
//! difference between this daemon and bare llama.cpp, and an operator
//! running a shared anchor node should be able to see and set that
//! difference in one place rather than by reading a 5,800-line module.
//!
//! What they do to a real turn is pinned, per fixture, by
//! `tests/main/turn_reshape_fidelity.rs`.
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
/// review date: `docs/DEFAULTS_LEDGER.md`.
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
/// OFF: the conversation and the model's output are served through
/// unmodified, as llama-server serves them; `SOVEREIGN_FRONTDOOR_RESHAPE=1`
/// turns the passes on.
///
/// Governs, on the request, the three runtime nudges, and on the
/// response the heredoc and absolute-path canonicalizers, which REWRITE
/// arguments of a tool call the model already emitted.
///
/// The nudges do more than append. Failure-recovery deletes the failed
/// call from history before injecting its banner, and read-attractor
/// deletes every read-classified tool_call/result pair, drops the
/// frontdoor's own compressed-history message, and replaces the
/// caller's system prompt with a write-mandate. Measured on the
/// committed gym turns: fixture 007 arrives with twelve messages and
/// reaches the model with four. Off, all five are skipped and the
/// conversation is served through as sent.
///
/// The three share an idempotency gate, so exactly one fires per turn.
/// That ordering is load-bearing rather than incidental — on the only
/// committed turn whose tail repeats, failure-recovery claims it and
/// anti-repetition never runs.
///
/// All five key on the Codex/opencode contract (`exec_command` calls,
/// the literal `Process exited with code N` result shape) and each was
/// cut against a named gym fixture. They defaulted ON until 2026-10-07,
/// when the operator made the OpenAI chat path's target llama-server
/// parity (note fb4d2489): they are the passes that make a locally-served
/// turn differ from bare llama.cpp, read-attractor appends a system
/// message at the tail that a real chat template may refuse, and
/// failure-recovery leans on the flattening the conversation path no
/// longer does. Opt-in keeps them measurable. Every firing logs at INFO,
/// so ON is auditable and OFF is total.
///
/// NOT governed: `frontdoor::promote_in_content_tool_call`, which lifts a tool
/// call the model emitted as content into the structured field. That
/// RECOVERS the model's intent rather than overriding it — off, the
/// call is silently lost, which is less faithful, not more.
pub fn reshape_enabled() -> bool {
    reshape_from(std::env::var("SOVEREIGN_FRONTDOOR_RESHAPE").ok().as_deref())
}

/// Pure half of [`reshape_enabled`], so its default is pinned by a test
/// without mutating process env under every other test in the binary.
fn reshape_from(raw: Option<&str>) -> bool {
    opt_in(raw)
}

#[cfg(test)]
mod switch_tests {
    use super::{opt_in, reshape_from};

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

    /// Same spellings as the allowlist switch: only 1/true turns the
    /// passes on, so a typo cannot start rewriting what clients sent.
    #[test]
    fn reshape_is_off_unless_explicitly_turned_on() {
        assert!(!reshape_from(None), "absent means off — llama-server parity");
        assert!(!reshape_from(Some("0")));
        assert!(!reshape_from(Some("on")), "unrecognised stays off");
        assert!(reshape_from(Some("1")));
        assert!(reshape_from(Some("true")));
    }
}
