// SPDX-License-Identifier: AGPL-3.0-or-later
// Pure mesh display/format helpers extracted from MeshSettings.svelte
// (§3.3 component decomposition): invite-link relay toggling, byte/token/
// GB formatting, and the relay-kind + member-status label maps. No runes,
// no IO — unit-tested; the component imports them.

/** Append `?relay=<value>` (or `&relay=…` if other params already
 *  exist) to the bare invite link. Idempotent: any existing relay
 *  query param gets replaced, so toggling between candidates
 *  doesn't accumulate junk.
 *
 *  Must NOT use `URL.searchParams.set()` — that percent-encodes
 *  reserved chars (`:` → `%3A`, `'` → `%27`) which makes the link
 *  ugly in a chat client AND produces `relay=100.64.0.2%3A9742`
 *  that older daemon builds (pre-percent-decode fix) fail to parse.
 *  Mirror `build_join_link` in Rust instead — its query format is
 *  the canonical one the parser's round-trip test locks in. */
export function withRelay(baseLink: string, relay: string | null): string {
  // Strip any existing `relay=` param so toggling is idempotent.
  const qIdx = baseLink.indexOf("?");
  let base = baseLink;
  let query: string[] = [];
  if (qIdx >= 0) {
    base = baseLink.slice(0, qIdx);
    query = baseLink
      .slice(qIdx + 1)
      .split("&")
      .filter((p) => p.length > 0 && !p.startsWith("relay="));
  }
  if (relay) query.push(`relay=${relay}`);
  return query.length > 0 ? `${base}?${query.join("&")}` : base;
}

/** Human label for a relay candidate's `kind`; unknown kinds echo back. */
export function relayLabel(kind: string): string {
  switch (kind) {
    case "tailscale":
      return "Tailscale (works across networks)";
    case "lan":
      return "Local network only";
    case "ipv6":
      return "IPv6 (sometimes routable)";
    default:
      return kind;
  }
}

/** Binary (1024-based) byte size with a unit suffix; non-finite / ≤0 → "0 B". */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v < 10 ? v.toFixed(1) : v.toFixed(0)} ${units[i]}`;
}

/** Compact token count: raw < 1k, `N.Nk` < 1M, else `N.NNM`. ≤0 → "0". */
export function formatTokens(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0";
  if (n < 1_000) return n.toString();
  if (n < 1_000_000) return `${(n / 1_000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}

/** GB size, rendering sub-1 GB as MB; non-finite / ≤0 → "0 GB". */
export function formatGb(gb: number): string {
  if (!Number.isFinite(gb) || gb <= 0) return "0 GB";
  if (gb < 1) return `${(gb * 1024).toFixed(0)} MB`;
  return `${gb < 10 ? gb.toFixed(1) : gb.toFixed(0)} GB`;
}

/** Member presence → CSS dot class. Unknown states fall back to offline. */
export function statusDot(status: string): string {
  switch (status) {
    case "online":
      return "online";
    case "busy":
      return "busy";
    case "away":
      return "away";
    default:
      return "offline";
  }
}
