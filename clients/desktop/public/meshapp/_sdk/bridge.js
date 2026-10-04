// SPDX-License-Identifier: AGPL-3.0-or-later
// MeshApp SDK — the host bridge client.
//
// `window.meshApp` is injected by the host (see `meshapp_shim.js`) and is the
// ONLY channel to the host: permission-gated, deterministic, read-only. This
// wraps it in a corpus-bound client so a bundle doesn't repeat its corpus id on
// every call, normalizes the chunk-id to a string, and fails with a friendly
// message when the shim didn't load. The method surface mirrors the shim 1:1.

/** True when the host bridge shim is present. */
export const hasBridge = () => !!window.meshApp;

/**
 * A corpus-bound view of `window.meshApp`. Throws a clear error if the shim is
 * absent (the bundle should `catch` and surface it). Every method drops the
 * leading `corpusId` arg — it's bound here.
 */
export function connect(corpus) {
  const m = window.meshApp;
  if (!m) {
    throw new Error("window.meshApp is not available — the host bridge shim did not load.");
  }
  return {
    corpus,
    // capability / generic
    capabilities: () => m.capabilities(),
    readCorpus: (atomIds) => m.readCorpus(corpus, atomIds),
    readChunk: (chunkId) => m.readChunk(corpus, String(chunkId)),
    documentFeed: (limitDocs) => m.documentFeed(corpus, limitDocs),
    // graph-explorer family
    graph: (nodeType, limit) => m.graph(corpus, nodeType, limit),
    subgraph: (nodeType, limit) => m.subgraph(corpus, nodeType, limit),
    node: (id) => m.node(corpus, id),
    findings: (pattern) => m.findings(corpus, pattern),
    search: (query, nodeType, limit) => m.searchEntities(corpus, query, nodeType, limit),
    claims: (limit) => m.claims(corpus, limit),
    questions: (limit) => m.questions(corpus, limit),
    reconciliation: () => m.reconciliation(corpus),
    corpusStats: () => m.corpusStats(corpus),
    timeline: () => m.timeline(corpus),
    // deterministic-compute family (LVT)
    searchParcels: (query, limit) => m.searchParcels(corpus, query, limit),
    parcelAnalytics: (businessTaxTarget) => m.parcelAnalytics(corpus, businessTaxTarget),
    // precomputed story artifact (Wrapped)
    wrappedArtifact: () => m.wrappedArtifact(corpus),
    // host navigation: open Outer Work (chat) scoped to this corpus
    openOuterWork: () => m.openOuterWork(corpus),
  };
}

/** The corpus-derived one-line description an atlas entity carries, or "". */
export const describe = (n) =>
  n && n.attributes && typeof n.attributes.description === "string" ? n.attributes.description : "";
