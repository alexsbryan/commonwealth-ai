//! VRAM capacity preflight — extracted from `run_daemon` (§3.3).
//!
//! Estimate the working set (weights + KV cache + grammar scratch) for
//! every slot the daemon would eagerly load and sum against detected
//! VRAM with a safety margin.
//!
//! **Posture is advisory by default.** If the config overcommits, the
//! preflight prints a warning and starts anyway. This keeps CPU-only and
//! low- or unrecognized-VRAM machines runnable: VRAM detection returns 0
//! there, so a hard gate would refuse every config even though the models
//! load fine into host RAM (just slower). A friend on a laptop without a
//! discrete GPU should be able to `svrn daemon run`, not hit a wall.
//!
//! Two env knobs adjust the posture:
//!   - `SOVEREIGN_STRICT_VRAM_CHECK=1` restores the hard refusal, for
//!     operators who want the anti-thrash guardrail — e.g. the 2026-05-11
//!     L40S class where 2 × Q4_K_L "fit" at 38 GB idle but evicted each
//!     other under live KV pressure, taking throughput to 1 slug/hr.
//!   - `SOVEREIGN_SKIP_VRAM_CHECK=1` silences the warning entirely.
//!
//! An UNREADABLE model file (missing/renamed GGUF) is always a hard
//! refusal in the default and strict postures — that's a file error, not
//! a capacity margin, and booting past it only yields a confusing
//! llama.cpp load failure. `SKIP` still bypasses it, preserving the prior
//! escape hatch. The report prints in every proceed case so the
//! diagnosis stays visible in logs.

use sovereign_core::setup_config::SetupConfig;

/// What the preflight decided to do about a capacity report. Kept as a
/// pure value (no I/O) so the policy is unit-testable without real
/// hardware; `check_vram` maps it to messages and a proceed/refuse bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VramAction {
    /// Config fits — proceed, info-log only.
    ProceedFits,
    /// Overcommit, silenced by `SOVEREIGN_SKIP_VRAM_CHECK` — proceed quietly.
    ProceedSilenced,
    /// Overcommit, default posture — warn and proceed.
    ProceedWithWarning,
    /// A slot's model file is unreadable — refuse (file error).
    RefuseUnreadable,
    /// Overcommit under `SOVEREIGN_STRICT_VRAM_CHECK` — refuse.
    RefuseStrict,
}

impl VramAction {
    /// The daemon proceeds unless this is a refusal.
    fn proceeds(self) -> bool {
        !matches!(
            self,
            VramAction::RefuseUnreadable | VramAction::RefuseStrict
        )
    }
}

/// Pure policy: given the fit verdict and env postures, decide the action.
///
/// Precedence is deliberate and preserves the historical escape hatch:
/// a clean fit wins; then `skip` (bypass everything, including an
/// unreadable file, as the old code did); then an unreadable file is a
/// hard error; then `strict` refuses; otherwise the new default is to
/// warn and proceed.
fn decide(fits: bool, unreadable: bool, skip: bool, strict: bool) -> VramAction {
    if fits {
        VramAction::ProceedFits
    } else if skip {
        VramAction::ProceedSilenced
    } else if unreadable {
        VramAction::RefuseUnreadable
    } else if strict {
        VramAction::RefuseStrict
    } else {
        VramAction::ProceedWithWarning
    }
}

