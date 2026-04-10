import type { InsightNodeDto } from "../types";
import { listInsights, deleteInsight } from "../api";

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
    _nodes = [node, ..._nodes];
  },

  /** Remove an insight by ID. */
  async remove(id: string) {
    await deleteInsight(id);
    _nodes = _nodes.filter((n) => n.id !== id);
  },
};
