//! Tailnet reachability detection.
//!
//! The **hard guarantee** (fail-closed off-tailnet) is structural, not
//! heuristic: the `ApiClient` is constructed with exactly one origin —
//! the host's `tailnet_address` — and never resolves a public-DNS
//! fallback. So we can only ever dial the tailnet. What this module
//! adds is the *user-facing distinction* between "you're off the
//! tailnet" (OffTailnet) and "the tailnet is up but the host isn't
//! answering" (HostDown), which need different prompts.
//!
//! `tailnet_present()` is a best-effort probe. The robust per-platform
//! implementation (enumerate interfaces for the Tailscale `utun*` /
//! `tun*` device, or call the Tailscale LocalAPI) is a pin-time task;
//! the stub assumes present so the monitor falls through to the host
//! probe. Misclassifying OffTailnet-as-HostDown is acceptable for v1
//! because the fail-closed guarantee above never opens a non-tailnet
//! route regardless.
pub fn tailnet_present() -> bool {
    // TODO(pin-time): per-platform interface enumeration / Tailscale
    // LocalAPI. Until then, assume the overlay may be up and let the
    // authenticated host probe decide Reachable vs HostDown.
    true
}