/// True iff the operator opted into hard VRAM enforcement.
fn strict_vram_check() -> bool {
    std::env::var("SOVEREIGN_STRICT_VRAM_CHECK")
        .map(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

/// Returns `true` to proceed with daemon startup, `false` to refuse
/// (caller returns exit code 1). See the module docs for the posture.
pub(crate) fn check_vram(config: &SetupConfig) -> bool {
    check_vram_reporting(config, &SetupConfig::default_path())
}

/// `check_vram`, told WHICH config file to name in its refusal hints.
///
/// Split out because both hints hard-coded `SetupConfig::default_path()`,
/// so a daemon started with `--config <path>` told the operator to fix a
/// file it had not read (observed 2026-08-30 during the remote-engine
/// bring-up).
pub(crate) fn check_vram_reporting(config: &SetupConfig, config_path: &std::path::Path) -> bool {
    // The preflight asks "do the GGUF slots fit in this machine's VRAM?" —
    // a question only the llama engine can be asked. An engine that holds
    // no local weights has no slots to size, and `[models]` is then just
    // schema it never reads, so sizing it refuses on paths that were never
    // going to be opened.
    //
    // Measured 2026-08-30 on the first live `[engine] kind = "remote"`
    // boot: the daemon died here with "model file unreadable" before the
    // engine was ever constructed. The factory was correct and the node
    // still could not start — which is why this is checked against the
    // running system and not only in a unit test (ARCH §18.1).
    if config.engine.kind != sovereign_core::setup_config::EngineKind::Llama {
        tracing::info!(
            target: "engine_factory",
            engine = %config.engine.kind,
            "VRAM preflight: skipped — this engine holds no local weights, so \
             `[models]` names nothing that will be loaded"
        );
        return true;
    }
    let hardware = sovereign_inference::hardware::HardwareProfile::detect();
    let slots = sovereign_inference::capacity::build_slots_from_config(config);
    let report = sovereign_inference::capacity::check_fit(&slots, &hardware);

    let action = decide(
        report.fits,
        report.has_unreadable_slot(),
        sovereign_inference::capacity::check_skipped_by_env(),
        strict_vram_check(),
    );

    match action {
        VramAction::ProceedFits => {
            tracing::info!(
                required_mb = report.total_required_mb,
                available_mb = report.available_mb,
                slots = report.per_slot.len(),
                "VRAM preflight: config fits"
            );
        }
        VramAction::ProceedSilenced => {
            tracing::warn!(
                required_mb = report.total_required_mb,
                available_mb = report.available_mb,
                "VRAM overcommit — silenced by SOVEREIGN_SKIP_VRAM_CHECK. Thrash risk accepted by operator."
            );
        }
        VramAction::ProceedWithWarning => {
            eprintln!("{}", report.warn_message());
            eprintln!(
                "hint: starting anyway. To enforce a hard stop instead, set \
                 SOVEREIGN_STRICT_VRAM_CHECK=1; to silence this warning, set \
                 SOVEREIGN_SKIP_VRAM_CHECK=1."
            );
            tracing::warn!(
                required_mb = report.total_required_mb,
                available_mb = report.available_mb,
                "VRAM overcommit — starting anyway (advisory). Set SOVEREIGN_STRICT_VRAM_CHECK=1 to enforce."
            );
        }
        VramAction::RefuseUnreadable => {
            eprintln!("{}", report.refuse_message());
            eprintln!(
                "hint: a model file above is unreadable (missing or moved). Fix its \
                 path in {} and re-run. (SOVEREIGN_SKIP_VRAM_CHECK=1 bypasses at your \
                 own risk.)",
                config_path.display(),
            );
        }
        VramAction::RefuseStrict => {
            eprintln!("{}", report.refuse_message());
            eprintln!(
                "hint: strict VRAM check is on (SOVEREIGN_STRICT_VRAM_CHECK). Unset it \
                 to start anyway with a warning, edit {}, or set \
                 SOVEREIGN_SKIP_VRAM_CHECK=1 to bypass.",
                config_path.display(),
            );
        }
    }

    action.proceeds()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitting_config_proceeds() {
        assert_eq!(decide(true, false, false, false), VramAction::ProceedFits);
        // A fit proceeds regardless of any env posture.
        assert_eq!(decide(true, true, true, true), VramAction::ProceedFits);
    }

    #[test]
    fn overcommit_default_warns_and_proceeds() {
        // The core fix: an overcommit no longer hard-blocks by default.
        let a = decide(false, false, false, false);
        assert_eq!(a, VramAction::ProceedWithWarning);
        assert!(a.proceeds(), "default overcommit must not refuse startup");
    }

    #[test]
    fn cpu_only_zero_vram_still_starts() {
        // CPU-only detects 0 VRAM → nothing fits → this is exactly the
        // friend's case. It must proceed (with a warning), not refuse.
        let a = decide(false, false, false, false);
        assert!(a.proceeds());
    }

    #[test]
    fn strict_env_restores_hard_refusal() {
        let a = decide(false, false, false, true);
        assert_eq!(a, VramAction::RefuseStrict);
        assert!(!a.proceeds(), "strict mode must refuse an overcommit");
    }

    #[test]
    fn skip_silences_and_proceeds_over_strict() {
        // SKIP is the bypass: it wins even when STRICT is also set.
        let a = decide(false, false, true, true);
        assert_eq!(a, VramAction::ProceedSilenced);
        assert!(a.proceeds());
    }

    #[test]
    fn unreadable_file_refuses_in_default_and_strict() {
        // A missing/renamed GGUF is a hard error, not fussy capacity.
        assert_eq!(
            decide(false, true, false, false),
            VramAction::RefuseUnreadable
        );
        assert_eq!(
            decide(false, true, false, true),
            VramAction::RefuseUnreadable
        );
        assert!(!decide(false, true, false, false).proceeds());
    }

    #[test]
    fn unreadable_file_still_bypassable_by_skip() {
        // Preserve the historical escape hatch: SKIP bypasses everything.
        let a = decide(false, true, true, false);
        assert_eq!(a, VramAction::ProceedSilenced);
        assert!(a.proceeds());
    }

    /// The bug this guards, found on the first live `[engine] kind =
    /// "remote"` boot (2026-08-30): the daemon refused to start because
    /// the VRAM preflight stat'd `[models]` GGUF paths that a remote
    /// engine never opens. The engine factory was already correct; the
    /// node still could not come up.
    ///
    /// Drives the REAL `check_vram` against a config whose model paths do
    /// not exist — under `llama` that is a refusal, and it must not be one
    /// for an engine that holds no weights.
    #[test]
    fn a_weightless_engine_skips_the_vram_preflight_entirely() {
        use sovereign_core::setup_config::EngineKind;

        let mut config = SetupConfig::unconfigured();
        // `unconfigured()` carries NO `[models]` since the terminal class made
        // the section an `Option` — a node may legitimately hold none. The
        // point of this test is a config that DOES name weights which happen
        // not to exist, so the table has to be present for the paths to mean
        // anything.
        config.models = Some(sovereign_core::setup_config::ModelsSection {
            primary: "/nonexistent/primary.gguf".into(),
            embed: "/nonexistent/embed.gguf".into(),
            ..Default::default()
        });

        for kind in [EngineKind::Remote, EngineKind::Custom("hypertuned".into())] {
            config.engine.kind = kind.clone();
            assert!(
                check_vram(&config),
                "`{kind}` holds no local weights, so `[models]` names nothing that will \
                 be loaded — sizing those paths refuses a node that would have run fine"
            );
        }
    }

    /// The other half: the skip is scoped to non-llama engines and must
    /// not become a blanket bypass. Under `llama` an unreadable model file
    /// is still a refusal — that guard is why a mistyped path fails at
    /// boot instead of at the first request.
    #[test]
    fn the_llama_engine_still_refuses_an_unreadable_model() {
        assert_eq!(
            decide(false, true, false, false),
            VramAction::RefuseUnreadable,
            "the weightless-engine skip must not weaken llama's own preflight"
        );
        assert!(!VramAction::RefuseUnreadable.proceeds());
    }
}
