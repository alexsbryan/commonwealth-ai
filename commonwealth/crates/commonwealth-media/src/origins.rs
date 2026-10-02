// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every loopback origin this node's endpoint serves to the mesh, who may
//! reach each one, and what each declares — ONE registry, from which the
//! acceptor table, the advertised ALPNs and the gossiped claims are all
//! derived (FIVE_PROGRAMS §4 rule 8; phase-b pb-rails-origins).
//!
//! A program registers an origin over the endpoint's loopback API: an ALPN,
//! or for `cwth/http/0` a set of path prefixes, plus a loopback port, an
//! admission policy, and its declared claims. The endpoint forwards members'
//! dials to it and nothing else — a protocol or prefix nobody registered is
//! refused by name. There is no per-class code: a new program's origin is a
//! registration, never a new acceptor arm (ARCH principles 8, 9).
//!
//! **The app registry is this registry's app entry, not a second one.**
//! `cwth/app/0` is [`PublishedApps`]: its names demux per request and its
//! claims keep their own `/v1/mesh/publish` doors, so it is held here whole
//! and consulted for the app ALPN. Everything else is a slot in [`Claims`] —
//! the same claim, TTL, renew and release the apps use.
//!
//! **The tie.** A registration is handed a secret its origin alone receives
//! on every forward ([`ORIGIN_TIE_HEADER`]). The origin believes `x-mesh-*`
//! only when the tie matches (`tied_pubkey`), because a caller that reaches
//! its loopback port without the endpoint in front can type those headers.
//! The per-process acceptor mark cannot do this across two processes.
//!
//! **Standing entries** are the endpoint's own — its gossip and join routes,
//! a media origin its config declares — with no claim and no tie, because the
//! endpoint's process is their owner.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use commonwealth_core::capabilities::{NodeCapabilities, OriginKind};
use commonwealth_core::ids::NodePubkey;
use commonwealth_transport::iroh::{Forward, IrohTransport, ALPN, APP_ALPN};
use commonwealth_transport::iroh_routed_forward::{mint_tie, PrefixRoute, ORIGIN_TIE_HEADER};
use serde::{Deserialize, Serialize};

use crate::apps::PublishedApps;
use crate::claims::{mint_claim_id, Claims, DEFAULT_CLAIM_TTL};
use crate::identity::{admit_app, admit_spliced_origin, verified_headers, MemberIdentity};

/// The registration wire lives in `oicp-types` (pb-serve-distributes-standalone).
pub use oicp_types::origin::{Admit, Framing, OriginClaim, OriginRegistration};

/// One registered origin as `GET /v1/mesh/origins` lists it — never its tie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredOrigin {
    pub slot: String,
    pub addr: SocketAddr,
    pub admit: Admit,
    pub framing: Framing,
    /// `None` for a standing entry, which no claim holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_secs: Option<u64>,
}

/// Why a registration, renew or release was refused. Each is reported as
/// itself; none degrades into a success that registered something else
/// (ARCH principle 6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OriginRefusal {
    #[error(
        "`{0}` is not an ALPN this endpoint registers — an ALPN is 1-64 visible ASCII bytes, and \
         `cwth/app/0` is the app registry's (`/v1/mesh/publish`)"
    )]
    BadAlpn(String),
    #[error("`{0}` is not a registrable prefix — an absolute path of `[A-Za-z0-9_-]` segments")]
    BadPrefix(String),
    #[error(
        "only `cwth/http/0` takes prefixes, and it takes at least one with `any` or `members` \
         admission (its connections are open to joiners, so a fallback ALPN cannot apply)"
    )]
    PrefixShape,
    #[error(
        "`{slot}` is already registered by {by} — a second registration is refused rather than \
         deciding which origin answers by who wrote last"
    )]
    Taken { slot: String, by: String },
    #[error("no live origin claim `{0}` — it was released, or its TTL dropped it")]
    NoSuchClaim(String),
}

/// Called with the ALPNs this node serves, each time that set changes. The
/// acceptor installs one to keep its endpoint's advertised set true: a
/// negotiated protocol with nothing behind it turns a clean refusal into a
/// hang. Called with no registry lock held.
pub type AlpnHook = Arc<dyn Fn(Vec<Vec<u8>>) + Send + Sync>;

