// SPDX-License-Identifier: AGPL-3.0-or-later
//! What this node publishes to the house RIGHT NOW, and who is allowed to
//! say so.
//!
//! Two tiers, and the default is the one with a TTL:
//!
//! - [`Tier::Config`] — `[iroh.apps]`, loaded once at start. A durable
//!   assertion, correct for a service that is always up and that somebody
//!   owns keeping true.
//! - [`Tier::Claimed`] — `svrn run --as chores -- python app.py`. Registered
//!   when the process binds, renewed while it lives, dropped when it exits,
//!   and dropped by the TTL when it exits badly.
//!
//! **Why the claimed tier is the default, rather than a second config table.**
//! A config file is a *claim about* what is running; the process *is* what is
//! running, and only one of them can be wrong. Config also only accumulates:
//! nobody deletes their March 1am-hack entry, and by June the house fan-out
//! returns eight `connection refused` rows from apps that stopped existing
//! months ago — reading, the whole time, as an authoritative list. That is
//! the rot the closure-loop rule names, and the fix is that the registration
//! cannot outlive the process that owns it.
//!
//! The vocabulary is deliberately the work atlas's — claim, TTL, renew,
//! release, no history — because it is the same lifecycle and this
//! workspace already has one name for it (ARCH principle 8). A released
//! claim leaves no trace: peers see live state, not a log of every port
//! anyone ever opened.
//!
//! **This type does not decide who may reach an app.** That is
//! [`crate::identity::admit_app`], once per connection on the verified key.
//! This answers only "what is published", and the per-request name lookup can
//! choose among these and nothing else.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The one app-name rule, re-exported from the wire boundary that enforces
/// it so a publisher and a request path cannot disagree about what a name is.
pub use commonwealth_transport::iroh_identity_forward::valid_app_name;

/// TTL a claim gets when the caller names none. An hour: long enough that a
/// laptop asleep for a coffee break does not lose its publish, short enough
/// that a `kill -9`'d runner is gone before anyone notices it in a fan-out.
pub const DEFAULT_CLAIM_TTL: Duration = Duration::from_secs(3600);
/// The longest TTL a claim may ask for, matching the work atlas's own cap.
/// Beyond this, the honest shape is `[iroh.apps]` — a durable assertion with
/// an owner, not a claim renewed by nobody.
pub const MAX_CLAIM_TTL: Duration = Duration::from_secs(24 * 3600);

/// Which tier published an app, and therefore what makes it go away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// `[iroh.apps]`. Goes away when a human edits the config and the daemon
    /// restarts.
    Config,
    /// A live claim. Goes away on release, or on its TTL.
    Claimed,
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tier::Config => write!(f, "[iroh.apps]"),
            Tier::Claimed => write!(f, "a live claim"),
        }
    }
}

/// One published app, as the `GET` surface and the CLI print it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedApp {
    pub name: String,
    pub addr: SocketAddr,
    pub tier: Tier,
    /// The claim to renew or release. `None` for the config tier, which has
    /// nothing to renew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<String>,
    /// Seconds left, RELATIVE. Not an absolute timestamp: the renewer and
    /// the daemon do not have to agree about the wall clock for a relative
    /// number to be actionable, and every consumer of this field is about to
    /// do arithmetic against `now` anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_secs: Option<u64>,
}

/// What a successful claim hands back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppClaim {
    pub claim_id: String,
    pub name: String,
    pub addr: SocketAddr,
    pub expires_in_secs: u64,
}

/// Why a publish, renew or release was refused. Every one of these is
/// reported to the caller as itself; none of them degrades into a success
/// that published something other than what was asked for (ARCH principle 6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublishRefusal {
    #[error(
        "`{0}` is not a usable app name — an app name is ASCII letters, digits, `_` and `-`, \
         because that is exactly what a request path can name it by"
    )]
    BadName(String),
    #[error(
        "`{name}` is already published by {tier} — publishing it twice would make which origin \
         answers depend on lookup order, so this one is refused rather than shadowing that one"
    )]
    NameTaken { name: String, tier: Tier },
    #[error("no live claim `{0}` — it was released, or its TTL dropped it")]
    NoSuchClaim(String),
}

