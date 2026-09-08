// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Is this daemon local-only?** — asked once, answered in one place.
//!
//! # Why this exists
//!
//! `cw-lift`'s `cw-local-only-daemon` bar claimed "a sovereign daemon can
//! compile and boot with zero commonwealth-\* and no iroh". Rungs 3a/3b took
//! the DIRECT deps to zero; the fusion census (2026-09-08) then measured what
//! remained and the bar's K2 fired:
//!
//! - 78 distinct `sovereign_mesh::` items reached from `sovereign-cli-daemon`,
//!   classified 25 mesh-only / 32 local-in-substance / 21 BOTH;
//! - **31 unmovable however the seam is drawn**, and 21 of those are the
//!   daemon's own lifecycle (`EmbeddedDaemon`, `daemon_services::assemble`,
//!   `rpc_warm_http`, `persist::resolve_self_node_id`) rather than mesh
//!   features;
//! - the best available crate seam moves **15,634 lines for zero deleted
//!   dependencies** — the shim K2 warns against, not a boundary.
//!
//! So the honest deliverable is a RUNTIME posture, and this type is it: the
//! daemon's network surface is a decision, made here, rather than a property
//! of which crates happen to be linked.
//!
//! # One decider (ARCH §10.6)
//!
//! Half a profile already existed and the halves did not know about each
//! other: `daemon::mdns_enabled_effective` decided mDNS from
//! `[discovery] mdns` + `SOVEREIGN_DISABLE_MDNS`, and
//! `iroh_access::resolve_enabled` decided iroh from `[iroh] enabled` + the
//! client-exposed marker + `SOVEREIGN_IROH`. Nothing answered "is this daemon
//! local-only", so nothing could gate the three unconditional loops
//! (gossip, auto-ingest collaborate, ring-sync) or the rail KV pump beside
//! them. Both existing gates now READ this profile instead of standing
//! parallel to it, so there is one answer and it is `LocalOnlyProfile`.
//!
//! # Not the skill privacy noun
//!
//! `ShardingPrivacy::LocalOnly` (sovereign-contracts/src/skills.rs) says a
//! SKILL's conversations never leave the machine. That is data-sharing
//! policy. This is the daemon's NETWORK posture: which background loops and
//! sockets exist at all. Different question, deliberately different type.
//!
//! # What the profile skips, and what it does not
//!
//! It skips the NETWORK, never the model. A local-only daemon still mints and
//! persists its `Mesh` with one member — the solo case is the honest N=1, and
//! an `Option<Mesh>` would fork every reader of membership into two shapes.
//! What it removes is the traffic: no multicast advertise/browse, no iroh
//! endpoint or relay contact, no gossip round, no peer-assisted ingest
//! handoff, no ring anti-entropy, no rail KV pump.

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

/// One background service a running daemon can have spawned.
///
/// A closed set (ARCH §2) so [`RunningServices`] cannot grow a member that no
/// reader visits, and so a new loop added to `start_daemon` without a census
/// entry is a compile-visible omission rather than an invisible one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MeshService {
    /// mDNS `_commonwealth._tcp` advertise (the multicast socket).
    MdnsAdvertise,
    /// mDNS browse loop populating the discovered-peers table.
    MdnsBrowse,
    /// The gossip heartbeat loop.
    Gossip,
    /// The peer-assisted ingest handoff loop (`auto_ingest`).
    AutoIngestCollaborate,
    /// Ring-ledger anti-entropy (`ring_sync`).
    RingSync,
    /// The mesh-store outbox pump that signs local writes onto their rings.
    RailKvPump,
    /// The iroh endpoint + acceptor.
    IrohEndpoint,
    /// The founder reachability watchdog (only ever with the endpoint).
    IrohWatchdog,
}

impl MeshService {
    /// Stable name — what the boot trace prints and what a test asserts on.
    pub fn as_str(self) -> &'static str {
        match self {
            MeshService::MdnsAdvertise => "mdns_advertise",
            MeshService::MdnsBrowse => "mdns_browse",
            MeshService::Gossip => "gossip",
            MeshService::AutoIngestCollaborate => "auto_ingest_collaborate",
            MeshService::RingSync => "ring_sync",
            MeshService::RailKvPump => "rail_kv_pump",
            MeshService::IrohEndpoint => "iroh_endpoint",
            MeshService::IrohWatchdog => "iroh_watchdog",
        }
    }

    /// Every service the census can name. Used by the trace to print what was
    /// NOT spawned, which is the half a log of spawns cannot show.
    pub const ALL: &'static [MeshService] = &[
        MeshService::MdnsAdvertise,
        MeshService::MdnsBrowse,
        MeshService::Gossip,
        MeshService::AutoIngestCollaborate,
        MeshService::RingSync,
        MeshService::RailKvPump,
        MeshService::IrohEndpoint,
        MeshService::IrohWatchdog,
    ];
}

/// What a running daemon actually spawned, recorded at the spawn sites.
///
/// This is the profile's INSTRUMENT (ARCH §18.1): "no network traffic" is not
/// checkable from config, and grepping the log for absences proves nothing. A
/// list of the loops that were started is falsifiable — delete a gate and the
/// boot assertion names the service that reappeared.
///
/// Distinct from [`crate::daemon_services::DaemonServices`], which is what a
/// HOST supplies to the daemon (capabilities, routers, stores). This is what
/// the daemon SPAWNED as a consequence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunningServices {
    spawned: std::collections::BTreeSet<MeshService>,
}

impl RunningServices {
    /// Record that `service` was spawned.
    pub fn record(&mut self, service: MeshService) {
        self.spawned.insert(service);
    }

    /// Was it spawned?
    pub fn contains(&self, service: MeshService) -> bool {
        self.spawned.contains(&service)
    }

    /// Everything spawned, in a stable order.
    pub fn spawned(&self) -> Vec<MeshService> {
        self.spawned.iter().copied().collect()
    }

    /// Names of everything spawned — the trace/assert-friendly form.
    pub fn names(&self) -> Vec<&'static str> {
        self.spawned.iter().map(|s| s.as_str()).collect()
    }

    /// Names of everything [`MeshService::ALL`] knows about that was NOT
    /// spawned. The local-only claim is about this list, so the boot trace
    /// prints it rather than leaving the operator to infer absence.
    pub fn skipped_names(&self) -> Vec<&'static str> {
        MeshService::ALL
            .iter()
            .filter(|s| !self.spawned.contains(s))
            .map(|s| s.as_str())
            .collect()
    }

    /// Did this boot start anything that touches the network?
    ///
    /// Every member of the closed set does, which is what makes it the right
    /// set: the census carries background loops that reach a peer, a relay or
    /// a multicast group, and nothing else. `auto_resume`'s local ingest
    /// re-spawn is deliberately not a member — it is work the operator asked
    /// for on this machine.
    pub fn any_network_service(&self) -> bool {
        !self.spawned.is_empty()
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

    #[test]
    fn a_census_reports_both_halves() {
        let mut svc = RunningServices::default();
        assert!(!svc.any_network_service());
        assert_eq!(svc.skipped_names().len(), MeshService::ALL.len());

        svc.record(MeshService::Gossip);
        svc.record(MeshService::RingSync);
        assert!(svc.any_network_service());
        assert!(svc.contains(MeshService::Gossip));
        assert!(!svc.contains(MeshService::IrohEndpoint));
        assert_eq!(svc.names(), vec!["gossip", "ring_sync"]);
        assert!(svc.skipped_names().contains(&"rail_kv_pump"));
    }
}