#[derive(Debug, Clone)]
struct Entry {
    alpn: String,
    prefix: Option<String>,
    addr: SocketAddr,
    admit: Admit,
    framing: Framing,
    tie: Option<String>,
    declared: Vec<(String, String)>,
    claims: Option<NodeCapabilities>,
    namespaces: Vec<String>,
}

#[derive(Default)]
struct State {
    standing: BTreeMap<String, Entry>,
    claimed: Claims<Entry>,
}

impl State {
    fn sweep(&mut self) {
        for (slot, row) in self.claimed.sweep(Instant::now()) {
            tracing::info!(
                target: "transport",
                slot = %slot,
                addr = %row.value.addr,
                claim = %row.id,
                "origin registry: a claim's TTL expired — this origin is no longer served. \
                 Its program exited without releasing, or stopped renewing"
            );
        }
    }

    fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.standing
            .values()
            .chain(self.claimed.iter().map(|(_, row)| &row.value))
    }

    /// The entries a dialer may reach: every one but [`Admit::Local`]'s. The
    /// served ALPN set and the acceptor decision both read this, so a local
    /// origin is neither advertised nor forwarded.
    fn dialable(&self) -> impl Iterator<Item = &Entry> {
        self.entries().filter(|e| e.admit != Admit::Local)
    }

    fn holder(&self, slot: &str) -> Option<String> {
        if self.standing.contains_key(slot) {
            return Some("this endpoint itself".into());
        }
        self.claimed
            .get(slot)
            .map(|row| format!("claim `{}`", row.id))
    }
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    hook: Mutex<Option<AlpnHook>>,
    /// The set the hook was last told; `None` until it has been told once.
    served: Mutex<Option<Vec<Vec<u8>>>>,
}

/// The live registry. Cloning shares one registry.
#[derive(Clone, Default)]
pub struct OriginRegistry {
    apps: PublishedApps,
    inner: Arc<Inner>,
}

impl std::fmt::Debug for OriginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginRegistry")
            .field("apps", &self.apps)
            .finish_non_exhaustive()
    }
}

/// The slot a registration takes: its ALPN, or its ALPN and one prefix.
fn slot(alpn: &str, prefix: Option<&str>) -> String {
    format!("{alpn}{}", prefix.unwrap_or(""))
}

fn valid_alpn(alpn: &str) -> bool {
    (1..=64).contains(&alpn.len())
        && alpn.bytes().all(|b| b.is_ascii_graphic())
        && alpn.as_bytes() != APP_ALPN
}

fn valid_prefix(prefix: &str) -> bool {
    prefix.len() > 1
        && prefix.starts_with('/')
        && prefix[1..]
            .split('/')
            .all(|seg| crate::apps::valid_app_name(seg.as_bytes()))
}

impl OriginRegistry {
    /// A registry whose app entry is `apps`.
    pub fn new(apps: PublishedApps) -> Self {
        let registry = Self {
            apps,
            inner: Arc::default(),
        };
        let weak = Arc::downgrade(&registry.inner);
        let apps = registry.apps.clone();
        // The app entry changes under its own doors, so its serving answer
        // re-derives the ALPN set here too.
        registry.apps.on_serving_change(Arc::new(move |_| {
            if let Some(inner) = weak.upgrade() {
                Self {
                    apps: apps.clone(),
                    inner,
                }
                .notify();
            }
        }));
        registry
    }

    /// The app entry.
    pub fn apps(&self) -> &PublishedApps {
        &self.apps
    }

    /// Add one of the endpoint's OWN origins: no claim, no TTL, no tie.
    /// `declared` are the endpoint's credentials for that origin, appended
    /// after the identity (a media origin's token).
    pub fn stand(
        &self,
        alpn: &[u8],
        prefixes: &[&str],
        addr: SocketAddr,
        admit: Admit,
        declared: Vec<(String, String)>,
    ) -> Result<(), OriginRefusal> {
        let alpn = String::from_utf8_lossy(alpn).into_owned();
        let entries = self.entries_of(&alpn, prefixes, addr, &admit, Framing::Http)?;
        self.mutate(|s| {
            Self::refuse_taken(s, &entries)?;
            for mut e in entries {
                e.declared = declared.clone();
                s.standing.insert(slot(&e.alpn, e.prefix.as_deref()), e);
            }
            Ok(())
        })
    }

