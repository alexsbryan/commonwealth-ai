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

export type Bridge = {
  invoke<T = unknown>(c: string, a?: Record<string, unknown>): Promise<T>;
};

export interface AtlasCorpus {
  corpus_id: string;
  display_name: string;
  total_atoms: number;
  atom_counts?: Record<string, number>;
}

/** A corpus can be hosted (chunks indexed) and still be useless to a
 *  mesh app, which reads the ATLAS (entities/edges/timeline), not the
 *  chunk index.
 *
 *  This deliberately does NOT probe via `meshapp_corpus_stats`, which
 *  looks like the more honest choice — it is the exact op the bundle
 *  calls — but cannot work from a test. Every `meshapp_*` command runs
 *  `authorize(installs, webview.label(), …)`, and the test command
 *  bridge always invokes as the MAIN window, so the probe returns
 *  "denied: caller is not a mesh-app window" for a fully-built atlas.
 *  A gate built on it reports "no atlas" about a corpus with 3,722
 *  documents in it. `atlas_list_corpora` is the host-side reader of the
 *  same data and is not label-gated. */
export async function atlasCorpora(bridge: Bridge): Promise<AtlasCorpus[]> {
  return bridge.invoke<AtlasCorpus[]>("atlas_list_corpora").catch(() => [] as AtlasCorpus[]);
}

/** The built atlas for `corpusId`, or null when there isn't one. */
export async function atlasBuilt(
  bridge: Bridge,
  corpusId: string,
): Promise<AtlasCorpus | null> {
  const all = await atlasCorpora(bridge);
  const found = all.find((c) => c.corpus_id === corpusId);
  return found && found.total_atoms > 0 ? found : null;
}

export interface ConvCorpus {
  corpus_id: string;
  display_name: string;
  conv_count: number;
  state_counts?: Record<string, number>;
}

/** The TIERED map — `conv_skeletons` + `conv_raptor_nodes` in SQLite,
 *  the artifact a vault or watched folder actually gets. Distinct from
 *  `atlasCorpora` above, which reads `atlas/atoms.json`: a corpus can
 *  have either, both, or neither, and `notebook_list` marks it
 *  explorable if EITHER exists (see `commands/corpus.rs` — the
 *  explorable set is a union of the two readers). `AtlasSurface` then
 *  routes to the matching view, so "which map does it have" decides
 *  which surface a beat is filming. */
export async function convCorpora(
  bridge: Bridge,
): Promise<{ corpora: ConvCorpus[]; error: string | null }> {
  // Deliberately NOT `.catch(() => [])`. `atlas_list_conv_corpora`
  // rejects with "Sqlite store not initialised" when the desktop has no
  // tiered store open at all — a different world from "this corpus
  // hasn't been read yet", with a different fix. Swallowing it makes an
  // infrastructure failure wear the costume of an empty result, and a
  // beat then skips with remediation advice that cannot work.
  try {
    return { corpora: await bridge.invoke<ConvCorpus[]>("atlas_list_conv_corpora"), error: null };
  } catch (e) {
    return { corpora: [], error: e instanceof Error ? e.message : String(e) };
  }
}

/** The tiered map for `corpusId` with at least one note in state
 *  `Ready`, or null. A corpus mid-build reports rows in `Pending` /
 *  `PartiallyReady`; those render as a half-populated list, so they are
 *  not a filmable precondition. */
export async function convCorpusReady(
  bridge: Bridge,
  corpusId: string,
): Promise<{
  corpus: ConvCorpus;
  readyCount: number;
} | { corpus: null; readyCount: 0; why: string }> {
  const { corpora, error } = await convCorpora(bridge);
  if (error) {
    return { corpus: null, readyCount: 0, why: `atlas_list_conv_corpora failed: ${error}` };
  }
  const found = corpora.find((c) => c.corpus_id === corpusId);
  if (!found) {
    return {
      corpus: null,
      readyCount: 0,
      why:
        `no tiered map for \`${corpusId}\`. Reported: ` +
        `[${corpora.map((c) => `${c.corpus_id}:${c.conv_count}`).join(", ") || "none"}]`,
    };
  }
  const readyCount = Number(found.state_counts?.Ready ?? 0);
  return readyCount > 0
    ? { corpus: found, readyCount }
    : {
        corpus: null,
        readyCount: 0,
        why:
          `\`${corpusId}\` has ${found.conv_count} note(s) in the tiered store but none in ` +
          `state Ready (${JSON.stringify(found.state_counts ?? {})}) — a build in flight, ` +
          `not a finished map`,
      };
}

export interface MeshAppInstall {
  app_id: string;
  name: string;
  granted: Record<string, boolean>;
  trust?: string;
}

/** The recorded install + granted permission subset for a mesh app.
 *  `meshapp_list_installs` is host-only (it refuses a mesh-app caller),
 *  which is exactly why a test can read it. */
export async function meshAppInstall(
  bridge: Bridge,
  appId: string,
): Promise<MeshAppInstall | null> {
  const installs = await bridge
    .invoke<MeshAppInstall[]>("meshapp_list_installs")
    .catch(() => [] as MeshAppInstall[]);
  return installs.find((i) => i.app_id === appId) ?? null;
}

export interface CorpusEntry {
  id: string;
  name: string;
  status: string;
  chunks_count: number | null;
}

/** The desktop's own view of an installed corpus — chunk count included.
 *  The honest gate for a mesh app that reads the chunk index rather than
 *  the atlas (the Today feed resolves through `document_feed`, which
 *  reads documents, which is why its empty `atlas/` dir is correct and
 *  an atlas gate would wrongly skip it). */
export async function corpusEntry(
  bridge: Bridge,
  corpusId: string,
): Promise<CorpusEntry | null> {
  const all = await bridge
    .invoke<CorpusEntry[]>("list_corpora")
    .catch(() => [] as CorpusEntry[]);
  return all.find((c) => c.id === corpusId) ?? null;
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
