// SPDX-License-Identifier: AGPL-3.0-or-later
// Preconditions for the demo beats.
//
// Every beat states what the world must look like before it can be
// filmed, and checks it against the LIVE daemon rather than assuming.
// The alternative — discovering in the edit that the peer was down, or
// that a corpus had chunks but no atlas — is how demo reels quietly
// become fiction.
//
// Everything here reads. Nothing here creates state: the demo attaches
// to the operator's real daemon, and a capture run must not mutate the
// index it's filming.
export const DAEMON = process.env.SOVEREIGN_DAEMON_URL ?? "http://127.0.0.1:9741";

export interface DaemonStatus {
  node_id?: string;
  mesh?: {
    name?: string;
    members_online?: number;
    members_total?: number;
    pooled_vram_gb?: number;
    pooled_storage_gb?: number;
  };
  inference?: {
    resident?: { role: string; model_id: string; resident: boolean; size_bytes: number | null }[];
  };
  knowledge?: { hosted_corpora?: string[]; total_chunks_searchable?: number };
}

let cached: DaemonStatus | null = null;

/** The daemon's own view of itself. Cached per process — beats run
 *  serially and a capture run is minutes long, but the numbers we quote
 *  on camera should come from one consistent snapshot. */
export async function daemonStatus(force = false): Promise<DaemonStatus> {
  if (cached && !force) return cached;
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), 5000);
  try {
    const res = await fetch(`${DAEMON}/status`, { signal: ctrl.signal });
    cached = (await res.json()) as DaemonStatus;
    return cached;
  } finally {
    clearTimeout(t);
  }
}

/** Corpus ids the daemon is actually hosting (chunks searchable). */
export async function hostedCorpora(): Promise<Set<string>> {
  const s = await daemonStatus();
  return new Set(s.knowledge?.hosted_corpora ?? []);
}

/** True when the named corpus is hosted. */
export async function hasCorpus(id: string): Promise<boolean> {
  return (await hostedCorpora()).has(id);
}

/** A corpus can be hosted (chunks indexed) and still be useless to a
 *  mesh app, which reads the ATLAS (entities/edges/timeline), not the
 *  chunk index. `meshapp_corpus_stats` is the honest probe: it goes
 *  through the exact op the bundle calls. Returns null when the atlas
 *  isn't there. */
export async function atlasStats(
  bridge: { invoke<T = unknown>(c: string, a?: Record<string, unknown>): Promise<T> },
  corpusId: string,
): Promise<Record<string, number> | null> {
  try {
    const stats = await bridge.invoke<Record<string, number>>("meshapp_corpus_stats", {
      corpusId,
    });
    // An atlas that exists but is empty is not filmable either.
    const atoms = Number(stats?.atoms ?? 0);
    return atoms > 0 ? stats : null;
  } catch {
    return null;
  }
}

export interface PlacementView {
  modelId: string | null;
  mode: string | null;
  workers: { endpoint: string; blocks: number }[];
  blocksLocal: number;
  blocksTotal: number;
}

/** Shared-model placement — the receipt for "this answer used a machine
 *  that isn't this one". Mirrors MeshDiagnosticsPanel's reader so the
 *  assertion and the pixels read the same field. */
export async function placement(bridge: {
  invoke<T = unknown>(c: string, a?: Record<string, unknown>): Promise<T>;
}): Promise<PlacementView | null> {
  let raw: Record<string, unknown> | null = null;
  try {
    raw = await bridge.invoke<Record<string, unknown> | null>("mesh_get_placement");
  } catch {
    return null;
  }
  if (!raw) return null;
  const p = raw.placement as Record<string, unknown> | undefined;
  if (!p) return null;
  const blocks = (p.blocks as { local?: number; total?: number } | undefined) ?? {};
  return {
    modelId: (raw.model_id as string) ?? null,
    mode: (p.mode as string) ?? null,
    workers: ((p.workers as { endpoint: string; blocks: number }[]) ?? []),
    blocksLocal: Number(blocks.local ?? 0),
    blocksTotal: Number(blocks.total ?? 0),
  };
}

/** How many mesh members the daemon can see right now. */
export async function meshOnline(): Promise<{ online: number; total: number; name: string }> {
  const s = await daemonStatus();
  return {
    online: Number(s.mesh?.members_online ?? 0),
    total: Number(s.mesh?.members_total ?? 0),
    name: s.mesh?.name ?? "",
  };
}

/** Live numbers for B8's closing caption. Read from the daemon at
 *  capture time — never typed into the overlay by hand. If the caption
 *  claims it, the machine reported it. */
export async function shelfFacts(): Promise<{
  corpora: number;
  chunks: number;
  peersOnline: number;
  peersTotal: number;
  pooledVramGb: number;
  meshName: string;
}> {
  const s = await daemonStatus(true);
  return {
    corpora: (s.knowledge?.hosted_corpora ?? []).length,
    chunks: Number(s.knowledge?.total_chunks_searchable ?? 0),
    peersOnline: Number(s.mesh?.members_online ?? 0),
    peersTotal: Number(s.mesh?.members_total ?? 0),
    pooledVramGb: Number(s.mesh?.pooled_vram_gb ?? 0),
    meshName: s.mesh?.name ?? "",
  };
}
