// Pure display helpers extracted from KnowledgeStatus.svelte (§3.3
// component decomposition): relative/absolute time formatting, the
// catalog-tier classifier, and the enrichment-phase label map. No runes,
// no IO — unit-tested; the component imports them.

export type CatalogTier = "featured" | "preview" | "hidden";

/** Compact "Ns/Nm/Nh/Nd ago" for a unix-seconds timestamp. `nowSecs` is
 *  injectable so the buckets are deterministically testable. Past-clamped
 *  (a future timestamp reads as "0s ago"). */
export function formatRelativeAgo(
  unixSecs: number,
  nowSecs: number = Math.floor(Date.now() / 1000),
): string {
  const diff = Math.max(0, nowSecs - unixSecs);
  if (diff < 60) return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}

/**
 * Catalog tier from `registry_snapshot.toml::catalog_status` — the
 * registry is the single source of truth so the UI never grows a parallel
 * allowlist. Anything missing the field defaults to "preview" (under
 * Coming soon, install disabled) so newly-registered recipes don't
 * accidentally surface as featured.
 */
export function catalogTier(
  catalogStatus: string | null | undefined,
): CatalogTier {
  switch (catalogStatus) {
    case "featured":
    case "hidden":
      return catalogStatus;
    case "preview":
    case null:
    case undefined:
    default:
      return "preview";
  }
}

/** Absolute year/month/day date for a unix-seconds timestamp. */
export function formatDate(unixSecs: number): string {
  return new Date(unixSecs * 1000).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/** Human label for an enrichment-pipeline phase; unknown phases echo back. */
export function phaseLabel(phase: string): string {
  switch (phase) {
    case "downloading":
      return "Downloading…";
    case "extracting":
      return "Extracting documents…";
    case "chunking":
      return "Chunking…";
    case "embedding":
      return "Embedding…";
    case "indexing":
      return "Building index…";
    case "extracting_claims":
      return "Extracting claims…";
    case "finding_relationships":
      return "Finding relationships…";
    case "extracting_relationships":
      return "Extracting relationships…";
    case "building_link_graph":
      return "Building link graph…";
    case "computing_profiles":
      return "Computing article profiles…";
    case "complete":
      return "Complete";
    case "failed":
      return "Failed";
    default:
      return phase;
  }
}
