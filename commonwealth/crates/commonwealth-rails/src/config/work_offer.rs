// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rails.toml` `[work_offer]` — the donor's section, moved whole from svrn's
//! `[compute.work_offer]` with the donor (pb-work-donor). `svrn mesh up`
//! hands a section written before the move over to this file
//! (sovereign-cli-mesh `rail_migration::migrate_work_offer`).

use serde::{Deserialize, Serialize};

use oicp_types::WorkOffer;

/// `[work_offer]` — the units this node will run on behalf of other
/// people on the ring (cw-lift 5d).
///
/// # Zero value inert, closed-enum opt-in
///
/// `SharedModelSection`/`SharedModelRole`'s shape, for the same reason: a
/// node that says nothing must donate nothing. The default is `kinds = []`,
/// `max_concurrent = 0` and `accept = "nobody"`, and each of the three alone
/// is enough to make the donor take no unit. Donating somebody else's argv on
/// your own machine is not something a config migration may switch on.
///
/// # Flat and explicit, because `[compute]` is
///
/// Every field carries `#[serde(default)]` with NO `skip_serializing_if`, so
/// `save_to` materializes all of them literally —
/// `compute_section_is_serialized_explicitly_so_unset_is_not_recoverable`
/// states the rule for this section's parent and it is honoured here rather
/// than amended. The cost is real and is paid on purpose: "unset" and
/// "deliberately empty" are indistinguishable afterwards, so nothing may ever
/// auto-arm a donor from the absence of a key.
///
/// That rule is also why [`accept`](Self::accept) is an enum beside
/// [`accept_from`](Self::accept_from) rather than the `Option<Vec<String>>`
/// the wire type carries. `WorkOffer::accept_from` is a TRI-state — `None`
/// accepts anyone on the ring, `Some(list)` exactly that list, `Some(∅)`
/// nobody — and a flat `Vec` can only express two of the three. Spelling the
/// third as "an absent key" is precisely what this section may not do, so the
/// policy is named and the list is data (ARCH §2.1).
///
/// # What is NOT here, and why
///
/// `os`, `arch` and `isolation` are on the wire [`WorkOffer`] and are absent
/// from this section deliberately. The first two are `std::env::consts` — a
/// host that could mis-state them would be lying to its own submitters. The
/// third is a property of what this BUILD implements: `ProcessExecutor` runs
/// a child process and this repository ships no sandbox at all, so the
/// offered isolation is `Subprocess` and an operator-set knob could only make
/// it a claim the code does not honour. `oicp_types::Isolation` refuses a
/// `Default` for exactly that reason, and re-spelling it here as a config
/// enum with an `Unset` arm would be a second speller of a closed set
/// (ARCH §10.6). `crate::donor::DONOR_ISOLATION` is the one
/// answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkOfferSection {
    /// The job kinds this node runs, spelled `id:vN` (`process:v1`). Empty —
    /// the default — is a node that donates nothing.
    ///
    /// **An entry is offered only while an executor resolves it** — one of
    /// cw-rails' own, or an execute origin a program on this node registered
    /// (pb-work-donor). Until then the kind is dropped from the published
    /// offer with a `warn` naming it (phase-b-39 fork 2). An offer for a kind
    /// nothing can run is a donor that leases units and then fails every one
    /// of them, which is worse for the submitter than a node that never
    /// offered.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// The container image a unit runs INSIDE, on this host.
    ///
    /// Absent — the default — means this node has no boundary and therefore
    /// offers no kind that demands one, which today is every kind that runs a
    /// submitter's argv. Naming one is not permission: `Sandbox::probe` still
    /// has to find a rootless runtime and find the image present LOCALLY
    /// before the node will describe itself as isolating anything, and a
    /// config naming an image on a host without podman gets `Subprocess` with
    /// the reason on the boot trace (ARCH §18.3).
    ///
    /// It is the donor's operator who names it, and deliberately not the
    /// package (which ships no image and has no registry to fetch from) and
    /// not the submitter (whose unit would then choose the contents of its
    /// own sandbox, and pull an arbitrary reference onto somebody else's
    /// machine). Pull it yourself once; a donor does not fetch on your
    /// behalf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// How many units this node holds leases on at once. `0` — the default —
    /// takes nothing. Raising it is the throughput knob; the donor never
    /// exceeds it because the fold counts its own live leases.
    #[serde(default)]
    pub max_concurrent: u32,
    /// Stand down while the operator is at the keyboard.
    ///
    /// Defaults to `true`, which is the one field whose zero value is not the
    /// `Default::default()` of its type — `DEFAULT_YIELD_TO_FOREGROUND` is
    /// read by both the serde default and [`Default`] so there is one answer.
    /// A donor that keeps eight test shards running through its owner's chat
    /// turn is a donor its owner turns off.
    #[serde(default = "default_yield_to_foreground")]
    pub yield_to_foreground: bool,
    /// Whose work this node accepts. See the type docs for why the policy is
    /// an enum and the list is a separate field.
    #[serde(default)]
    pub accept: WorkAcceptFrom,
    /// The actor keys [`WorkAcceptFrom::Listed`] admits — 64 lowercase hex
    /// characters each, as `svrn ring roster` prints them. Read only under
    /// that policy; a boot with an unparseable entry is refused naming the
    /// entry, because a mis-spelled key is a set-membership test that
    /// silently never matches.
    #[serde(default)]
    pub accept_from: Vec<String>,
    /// The local checkouts this node will run repo-pinned work against.
    ///
    /// `oicp_types::OfferedRepo` itself, not a config twin of it: the wire
    /// type is already `{path, url}` of plain strings with no
    /// `skip_serializing_if`, so it is flat and explicit exactly as this
    /// section requires (ARCH §19 — the surface that already serves).
    ///
    /// LAST field: an array of tables, and TOML puts every scalar before it.
    #[serde(default)]
    pub repos: Vec<oicp_types::OfferedRepo>,
}