/// Called with whether ANY app is published, each time that answer changes.
///
/// The acceptor installs one to add or remove `APP_ALPN` from the endpoint:
/// a node that negotiates a protocol it publishes nothing for turns a clean
/// refusal into a connection that opens and then closes.
///
/// Called with the registry lock RELEASED. A hook that calls back into the
/// registry it is installed on will not deadlock, but it will observe a state
/// one step later than the one that fired it — do not.
pub type ServingHook = Arc<dyn Fn(bool) + Send + Sync>;

/// The live registry. Cloning shares one registry, not a copy of it.
#[derive(Clone, Default)]
pub struct PublishedApps {
    inner: Arc<Inner>,
}

/// Both locks are taken with `unwrap_or_else(PoisonError::into_inner)`, which
/// is a DELIBERATE deviation from "never swallow an Err" (ARCH principle 6)
/// and is named here rather than left as an idiom nobody re-reads.
///
/// A poisoned lock means a thread panicked while holding it. The only code
/// that runs under this one is a handful of `BTreeMap` operations, which
/// leave the map coherent whether or not they completed — there is no
/// half-applied claim to observe. Propagating the poison instead would mean
/// one panic, anywhere, permanently closes `cwth/app/0` on this node: every
/// later dial resolves against a lock that will never open again, and the
/// operator sees apps that stopped being reachable for no reason a log
/// explains. Proceeding on coherent state is the smaller wrong.
#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    hook: Mutex<Option<ServingHook>>,
}

#[derive(Default)]
struct State {
    config: BTreeMap<String, SocketAddr>,
    claimed: BTreeMap<String, ClaimRow>,
}

struct ClaimRow {
    id: String,
    addr: SocketAddr,
    deadline: Instant,
}

impl State {
    /// Drop every expired claim. Called on every read as well as every write,
    /// so expiry is observed by the next question anyone asks rather than by
    /// a timer that has to be running — a registry whose correctness depends
    /// on a background task is a registry that is wrong whenever that task
    /// dies (ARCH principle 10).
    fn sweep(&mut self, now: Instant) {
        self.claimed.retain(|name, row| {
            let live = row.deadline > now;
            if !live {
                tracing::info!(
                    target: "transport",
                    app = %name,
                    addr = %row.addr,
                    claim = %row.id,
                    "app registry: a claim's TTL expired — this app is no longer published. \
                     The runner exited without releasing, or stopped renewing"
                );
            }
            live
        });
    }

    fn serving(&self) -> bool {
        !self.config.is_empty() || !self.claimed.is_empty()
    }

    fn tier_holding(&self, name: &str) -> Option<Tier> {
        if self.config.contains_key(name) {
            Some(Tier::Config)
        } else if self.claimed.contains_key(name) {
            Some(Tier::Claimed)
        } else {
            None
        }
    }

    fn find_claim(&self, claim_id: &str) -> Option<&str> {
        self.claimed
            .iter()
            .find(|(_, row)| row.id == claim_id)
            .map(|(name, _)| name.as_str())
    }
}

impl std::fmt::Debug for PublishedApps {
    /// Counts, taken under the lock and NOTHING else.
    ///
    /// Not `listing()`: every read on this type sweeps expired claims and can
    /// fire the serving hook, and a hook that reconfigures a live endpoint is
    /// not something a `{:?}` in a tracing line should be able to cause. A
    /// `Debug` with side effects is a debugging session that changes what it
    /// is looking at.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("PublishedApps")
            .field("config", &state.config.len())
            .field("claimed", &state.claimed.len())
            .finish()
    }
}

impl PublishedApps {
    /// A registry holding the config tier, with no claims yet.
    pub fn with_config(config: BTreeMap<String, SocketAddr>) -> Self {
        let apps = Self::default();
        apps.with_state(|s| s.config = config);
        apps
    }

    /// Replace the config tier wholesale — what `[iroh.apps]` says now.
    ///
    /// Claims are untouched: a running app's publish is a fact about a
    /// process, and a config reload is not news about that process. A config
    /// entry arriving for a name a claim already holds is dropped with a
    /// warning naming both, because the alternative is a durable line in a
    /// file silently taking a port away from something that is running.
    pub fn set_config(&self, config: BTreeMap<String, SocketAddr>) {
        self.with_state(|s| {
            let (taken, free): (Vec<_>, Vec<_>) = config
                .into_iter()
                .partition(|(name, _)| s.claimed.contains_key(name));
            for (name, addr) in taken {
                tracing::warn!(
                    target: "transport",
                    app = %name,
                    config_addr = %addr,
                    "app registry: `[iroh.apps]` names an app a RUNNING process already \
                     publishes — the running one keeps the name and the config line is \
                     ignored until it releases"
                );
            }
            s.config = free.into_iter().collect();
        });
    }

