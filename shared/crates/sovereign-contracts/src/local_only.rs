// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Is this node local-only?** — the one decider, over `[daemon] local_only`
//! and `SOVEREIGN_LOCAL_ONLY`. It moved here from `sovereign_daemon::local_only`
//! (still re-exported there, with that module's rationale) so the mesh
//! program's `svrn mesh up` asks the same question the daemon's boot does
//! (pb-rails-untether) instead of a second copy (principle 8).

/// Where a [`LocalOnlyProfile`] verdict came from. Carried with the verdict so
/// the boot trace can say *why* the daemon is in the posture it is in — a
/// closed set, so a new source cannot be added without visiting every reader
/// (ARCH §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOnlySource {
    /// [`ENV_VAR`] was set to a recognised value; it wins over config in both
    /// directions (a host can force the profile ON for a hardened deploy, or
    /// force it OFF to run a networked daemon from a local-only config).
    Env,
    /// `[daemon] local_only` in `config.toml` decided it — the env var was
    /// unset (or unparseable, which warns and defers here).
    Config,
    /// Neither said anything: the shipped default, which is NETWORKED.
    Default,
}

impl LocalOnlySource {
    /// Stable string for tracing/report surfaces.
    pub fn as_str(self) -> &'static str {
        match self {
            LocalOnlySource::Env => "env",
            LocalOnlySource::Config => "config",
            LocalOnlySource::Default => "default",
        }
    }
}

/// The env override, honoured in both directions. Declared in
/// `quality/env-flags.toml` (cluster `mesh`, status `shipped`).
pub const ENV_VAR: &str = "SOVEREIGN_LOCAL_ONLY";

/// **The** answer to "is this daemon local-only".
///
/// Resolve it once per boot ([`LocalOnlyProfile::resolve`]) and pass the value
/// down; every gate that used to read config or env for itself reads this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalOnlyProfile {
    on: bool,
    source: LocalOnlySource,
}

impl Default for LocalOnlyProfile {
    /// The shipped posture: networked. Ships default-OFF — see
    /// `sovereign/DEFAULTS_LEDGER.md`.
    fn default() -> Self {
        Self {
            on: false,
            source: LocalOnlySource::Default,
        }
    }
}

impl LocalOnlyProfile {
    /// Resolve from the process environment plus the `[daemon] local_only`
    /// config field. The only impure entry point; [`Self::decide`] is the
    /// testable core.
    pub fn resolve(cfg_local_only: bool) -> Self {
        Self::decide(std::env::var(ENV_VAR).ok().as_deref(), cfg_local_only)
    }

    /// Pure decision: env (tri-state) over config (bool) over the default.
    ///
    /// Tri-state is why this does not reuse `auto_resume::env_truthy`, which
    /// is a two-state read (`set-and-truthy` vs. everything else) and so
    /// cannot express "explicitly force the profile off". An unrecognised
    /// value is REPORTED and defers to config rather than being silently read
    /// as false (ARCH §18.3 — never silently substitute).
    pub fn decide(env: Option<&str>, cfg_local_only: bool) -> Self {
        match env.map(parse_env) {
            Some(Some(on)) => Self {
                on,
                source: LocalOnlySource::Env,
            },
            Some(None) => {
                tracing::warn!(
                    var = ENV_VAR,
                    value = env.unwrap_or(""),
                    config = cfg_local_only,
                    "local_only: unrecognised env value — ignoring it and using \
                     [daemon] local_only. Recognised: 1/true/yes/on, 0/false/no/off."
                );
                Self {
                    on: cfg_local_only,
                    source: LocalOnlySource::Config,
                }
            }
            None => {
                if cfg_local_only {
                    Self {
                        on: true,
                        source: LocalOnlySource::Config,
                    }
                } else {
                    Self::default()
                }
            }
        }
    }

    /// Is the daemon local-only — no discovery, no transport, no mesh loops?
    pub fn is_local_only(self) -> bool {
        self.on
    }

    /// What decided it.
    pub fn source(self) -> LocalOnlySource {
        self.source
    }

    /// One word for a tracing field / status surface.
    pub fn label(self) -> &'static str {
        if self.on {
            "local-only"
        } else {
            "networked"
        }
    }
}

/// `Some(true)`/`Some(false)` for a recognised value; `None` for anything
/// else, which the caller REPORTS rather than defaulting.
fn parse_env(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_networked_and_says_so() {
        let p = LocalOnlyProfile::default();
        assert!(!p.is_local_only());
        assert_eq!(p.source(), LocalOnlySource::Default);
        assert_eq!(p.label(), "networked");
    }

    #[test]
    fn config_decides_when_env_is_unset() {
        let on = LocalOnlyProfile::decide(None, true);
        assert!(on.is_local_only());
        assert_eq!(on.source(), LocalOnlySource::Config);
        assert_eq!(on.label(), "local-only");

        let off = LocalOnlyProfile::decide(None, false);
        assert!(!off.is_local_only());
        assert_eq!(off.source(), LocalOnlySource::Default);
    }

    #[test]
    fn env_wins_over_config_in_both_directions() {
        // Force ON over a networked config.
        for raw in ["1", "true", "YES", " on "] {
            let p = LocalOnlyProfile::decide(Some(raw), false);
            assert!(p.is_local_only(), "{raw} should force the profile on");
            assert_eq!(p.source(), LocalOnlySource::Env);
        }
        // Force OFF over a local-only config — the direction `SOVEREIGN_IROH`
        // and `SOVEREIGN_DISABLE_MDNS` cannot express, and the one an operator
        // needs to run a networked daemon from a hardened config file.
        for raw in ["0", "false", "NO", "off"] {
            let p = LocalOnlyProfile::decide(Some(raw), true);
            assert!(!p.is_local_only(), "{raw} should force the profile off");
            assert_eq!(p.source(), LocalOnlySource::Env);
        }
    }

    #[test]
    fn unrecognised_env_defers_to_config_rather_than_reading_as_false() {
        // The §18.3 case: a typo must not silently disable a hardened
        // deployment's profile.
        let p = LocalOnlyProfile::decide(Some("maybe"), true);
        assert!(
            p.is_local_only(),
            "an unparseable override must not substitute a false"
        );
        assert_eq!(p.source(), LocalOnlySource::Config);
    }
}