/// Whether a donor stands down for foreground work when nothing says
/// otherwise. ONE constant, read by both [`Default`] and serde, so a config
/// written by `save_to` and a config with the key absent resolve the same.
pub const DEFAULT_YIELD_TO_FOREGROUND: bool = true;

fn default_yield_to_foreground() -> bool {
    DEFAULT_YIELD_TO_FOREGROUND
}

impl Default for WorkOfferSection {
    fn default() -> Self {
        Self {
            kinds: Vec::new(),
            image: None,
            max_concurrent: 0,
            yield_to_foreground: DEFAULT_YIELD_TO_FOREGROUND,
            accept: WorkAcceptFrom::default(),
            accept_from: Vec::new(),
            repos: Vec::new(),
        }
    }
}

/// Whose submissions a donor accepts — the closed policy half of
/// `WorkOffer::accept_from`'s tri-state.
///
/// `SharedModelRole`'s shape: a small closed set whose default is the inert
/// end, so opting in is an edit somebody made on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkAcceptFrom {
    /// Accept nobody. The default, and `Some(∅)` on the wire — which is
    /// self-only rather than open, and is why an empty list is representable
    /// instead of being normalized into "anyone".
    #[default]
    Nobody,
    /// Accept exactly the actors in [`WorkOfferSection::accept_from`].
    Listed,
    /// Accept anyone the ring's roster admits. `None` on the wire. The ring
    /// roster is the only remaining gate, so this is the setting that says
    /// "everybody in this roster may run argv on my machine".
    Anyone,
}

impl WorkOfferSection {
    /// The wire offer this section describes, or `None` when the section is
    /// inert (`kinds` is empty).
    ///
    /// `os`/`arch`/`isolation` are the caller's because they are facts about
    /// the running build rather than about this file — see the type docs.
    ///
    /// Only the KIND spellings are validated here, because that is all this
    /// crate can see: whether a kind resolves to a registered executor is the
    /// donor's check, and it runs at boot against the registry
    /// (`crate::donor::resolve_offer`).
    pub fn to_offer(
        &self,
        os: &str,
        arch: &str,
        isolation: oicp_types::Isolation,
    ) -> Result<Option<WorkOffer>, InvalidWorkOffer> {
        if self.kinds.is_empty() {
            return Ok(None);
        }
        let mut kinds = Vec::with_capacity(self.kinds.len());
        for raw in &self.kinds {
            kinds.push(
                oicp_types::JobKind::parse(raw).map_err(|why| InvalidWorkOffer::Kind {
                    raw: raw.clone(),
                    why: why.to_string(),
                })?,
            );
        }
        Ok(Some(WorkOffer {
            kinds,
            max_concurrent: self.max_concurrent,
            yield_to_foreground: self.yield_to_foreground,
            isolation,
            os: os.to_string(),
            arch: arch.to_string(),
            repos: self.repos.clone(),
            accept_from: match self.accept {
                WorkAcceptFrom::Nobody => Some(Vec::new()),
                WorkAcceptFrom::Listed => Some(self.accept_from.clone()),
                WorkAcceptFrom::Anyone => None,
            },
        }))
    }
}

/// Why `[work_offer]` does not describe an offer. One variant per
/// rule, so a refusal names the fix (ARCH §18.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidWorkOffer {
    #[error("[work_offer] kinds contains `{raw}`, which is not a job kind: {why}")]
    Kind { raw: String, why: String },
}

#[cfg(test)]
#[path = "work_offer_tests.rs"]
mod tests;