    /// Install the serving hook, and fire it once with the current answer so
    /// the caller does not have to duplicate the "is anything published"
    /// question it is about to be told about.
    pub fn on_serving_change(&self, hook: ServingHook) {
        let serving = self.is_serving();
        *self
            .inner
            .hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook.clone());
        hook(serving);
    }

    /// Everything published right now, by name — the map the per-request
    /// lookup chooses among.
    pub fn snapshot(&self) -> BTreeMap<String, SocketAddr> {
        self.with_state(|s| {
            let mut out = s.config.clone();
            for (name, row) in &s.claimed {
                out.insert(name.clone(), row.addr);
            }
            out
        })
    }

    /// Everything published right now, with the tier and the time left — the
    /// `svrn publish` view.
    pub fn listing(&self) -> Vec<PublishedApp> {
        let now = Instant::now();
        self.with_state(|s| {
            let config = s.config.iter().map(|(name, addr)| PublishedApp {
                name: name.clone(),
                addr: *addr,
                tier: Tier::Config,
                claim_id: None,
                expires_in_secs: None,
            });
            let claimed = s.claimed.iter().map(|(name, row)| PublishedApp {
                name: name.clone(),
                addr: row.addr,
                tier: Tier::Claimed,
                claim_id: Some(row.id.clone()),
                expires_in_secs: Some(row.deadline.saturating_duration_since(now).as_secs()),
            });
            config.chain(claimed).collect()
        })
    }

    /// Whether this node publishes anything at all. The fact behind the
    /// `APP_ALPN` advertisement and behind `origins: [app]` in gossip — read
    /// live, because a gossip stamp that says `app` while nothing is
    /// published is a peer's wasted dial.
    pub fn is_serving(&self) -> bool {
        self.with_state(|s| s.serving())
    }

    /// Publish `name` at `addr` for `ttl`, taking a claim on the name.
    ///
    /// Refused when the name is unusable, or when anything already publishes
    /// it. Refusing a duplicate rather than shadowing is the point: with two
    /// entries for one name, which origin answers becomes a fact about map
    /// ordering, and the loser is a process that believes it is published and
    /// is not.
    pub fn claim(
        &self,
        name: &str,
        addr: SocketAddr,
        ttl: Duration,
    ) -> Result<AppClaim, PublishRefusal> {
        if !valid_app_name(name.as_bytes()) {
            return Err(PublishRefusal::BadName(name.to_string()));
        }
        let ttl = ttl.min(MAX_CLAIM_TTL);
        let id = mint_claim_id(name);
        let claim = self.mutating(|s| {
            if let Some(tier) = s.tier_holding(name) {
                return Err(PublishRefusal::NameTaken {
                    name: name.to_string(),
                    tier,
                });
            }
            s.claimed.insert(
                name.to_string(),
                ClaimRow {
                    id: id.clone(),
                    addr,
                    deadline: Instant::now() + ttl,
                },
            );
            Ok(AppClaim {
                claim_id: id.clone(),
                name: name.to_string(),
                addr,
                expires_in_secs: ttl.as_secs(),
            })
        })?;
        tracing::info!(
            target: "transport",
            app = %claim.name,
            addr = %claim.addr,
            claim = %claim.claim_id,
            ttl_secs = claim.expires_in_secs,
            "app registry: published — members may reach it as the first path segment, \
             with no port forwarded and no config edited"
        );
        Ok(claim)
    }

    /// Push a claim's deadline out by `ttl` from now. The heartbeat half of
    /// the same lifecycle: a runner renews while its child lives.
    pub fn renew(&self, claim_id: &str, ttl: Duration) -> Result<AppClaim, PublishRefusal> {
        let ttl = ttl.min(MAX_CLAIM_TTL);
        self.mutating(|s| {
            let Some(name) = s.find_claim(claim_id).map(str::to_string) else {
                return Err(PublishRefusal::NoSuchClaim(claim_id.to_string()));
            };
            let row = s.claimed.get_mut(&name).expect("just found");
            row.deadline = Instant::now() + ttl;
            Ok(AppClaim {
                claim_id: claim_id.to_string(),
                name,
                addr: row.addr,
                expires_in_secs: ttl.as_secs(),
            })
        })
    }

    /// Unpublish a claimed app. Returns the name it held.
    ///
    /// The TTL is the backstop, not the mechanism: explicit release is what
    /// makes a housemate's fan-out stop including your app the second you hit
    /// ctrl-C, rather than up to an hour later.
    pub fn release(&self, claim_id: &str) -> Result<String, PublishRefusal> {
        let name = self.mutating(|s| {
            let Some(name) = s.find_claim(claim_id).map(str::to_string) else {
                return Err(PublishRefusal::NoSuchClaim(claim_id.to_string()));
            };
            s.claimed.remove(&name);
            Ok(name)
        })?;
        tracing::info!(
            target: "transport",
            app = %name,
            claim = %claim_id,
            "app registry: released — no longer published"
        );
        Ok(name)
    }

    /// Read under the lock, sweeping expired claims first. The sweep can
    /// change whether anything is served, so this fires the hook too.
    fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        self.mutating(|s| Ok::<T, std::convert::Infallible>(f(s)))
            .expect("infallible")
    }

    /// Take the lock, sweep, run `f`, and fire the serving hook OUTSIDE the
    /// lock if the answer changed. Every read and every write goes through
    /// here so there is one place that can forget neither.
    fn mutating<T, E>(&self, f: impl FnOnce(&mut State) -> Result<T, E>) -> Result<T, E> {
        let (out, changed_to) = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let before = state.serving();
            state.sweep(Instant::now());
            let out = f(&mut state);
            let after = state.serving();
            (out, (before != after).then_some(after))
        };
        if let Some(serving) = changed_to {
            let hook = self
                .inner
                .hook
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(hook) = hook {
                hook(serving);
            }
        }
        out
    }
}

