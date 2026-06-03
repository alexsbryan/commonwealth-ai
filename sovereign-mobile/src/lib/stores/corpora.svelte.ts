// Loaded CORPUS_REFs for the active host, used to badge a cited source
// as private-to-this-host (acceptance §7). Refreshed by App on load /
// reconnect; reads fall back to cache offline.

import { listCorpora } from "../api";
import type { CorpusRef } from "../types";

let corpora = $state<CorpusRef[]>([]);

export const corporaStore = {
  get all(): CorpusRef[] {
    return corpora;
  },
  async refresh(): Promise<void> {
    try {
      corpora = await listCorpora();
    } catch {
      /* offline + empty cache → leave as-is */
    }
  },
  /** A source is private when it's explicitly local / not mesh-shared.
   *  Unknown corpora are not badged (we don't claim privacy we can't
   *  confirm). The conversation corpus is always private. */
  isPrivate(corpusId: string): boolean {
    const c = corpora.find((x) => x.corpus_id === corpusId);
    if (!c) return false;
    return c.mesh_shared === false || c.scope === "local";
  },
};
