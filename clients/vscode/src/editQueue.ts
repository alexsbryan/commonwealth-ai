// Client-side tab-through queue over a /v1/edit_predictions response
// (NEXT_EDIT.md §3). Pure. The daemon returns the full remaining-site
// queue in one response; the client renders one edit at a time and
// keeps the rest positionally correct as accepts land — every accept
// shifts the offsets of edits later in the document by the length
// delta. All offsets are UTF-16 code units, i.e. plain JS string
// offsets.

import { EditPredictionEdit } from "./client";

export interface QueuedEdit {
  start: number;
  end: number;
  newText: string;
  /** What the range said when the daemon predicted — revalidated
   *  against the live document before every apply. */
  oldText: string;
}

/** Materialize the response queue, capturing each edit's expected
 *  old text from the exact text the request carried. */
export function buildQueue(requestText: string, edits: EditPredictionEdit[]): QueuedEdit[] {
  return edits.map((e) => ({
    start: e.start,
    end: e.end,
    newText: e.new_text,
    oldText: requestText.slice(e.start, e.end),
  }));
}

/** Offsets of the remaining queue after `applied` landed: edits
 *  positioned after it shift by the length delta; edits before it
 *  (the queue wraps around the cursor) are untouched. */
export function shiftAfterApply(queue: QueuedEdit[], applied: QueuedEdit): QueuedEdit[] {
  const delta = applied.newText.length - (applied.end - applied.start);
  return queue.map((e) =>
    e.start > applied.start ? { ...e, start: e.start + delta, end: e.end + delta } : e,
  );
}