/// A claim id: the name it holds, plus enough entropy that two runners of the
/// same app on one box cannot collide.
///
/// Identity from essence and a random seed, never a counter (ARCH principle
/// 8). It is not a secret and is not treated as one — every surface that
/// takes it is loopback-only, and any process that can present a claim id
/// could have taken the claim itself.
fn mint_claim_id(name: &str) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write(name.as_bytes());
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    format!("{name}-{:012x}", h.finish() & 0xffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        ([127, 0, 0, 1], port).into()
    }

    fn ttl() -> Duration {
        Duration::from_secs(60)
    }

    #[test]
    fn a_claim_publishes_and_a_release_unpublishes() {
        let apps = PublishedApps::default();
        assert!(!apps.is_serving());
        let claim = apps.claim("chores", addr(5000), ttl()).unwrap();
        assert_eq!(apps.snapshot().get("chores"), Some(&addr(5000)));
        assert!(apps.is_serving());
        assert_eq!(apps.release(&claim.claim_id).unwrap(), "chores");
        assert!(apps.snapshot().is_empty());
        assert!(!apps.is_serving());
    }

    /// The failing input the whole tier exists for: a runner that dies badly
    /// releases nothing, and the registration must not outlive it.
    #[test]
    fn a_claim_nobody_released_is_gone_when_its_ttl_passes() {
        let apps = PublishedApps::default();
        apps.claim("chores", addr(5000), Duration::from_millis(30))
            .unwrap();
        assert!(apps.snapshot().contains_key("chores"));
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            apps.snapshot().is_empty(),
            "an expired claim must not still be published"
        );
        assert!(!apps.is_serving());
    }

    #[test]
    fn a_renewed_claim_outlives_its_original_ttl() {
        let apps = PublishedApps::default();
        let claim = apps
            .claim("chores", addr(5000), Duration::from_millis(40))
            .unwrap();
        std::thread::sleep(Duration::from_millis(25));
        apps.renew(&claim.claim_id, Duration::from_secs(60))
            .unwrap();
        std::thread::sleep(Duration::from_millis(40));
        assert!(
            apps.snapshot().contains_key("chores"),
            "renewing must push the deadline out, not leave the original"
        );
    }

    /// Shadowing is the alternative, and it makes which origin answers a fact
    /// about map iteration order — with the loser believing it is published.
    #[test]
    fn a_name_the_config_tier_holds_is_refused_not_shadowed() {
        let apps = PublishedApps::with_config([("chores".to_string(), addr(8096))].into());
        assert_eq!(
            apps.claim("chores", addr(5000), ttl()),
            Err(PublishRefusal::NameTaken {
                name: "chores".into(),
                tier: Tier::Config
            })
        );
        assert_eq!(
            apps.snapshot().get("chores"),
            Some(&addr(8096)),
            "the refused claim must not have moved the config entry"
        );
    }

    #[test]
    fn a_name_another_claim_holds_is_refused() {
        let apps = PublishedApps::default();
        apps.claim("chores", addr(5000), ttl()).unwrap();
        assert!(matches!(
            apps.claim("chores", addr(5001), ttl()),
            Err(PublishRefusal::NameTaken {
                tier: Tier::Claimed,
                ..
            })
        ));
    }

    /// The name rule is the path rule. A name the registry accepts that
    /// `split_app_name` would refuse is publishable and unreachable at once.
    #[test]
    fn a_name_a_request_path_could_not_carry_is_refused() {
        let apps = PublishedApps::default();
        for bad in ["", "chores/x", "../etc", "chore s", "chores?x"] {
            assert_eq!(
                apps.claim(bad, addr(5000), ttl()),
                Err(PublishRefusal::BadName(bad.to_string())),
                "{bad:?} must not be publishable"
            );
        }
    }

    #[test]
    fn a_ttl_beyond_the_cap_is_clamped_to_it() {
        let apps = PublishedApps::default();
        let claim = apps
            .claim("chores", addr(5000), Duration::from_secs(999_999))
            .unwrap();
        assert_eq!(claim.expires_in_secs, MAX_CLAIM_TTL.as_secs());
    }

    #[test]
    fn renewing_or_releasing_an_unknown_claim_says_so() {
        let apps = PublishedApps::default();
        assert_eq!(
            apps.release("chores-000000000000"),
            Err(PublishRefusal::NoSuchClaim("chores-000000000000".into()))
        );
        assert_eq!(
            apps.renew("chores-000000000000", ttl()),
            Err(PublishRefusal::NoSuchClaim("chores-000000000000".into()))
        );
    }

    /// The hook is what adds and removes `APP_ALPN`, so it must fire on the
    /// EDGES only — an endpoint reconfigured on every dial is a cost paid for
    /// nothing.
    #[test]
    fn the_serving_hook_fires_on_the_edges_and_not_between() {
        let apps = PublishedApps::default();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        apps.on_serving_change(Arc::new(move |serving| sink.lock().unwrap().push(serving)));
        let a = apps.claim("chores", addr(5000), ttl()).unwrap();
        let b = apps.claim("printer", addr(5001), ttl()).unwrap();
        apps.release(&a.claim_id).unwrap();
        apps.release(&b.claim_id).unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![false, true, false],
            "install (nothing yet), the first publish, and the last release"
        );
    }

    /// An expiry is an edge too: the endpoint must stop advertising a
    /// protocol whose last origin walked away.
    #[test]
    fn the_serving_hook_fires_when_the_last_claim_expires() {
        let apps = PublishedApps::default();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        apps.on_serving_change(Arc::new(move |serving| sink.lock().unwrap().push(serving)));
        apps.claim("chores", addr(5000), Duration::from_millis(30))
            .unwrap();
        std::thread::sleep(Duration::from_millis(60));
        apps.is_serving();
        assert_eq!(*seen.lock().unwrap(), vec![false, true, false]);
    }

    #[test]
    fn the_listing_names_the_tier_and_the_time_left() {
        let apps = PublishedApps::with_config([("jellyfin".to_string(), addr(8096))].into());
        apps.claim("chores", addr(5000), ttl()).unwrap();
        let listing = apps.listing();
        let jellyfin = listing.iter().find(|a| a.name == "jellyfin").unwrap();
        assert_eq!(jellyfin.tier, Tier::Config);
        assert_eq!(jellyfin.expires_in_secs, None);
        let chores = listing.iter().find(|a| a.name == "chores").unwrap();
        assert_eq!(chores.tier, Tier::Claimed);
        assert!(chores.expires_in_secs.unwrap() <= 60);
        assert!(chores.claim_id.is_some());
    }

    /// Two runners of the same app on one box, started in the same
    /// millisecond, must not be handed one id — releasing would then
    /// unpublish somebody else's.
    #[test]
    fn claim_ids_are_distinct_for_the_same_name() {
        let a = mint_claim_id("chores");
        let b = mint_claim_id("chores");
        assert_ne!(a, b);
        assert!(a.starts_with("chores-"));
    }
}
