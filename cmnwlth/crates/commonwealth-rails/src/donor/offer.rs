// SPDX-License-Identifier: AGPL-3.0-or-later
//! The boot decision: `[work_offer]` against the executors this node has. A
//! sibling of `donor.rs` so it stays out of ARCH §3.1's approach band.
use super::*;

/// Why `[work_offer]` cannot become an offer this node may publish.
///
/// Every one is a REFUSED START of cw-rails that names the thing to fix. An
/// offer this node cannot honour is worse for a submitter than no offer at
/// all: the unit is leased, burns an attempt, and comes back with a verdict
/// about this node rather than about their tree (ARCH §18.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OfferRefused {
    /// A kind in the config is not spelled `id:vN`.
    #[error("{0}")]
    Config(#[from] InvalidWorkOffer),
    // A kind with no executor was a REFUSED BOOT of the daemon until
    // pb-work-donor. Here it is a dropped kind with a `warn`: an execute
    // origin a program on this node registers later resolves it (phase-b-39
    // fork 2), and cw-rails cannot tell that kind from a typo without naming
    // the program's vocabulary. The variant is gone rather than kept
    // unreachable, as the isolation floor's was on 2026-09-10.
    /// An `accept_from` entry is not an actor key.
    #[error("[work_offer] accept_from contains `{raw}`, which is not an actor key: {why}")]
    AcceptKey { raw: String, why: String },
}

