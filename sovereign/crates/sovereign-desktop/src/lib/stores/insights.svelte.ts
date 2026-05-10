import { produce } from "immer";
import type { InsightNodeDto } from "../types";
import { listInsights, deleteInsight } from "../api";

// All writes go through `produce()` so the array reference changes on
// every mutation — see docs/frontend-state.md. Today the mutators here
// all rewrite the whole array, so the discipline is cheap; it becomes
// load-bearing once we add nested fields or partial updates.
let _nodes: InsightNodeDto[] = $state([]);
let _loading = $state(false);

export const insightStore = {
  get items() {
    return _nodes;
  },
  get count() {
    return _nodes.length;
  },
  get loading() {
    return _loading;
  },

  /** Check if a specific paragraph in a message is already clipped. */
  has(messageId: string, paragraphIndex: number): boolean {
    return _nodes.some(
      (n) =>
        n.message_id === messageId && n.paragraph_index === paragraphIndex,
    );
  },

  /** Load from persistent store on app start or conversation change. */
  async init() {
    _loading = true;
    try {
      _nodes = await listInsights(100);
    } finally {
      _loading = false;
    }
  },

  /** Called immediately after a clip action — node already persisted. */
  add(node: InsightNodeDto) {
    _nodes = produce(_nodes, (draft) => {
      draft.unshift(node);
    });
  },

  /** Remove an insight by ID. */
  async remove(id: string) {
    await deleteInsight(id);
    _nodes = produce(_nodes, (draft) => {
      const idx = draft.findIndex((n) => n.id === id);
      if (idx !== -1) draft.splice(idx, 1);
    });
  },
};