    /// Register a program's origin. Refused by name when a slot is taken.
    pub fn register(&self, req: OriginRegistration) -> Result<OriginClaim, OriginRefusal> {
        let addr: SocketAddr = ([127, 0, 0, 1], req.port).into();
        let prefixes: Vec<&str> = req.prefixes.iter().map(String::as_str).collect();
        let entries = self.entries_of(&req.alpn, &prefixes, addr, &req.admit, req.framing)?;
        let ttl = req
            .ttl_secs
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_CLAIM_TTL)
            .min(crate::claims::MAX_CLAIM_TTL);
        let id = mint_claim_id(&req.alpn.replace('/', "-"));
        let tie = mint_tie();
        let slots = self.mutate(|s| {
            Self::refuse_taken(s, &entries)?;
            let mut slots = Vec::new();
            // The declaration rides the FIRST slot only, so a registration
            // of five prefixes is one declaration, not five.
            for (i, mut e) in entries.into_iter().enumerate() {
                e.tie = Some(tie.clone());
                if i == 0 {
                    e.claims = req.claims.clone();
                    e.namespaces = req.namespaces.clone();
                }
                let key = slot(&e.alpn, e.prefix.as_deref());
                s.claimed.insert(key.clone(), &id, e, ttl);
                slots.push(key);
            }
            Ok(slots)
        })?;
        tracing::info!(
            target: "transport",
            claim = %id,
            slots = ?slots,
            port = req.port,
            admit = ?req.admit,
            ttl_secs = ttl.as_secs(),
            declares = req.claims.is_some(),
            "origin registry: registered — members reach it through this endpoint"
        );
        Ok(OriginClaim {
            claim_id: id,
            tie,
            slots,
            expires_in_secs: ttl.as_secs(),
        })
    }

    /// Push a claim's deadline out by `ttl` from now; the seconds granted.
    /// `Some(claims)` replaces the claim's declaration, so what a registrant
    /// declares moves with its state; `None` keeps the one it has.
    pub fn renew(
        &self,
        claim_id: &str,
        ttl: Duration,
        claims: Option<NodeCapabilities>,
    ) -> Result<u64, OriginRefusal> {
        let ttl = ttl.min(crate::claims::MAX_CLAIM_TTL);
        let declares = claims.is_some();
        let secs = self.mutate(|s| {
            let keys = s
                .claimed
                .renew(claim_id, ttl)
                .ok_or_else(|| OriginRefusal::NoSuchClaim(claim_id.to_string()))?;
            if let Some(claims) = claims {
                // The declaration stays on ONE slot, as `register` put it:
                // the slot holding it, or the first when it declared none.
                let holder = keys
                    .iter()
                    .find(|k| s.claimed.get(k).is_some_and(|r| r.value.claims.is_some()))
                    .unwrap_or(&keys[0])
                    .clone();
                if let Some(row) = s.claimed.get_mut(&holder) {
                    row.value.claims = Some(claims);
                }
            }
            Ok(ttl.as_secs())
        })?;
        tracing::debug!(target: "transport", claim = %claim_id, ttl_secs = secs, declares,
                        "origin registry: renewed — a declaration replaces the claim's, \
                         none keeps it");
        Ok(secs)
    }

    /// Withdraw a claim; the slots it held.
    pub fn release(&self, claim_id: &str) -> Result<Vec<String>, OriginRefusal> {
        let slots = self.mutate(|s| {
            s.claimed
                .release(claim_id)
                .map(|rows| rows.into_iter().map(|(slot, _)| slot).collect::<Vec<_>>())
                .ok_or_else(|| OriginRefusal::NoSuchClaim(claim_id.to_string()))
        })?;
        tracing::info!(target: "transport", claim = %claim_id, slots = ?slots,
                       "origin registry: released — no longer served");
        Ok(slots)
    }

    /// Everything registered, with no tie.
    pub fn listing(&self) -> Vec<RegisteredOrigin> {
        let now = Instant::now();
        self.read(|s| {
            let standing = s.standing.iter().map(|(slot, e)| RegisteredOrigin {
                slot: slot.clone(),
                addr: e.addr,
                admit: e.admit.clone(),
                framing: e.framing,
                claim_id: None,
                expires_in_secs: None,
            });
            let claimed = s.claimed.iter().map(|(slot, row)| RegisteredOrigin {
                slot: slot.clone(),
                addr: row.value.addr,
                admit: row.value.admit.clone(),
                framing: row.value.framing,
                claim_id: Some(row.id.clone()),
                expires_in_secs: Some(row.deadline.saturating_duration_since(now).as_secs()),
            });
            standing.chain(claimed).collect()
        })
    }

    /// The ALPNs this node serves right now, sorted: every registered ALPN,
    /// and `cwth/app/0` while any app is published.
    pub fn alpns(&self) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = self.read(|s| {
            s.dialable()
                .map(|e| e.alpn.as_bytes().to_vec())
                .collect::<Vec<_>>()
        });
        if self.apps.is_serving() {
            out.push(APP_ALPN.to_vec());
        }
        out.sort();
        out.dedup();
        out
    }

    /// The origin kinds to advertise: exactly those whose ALPN — through the
    /// one kind → class → ALPN chain (`class_of`, `alpn_for_class`) — is
    /// served. No third map.
    pub fn advertised_kinds(&self) -> Vec<OriginKind> {
        let alpns = self.alpns();
        OriginKind::ALL
            .into_iter()
            .filter(|k| {
                alpns
                    .iter()
                    .any(|a| a == IrohTransport::alpn_for_class(crate::reach::class_of(*k)))
            })
            .collect()
    }

    /// Every registration's declared claims, in slot order.
    pub fn declared_claims(&self) -> Vec<NodeCapabilities> {
        self.read(|s| s.entries().filter_map(|e| e.claims.clone()).collect())
    }

    /// Every ring namespace a registered program writes on its own behalf.
    pub fn namespaces(&self) -> Vec<String> {
        let mut out: Vec<String> =
            self.read(|s| s.entries().flat_map(|e| e.namespaces.clone()).collect());
        out.sort();
        out.dedup();
        out
    }

    /// Install the ALPN hook and fire it once with the current set.
    pub fn on_alpns_change(&self, hook: AlpnHook) {
        *self
            .inner
            .hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook);
        *self
            .inner
            .served
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.notify();
    }

    /// THE acceptor decision: where a dial on `alpn` from `dialer` (named
    /// `who` by the roster, or nobody) goes, or `None` to close it. Every
    /// arm is data from the registry; nothing here names a protocol but the
    /// app entry's, which demuxes by name per request.
    pub fn forward_for(
        &self,
        alpn: &[u8],
        who: Option<&MemberIdentity>,
        dialer: NodePubkey,
    ) -> Option<Forward> {
        if alpn == APP_ALPN {
            return admit_app(who, dialer, &self.apps.snapshot_for(who), &[]);
        }
        let alpn = String::from_utf8_lossy(alpn).into_owned();
        let entries: Vec<Entry> =
            self.read(|s| s.dialable().filter(|e| e.alpn == alpn).cloned().collect());
        if entries.iter().any(|e| e.prefix.is_some()) {
            let routes = entries
                .into_iter()
                .filter_map(|e| {
                    // The one members-and-allow decider, not a re-spelling.
                    let admitted = match &e.admit {
                        Admit::Any => true,
                        Admit::Members(allow) => {
                            admit_spliced_origin(&e.alpn, who, dialer, Some(e.addr), allow, &[])
                                .is_some()
                        }
                        // `dialable` left no local entry to route.
                        Admit::MembersElse(_) | Admit::Local => false,
                    };
                    let mut headers = e.declared.clone();
                    headers.extend(e.tie.map(|t| (ORIGIN_TIE_HEADER.to_string(), t)));
                    Some((
                        e.prefix?,
                        PrefixRoute {
                            origin: e.addr,
                            headers,
                            admitted,
                        },
                    ))
                })
                .collect();
            return Some(Forward::HttpByPrefix {
                routes: Arc::new(routes),
                headers: verified_headers(who, dialer),
            });
        }
        let Some(entry) = entries.into_iter().next() else {
            tracing::warn!(
                target: "transport",
                alpn = %alpn,
                dialer = %hex::encode(dialer.0),
                "acceptor: nothing is registered for this ALPN — closing"
            );
            return None;
        };
        if let Admit::MembersElse(other) = &entry.admit {
            if who.is_none() {
                let fallback: Option<Entry> =
                    self.read(|s| s.dialable().find(|e| &e.alpn == other).cloned());
                tracing::info!(
                    target: "transport",
                    alpn = %alpn,
                    to = %other,
                    registered = fallback.is_some(),
                    dialer = %hex::encode(dialer.0),
                    "acceptor: a non-member's dial goes to the fallback origin (closed if none)"
                );
                return fallback
                    .filter(|f| !matches!(f.admit, Admit::MembersElse(_)))
                    .and_then(|f| Self::decide(f, who, dialer));
            }
        }
        Self::decide(entry, who, dialer)
    }

    /// One whole-ALPN entry's decision.
    fn decide(e: Entry, who: Option<&MemberIdentity>, dialer: NodePubkey) -> Option<Forward> {
        let forward = match &e.admit {
            Admit::Any => {
                let mut headers = verified_headers(who, dialer);
                headers.extend(e.declared.iter().cloned());
                Forward::Http {
                    origin: e.addr,
                    headers,
                }
            }
            Admit::Members(allow) => {
                admit_spliced_origin(&e.alpn, who, dialer, Some(e.addr), allow, &e.declared)?
            }
            Admit::MembersElse(_) => {
                admit_spliced_origin(&e.alpn, who, dialer, Some(e.addr), &[], &e.declared)?
            }
            // Never forwarded; `dialable` keeps it from reaching here.
            Admit::Local => return None,
        };
        match (e.framing, forward) {
            (Framing::Bytes, _) => Some(Forward::Splice(e.addr)),
            (
                Framing::Http,
                Forward::Http {
                    origin,
                    mut headers,
                },
            ) => {
                headers.extend(e.tie.map(|t| (ORIGIN_TIE_HEADER.to_string(), t)));
                Some(Forward::Http { origin, headers })
            }
            (Framing::Http, other) => Some(other),
        }
    }

    fn entries_of(
        &self,
        alpn: &str,
        prefixes: &[&str],
        addr: SocketAddr,
        admit: &Admit,
        framing: Framing,
    ) -> Result<Vec<Entry>, OriginRefusal> {
        if !valid_alpn(alpn) {
            return Err(OriginRefusal::BadAlpn(alpn.to_string()));
        }
        let takes_prefixes = alpn.as_bytes() == ALPN;
        if takes_prefixes
            && (prefixes.is_empty()
                || matches!(admit, Admit::MembersElse(_))
                || framing != Framing::Http)
            || (!takes_prefixes && !prefixes.is_empty())
        {
            return Err(OriginRefusal::PrefixShape);
        }
        if let Some(bad) = prefixes.iter().find(|p| !valid_prefix(p)) {
            return Err(OriginRefusal::BadPrefix(bad.to_string()));
        }
        let entry = |prefix: Option<&str>| Entry {
            alpn: alpn.to_string(),
            prefix: prefix.map(str::to_string),
            addr,
            admit: admit.clone(),
            framing,
            tie: None,
            declared: Vec::new(),
            claims: None,
            namespaces: Vec::new(),
        };
        Ok(if takes_prefixes {
            prefixes.iter().map(|p| entry(Some(p))).collect()
        } else {
            vec![entry(None)]
        })
    }

    fn refuse_taken(s: &State, entries: &[Entry]) -> Result<(), OriginRefusal> {
        for e in entries {
            let key = slot(&e.alpn, e.prefix.as_deref());
            if let Some(by) = s.holder(&key) {
                return Err(OriginRefusal::Taken { slot: key, by });
            }
        }
        Ok(())
    }

    fn read<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        self.mutate(|s| Ok::<T, std::convert::Infallible>(f(s)))
            .unwrap_or_else(|never| match never {})
    }

    /// Take the lock, sweep, run `f`, then re-derive the ALPN set OUTSIDE the
    /// lock. Every read and write goes through here, so expiry is observed
    /// by the next question and the hook cannot be forgotten. Poison is taken
    /// through for the reason `PublishedApps` gives: the state is a map and
    /// stays coherent.
    fn mutate<T, E>(&self, f: impl FnOnce(&mut State) -> Result<T, E>) -> Result<T, E> {
        let out = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.sweep();
            f(&mut state)
        };
        self.notify();
        out
    }

    fn notify(&self) {
        let hook = self
            .inner
            .hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(hook) = hook else { return };
        let mut alpns: Vec<Vec<u8>> = {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state
                .dialable()
                .map(|e| e.alpn.as_bytes().to_vec())
                .collect()
        };
        if self.apps.is_serving() {
            alpns.push(APP_ALPN.to_vec());
        }
        alpns.sort();
        alpns.dedup();
        {
            let mut served = self
                .inner
                .served
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if served.as_ref() == Some(&alpns) {
                return;
            }
            *served = Some(alpns.clone());
        }
        hook(alpns);
    }
}

#[cfg(test)]
#[path = "origins_tests.rs"]
mod tests;