/// Render a kind list for a refusal sentence — `none` rather than `[]`, so an
/// operator reading it is told the registry is empty rather than shown an
/// empty bracket they have to interpret.
pub(super) fn registered_list(kinds: &[JobKind]) -> String {
    if kinds.is_empty() {
        return "none".to_string();
    }
    kinds
        .iter()
        .map(|k| k.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The executor registry this node donates through.
///
/// One place, so the kinds an offer ACCEPTS and the kinds a running donor can
/// RESOLVE are the same set by construction rather than by two lists agreeing
/// (ARCH §10.6): cw-rails' own `process:v1`, and one [`OriginExecutor`] per
/// execute origin a program on this node has registered (pb-work-donor).
///
/// **The origins are why this takes a list (cw-lift 5g, pb-work-donor).**
/// An `ingest:v1` unit runs through the svrn daemon's own corpus engine, so a
/// node with no daemon serving that origin cannot run it — and the honest way
/// to say that is to register nothing, which makes [`resolve_offer`] drop the
/// kind from the offer and NAME it. The alternative, registering an executor
/// that fails at run time, is a donor that leases units and burns the
/// submitter's attempts (ARCH §18.3).
pub fn donor_registry(sandbox: Sandbox, origins: &[Arc<OriginExecutor>]) -> JobExecutorRegistry {
    let mut registry = JobExecutorRegistry::new();
    // `register` refuses a duplicate kind rather than overwriting, and both
    // `Result`s are surfaced rather than unwrapped, because a second
    // registration of one kind must not be able to vanish (ARCH §18.3).
    //
    // The sandbox goes INTO the executor rather than being consulted beside
    // it, so the boundary a unit actually runs in and the isolation this node
    // advertises are one value read twice — a node cannot run units one way
    // and describe itself another (ARCH §10.6).
    if let Err(e) = registry.register(Arc::new(
        commonwealth_work::process::ProcessExecutor::with_sandbox(sandbox),
    )) {
        warn!(target: TRACE_TARGET, error = %e, "work donor: an executor could not be registered");
    }
    for origin in origins {
        if let Err(e) = registry
            .register(Arc::clone(origin) as Arc<dyn commonwealth_work::executor::JobExecutor>)
        {
            warn!(target: TRACE_TARGET, error = %e, slot = %origin.slot(),
                  "work donor: an execute origin could not be registered");
        }
    }
    registry
}

/// Resolve `[work_offer]` against the executors this node has right now, or
/// refuse the start naming what is wrong.
///
/// `Ok(None)` is the inert config — no kinds, no offer. That is the shipped
/// posture and it is not an error.
///
/// **The invariant:** every OFFERED kind resolves to a registered executor
/// whose isolation this build covers. A configured kind with no executor yet
/// is dropped and named, and offered again once its origin registers
/// (`an_offered_kind_waits_for_its_origin_and_is_offered_when_it_registers`).
///
/// `os` and `arch` are WHERE A UNIT RUNS, not who is hosting it — the caller
/// reads them off `Sandbox::platform`, so under a boundary they are the
/// image's. They were `std::env::consts` until 2026-09-10; see the field docs
/// on [`WorkOffer::os`] for the machine that failure was found on.
pub fn resolve_offer(
    section: &WorkOfferSection,
    registry: &JobExecutorRegistry,
    os: &str,
    arch: &str,
    provides: Isolation,
) -> Result<Option<WorkOffer>, OfferRefused> {
    let Some(offer) = section.to_offer(os, arch, provides)? else {
        debug!(
            target: TRACE_TARGET,
            "work donor: [work_offer] names no kinds, so this node donates nothing"
        );
        return Ok(None);
    };
    for raw in section.accept_from.iter() {
        if let Err(why) = ActorKey::parse(raw) {
            return Err(OfferRefused::AcceptKey {
                raw: raw.clone(),
                why: why.to_string(),
            });
        }
    }
    // THE invariant: offered kinds are a SUBSET of registry kinds.
    // `registry.kinds()` is the same set `registry.resolve` answers from, so
    // a kind that passes here cannot fail to resolve in the loop — one
    // decider, not two lists that agree today (ARCH §10.6).
    // THE ISOLATION FLOOR IS A DROP, NOT A REFUSED BOOT (2026-09-10).
    //
    // An isolation shortfall is OURS — it is this build changing what a kind
    // demands under a config that was valid when it was written. Every node
    // on this mesh carries `kinds = ["process:v1"]` today, so refusing the
    // start would take them DOWN on their next restart to enforce a rule
    // about work they are not currently doing, which trades a security
    // posture for an outage.
    //
    // So the kind is dropped from the published offer and the drop is NAMED
    // at `warn` — never silently, which would be the §18.3 substitution. If
    // that empties the offer, this node publishes none and donates nothing,
    // which is exactly the shipped posture for a node that offers no kind.
    // The partition is `commonwealth-work`'s, not this module's — the floor
    // has to reach a donor built from the package alone, and a second copy
    // here is the §10.6 twin this campaign has now closed three times.
    //
    // An UNREGISTERED kind is dropped the same way (phase-b-39 fork 2): its
    // execute origin may register after this start, and the offer is
    // re-resolved when it does. With the daemon down there is no donor
    // offering `ingest:v1` today either.
    let partition = registry.offerable(&offer.kinds, provides);
    for kind in &partition.unregistered {
        warn!(
            target: TRACE_TARGET,
            kind = %kind,
            registered = %registered_list(&registry.kinds()),
            "work donor: NOT offering this kind — no executor in cw-rails and no \
             execute origin registered for it on this node. It is offered once a \
             program registers its origin; if it is a typo, fix `kinds`"
        );
    }
    for dropped in &partition.dropped {
        warn!(
            target: TRACE_TARGET,
            kind = %dropped.kind,
            required = ?dropped.required,
            provides = ?dropped.provides,
            "work donor: NOT offering this kind — {dropped}, so a unit of it \
             would run with this node's user, filesystem and network behind \
             nothing but consent. The config is left alone; the kind is \
             dropped from the offer. A container-backed executor is what \
             lifts this, not a config key"
        );
    }
    let offerable = partition.offerable;
    if offerable.is_empty() {
        info!(
            target: TRACE_TARGET,
            configured = %registered_list(&offer.kinds),
            "work donor: every configured kind was dropped (the isolation floor, \
             or no executor yet), so this node publishes no offer and takes nothing"
        );
        return Ok(None);
    }
    let offer = WorkOffer {
        kinds: offerable,
        ..offer
    };
    if offer.max_concurrent == 0 {
        // Reported, not corrected. `may_take` will refuse every unit with
        // `Concurrency { held: 0, max: 0 }`, which is a working donor that
        // takes nothing — the exact shape of a silent misconfiguration, so it
        // is named once at boot where somebody is reading (ARCH §18.3).
        warn!(
            target: TRACE_TARGET,
            kinds = %registered_list(&offer.kinds),
            "work donor: [work_offer] names kinds but max_concurrent is 0, so this \
             node advertises work it will then refuse every unit of — set max_concurrent"
        );
    }
    info!(
        target: TRACE_TARGET,
        kinds = %registered_list(&offer.kinds),
        max_concurrent = offer.max_concurrent,
        yield_to_foreground = offer.yield_to_foreground,
        repos = offer.repos.len(),
        registered = %registered_list(&registry.kinds()),
        "work donor: this node offers work"
    );
    Ok(Some(offer))
}
