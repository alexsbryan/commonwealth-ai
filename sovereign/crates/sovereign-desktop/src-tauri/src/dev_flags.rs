// SPDX-License-Identifier: AGPL-3.0-or-later
//! Developer-only escape hatches for replaying the onboarding surfaces
//! without wiping `~/.sovereign/` or re-downloading multi-GB models.
//!
//! The desktop has two onboarding gates, and there is one flag per gate.
//! Each flag is read from the environment (so a single launcher —
//! `scripts/dev-onboarding.sh` — can set them and you never thread env
//! vars by hand) and is an **in-memory override only**: none of them is
//! persisted, so a normal launch with no env always resumes the real
//! saved state. Your config, projects, corpora and models are never
//! touched.
//!
//! | Gate | Surface | Flag | Effect (in-memory only) |
//! |------|---------|------|--------------------------|
//! | 1 | Setup wizard (`WelcomeThreshold` → `SetupFlow`) | `SOVEREIGN_DEV_FORCE_SETUP=1` | `DesktopConfig.setup_complete = false` (`main.rs`) |
//! | 2 | Recipe-author onboarding (tutorial / `RecipeAuthorWelcome`) | `SOVEREIGN_DEV_FORCE_FIRST_RUN=1` | `is_first_run()` → `true` and `recipe_author_list_projects()` → `[]`, so the Welcome shows its first-timer tutorial CTA |
//!
//! A value is "on" when it equals `1` or (case-insensitively) `true`.

use std::sync::Once;

/// `SOVEREIGN_DEV_FORCE_SETUP=1` — replay the setup wizard (Gate 1) even
/// when setup is already complete. Consumed in `main.rs`, which flips the
/// in-memory `DesktopConfig.setup_complete` to `false`.
pub fn force_setup() -> bool {
    env_truthy("SOVEREIGN_DEV_FORCE_SETUP")
}

/// `SOVEREIGN_DEV_FORCE_FIRST_RUN=1` — make the recipe-author surface
/// (Gate 2) behave as a first launch: [`is_first_run`] returns `true` and
/// `recipe_author_list_projects` returns empty, so `RecipeAuthorWelcome`
/// shows its first-timer tutorial CTA. In-memory only — your authored
/// projects stay on disk and reappear on the next plain launch.
///
/// [`is_first_run`]: crate::enrich_commands::is_first_run
pub fn force_first_run() -> bool {
    env_truthy("SOVEREIGN_DEV_FORCE_FIRST_RUN")
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Emit a single startup line naming any active dev onboarding flags, so
/// a forced run is never silent (glassbox). Idempotent: only the first
/// call logs, so it is safe to call from a hot path.
pub fn log_active() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let mut active: Vec<&str> = Vec::new();
        if force_setup() {
            active.push("SOVEREIGN_DEV_FORCE_SETUP (replaying the setup wizard)");
        }
        if force_first_run() {
            active.push(
                "SOVEREIGN_DEV_FORCE_FIRST_RUN \
                 (replaying the recipe-author onboarding; real projects hidden in-memory)",
            );
        }
        if active.is_empty() {
            return;
        }
        tracing::warn!(
            flags = %active.join("; "),
            "dev onboarding override active — in-memory only, nothing on disk is wiped"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::env_truthy;

    #[test]
    fn env_truthy_accepts_1_and_true_case_insensitive() {
        // A unique key so we never collide with a real flag or with
        // another test reading the same var in parallel.
        let key = "SOVEREIGN_DEV_FLAGS_TEST_TRUTHY";
        std::env::set_var(key, "1");
        assert!(env_truthy(key), "\"1\" should be truthy");
        std::env::set_var(key, "true");
        assert!(env_truthy(key), "\"true\" should be truthy");
        std::env::set_var(key, "TRUE");
        assert!(env_truthy(key), "\"TRUE\" should be truthy (case-insensitive)");
        std::env::set_var(key, "0");
        assert!(!env_truthy(key), "\"0\" should be falsy");
        std::env::set_var(key, "yes");
        assert!(!env_truthy(key), "\"yes\" should be falsy (only 1/true)");
        std::env::remove_var(key);
        assert!(!env_truthy(key), "unset should be falsy");
    }
}
