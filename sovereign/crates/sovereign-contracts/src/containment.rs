// SPDX-License-Identifier: AGPL-3.0-or-later
//! Boot admission for the DISTRIBUTED primary, the pure half: may ggml's RPC
//! client live in the loading process's own address space? The decision table
//! and its inputs' one reader, moved from `sovereign_compute::containment`
//! (pb-serve-distributes) so serve's boot guard and svrn's `doctor` render one
//! verdict without doctor linking the loader. The guard itself — reading the
//! config and refusing boot — stays the loader's; that module's doc carries
//! the incident (2026-07-27, ggml-rpc.cpp:386) this table exists for.

use crate::setup_config::SharedModelRole;

/// Is this process armed to serve RPC workers? One reader of
/// `SOVEREIGN_RPC_DISCOVER`.
///
/// Two sites feed a containment VERDICT (serve's boot guard and
/// `doctor_cmd`), so a divergence would mean the doctor reporting a
/// containment posture the loader does not actually run under. It lives
/// beside the verdict it feeds.
pub fn rpc_discovery_armed() -> bool {
    std::env::var("SOVEREIGN_RPC_DISCOVER").is_ok()
}

/// Environment override: proceed with an in-process distributed primary anyway.
///
/// Named like the other deliberate-risk escapes (`SOVEREIGN_SKIP_VRAM_CHECK`).
pub const OVERRIDE_ENV: &str = "SOVEREIGN_ALLOW_INPROCESS_DISTRIBUTED_PRIMARY";

/// What the boot guard decided about running a distributed primary in-process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainmentVerdict {
    /// Not a distributing node — nothing to say.
    NotApplicable,
    /// `[compute] distributed_primary` is on: the abort lands in the child.
    Armed,
    /// Election-eligible anchor. The hazard is one host election away, not
    /// present, so this warns rather than refusing — refusing every anchor
    /// would take out a whole fleet on upgrade.
    Warn,
    /// A declared host with the primary in-process. Refuse.
    Refuse,
    /// Refusable, but the operator set the override.
    RefuseOverridden,
}

impl ContainmentVerdict {
    /// Whether the daemon may continue booting.
    pub fn proceeds(self) -> bool {
        !matches!(self, ContainmentVerdict::Refuse)
    }
}

/// The decision table.
///
/// Deliberately **size-blind**. The abort is a teardown of REMOTE buffers, not a
/// local memory event: a small primary takes the stream-split branch of
/// `classify_placement`, still holds RPC buffers on its workers, and still
/// aborts the host when one of them disappears. The 2026-07-25 forced-tunnel
/// E2E ran precisely that shape — a 4B primary with `role = "host"` — so a size
/// threshold would carve a hole exactly where a real incident already lived.
/// Model size governs a different question (can this node survive a local
/// fallback), which `local_fit_verdict` already owns.
///
/// Also deliberately blind to `[models].fast`, `[iroh].enabled`, and pooled
/// memory: none of them changes whether the RPC client is in this process.
/// Four booleans and an enum is the whole rule, and that legibility is the
/// point — a guard nobody can reason about is a guard that gets disabled.
pub fn classify_containment(
    child_owns_primary: bool,
    role: SharedModelRole,
    pinned_host_is_self: bool,
    discover_forced_by_env: bool,
    override_set: bool,
) -> ContainmentVerdict {
    // Contained. Nothing else matters — this is the fixed configuration and it
    // must never be refused for any other reason.
    if child_owns_primary {
        return ContainmentVerdict::Armed;
    }

    let declared_host = matches!(role, SharedModelRole::Host) || pinned_host_is_self;
    // A hand-set SOVEREIGN_RPC_DISCOVER turns a node that would NOT otherwise
    // discover into one that does, and can win the host election — the CLI
    // power-user path into the same hazard.
    //
    // It is only *new information* when the role does not already imply
    // discovery. A serving role sets this very variable itself
    // (`apply_shared_model_role_to_env`, which runs earlier in boot), so
    // reading it unqualified means reading back something the daemon wrote
    // sixty lines ago — which made the `Anchor` arm below unreachable and
    // refused every anchor on the normal path, citing "declared host" at a node
    // that had declared nothing of the sort. Observed on BeefyMac 2026-07-29
    // with a verbatim `role = "anchor"` config.
    let role_implies_discovery = matches!(role, SharedModelRole::Host | SharedModelRole::Anchor);
    let operator_forced_discovery = discover_forced_by_env && !role_implies_discovery;
    if declared_host || operator_forced_discovery {
        return if override_set {
            ContainmentVerdict::RefuseOverridden
        } else {
            ContainmentVerdict::Refuse
        };
    }

    // An anchor lends memory to someone else's split and only reaches the
    // dangerous reload after WINNING the host election.
    if matches!(role, SharedModelRole::Anchor) {
        return ContainmentVerdict::Warn;
    }

    ContainmentVerdict::NotApplicable
}

