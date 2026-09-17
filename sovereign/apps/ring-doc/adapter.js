// The durable doc adapter: the rail's acts in, one Yjs document out.
//
// Everything here is a pure function over values, so it runs under
// `node --test` with no browser and no daemon. `app.js` is the half that
// touches `window.ring`, the DOM and the clock; this half is the half with
// the rules, and the rules are the ones that can be got wrong silently.
//
// The property that makes this work is Yjs's, not ours: updates are
// commutative, associative and idempotent. The rail's total order is a
// convenience — it gives every node the same list to walk — and NOT something
// convergence depends on. `adapter.test.mjs` pins both halves of that: the
// same acts in two orders reach the same state vector, and an act applied
// twice is applied once.
import { Y } from "./vendor/ring-doc-bundle.js";

/// The act kind this app writes and reads. One string, one place — an app
/// whose writer and reader spell the kind separately stops converging the day
/// one of them is edited.
export const DOC_CHANGE = "doc-change";

/// The document this app edits. The act carries it so one ring can hold more
/// than one doc later without the reader needing to change.
export const DOC_ID = "ring-doc";

/// The `origin` stamped on every `Y.applyUpdate` that came from the rail.
///
/// `app.js` checks for it in `doc.on('update', ...)` and does NOT append.
/// Without it, applying a peer's act produces a local update event, which
/// appends an act, which the peer applies, which appends — the two nodes
/// write to the journal forever over a document that has stopped changing.
export const FROM_RAIL = "rail";

// base64 in chunks. `String.fromCharCode(...bytes)` spreads the whole array
// onto the call stack and throws on a document of any size; 0x8000 is the
// conventional safe stride.
const CHUNK = 0x8000;

/// A Yjs update as a JSON-safe string. The rail's payload must be whole
/// numbers and strings — two nodes have to derive identical bytes from an act
/// — so the update travels base64, never as an array of numbers.
export function bytesToB64(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i += CHUNK) {
    s += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK));
  }
  return btoa(s);
}

/// The inverse. Throws on anything that is not base64, which is what we want:
/// a malformed act is a gap to report, never an update to guess at.
export function b64ToBytes(b64) {
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i += 1) out[i] = s.charCodeAt(i);
  return out;
}

/// The act this app writes. One act per debounce window, never one per
/// keystroke.
export const changeAct = (update, doc = DOC_ID) => ({
  kind: DOC_CHANGE,
  doc,
  update: bytesToB64(update),
});

/// The reducer half of the read path, over ONE act at a time.
///
/// Written as a reducer rather than a loop over `log.ops` because the SDK's
/// `fold` is what knows which acts survived a correction. Walking `ops`
/// directly would apply a voided update — and Yjs has no un-apply, so the
/// document would be wrong with nothing on screen to say so.
export function docReducer(acc, payload, op) {
  if (!payload || payload.kind !== DOC_CHANGE || payload.doc !== acc.doc) {
    return acc;
  }
  if (typeof payload.update !== "string") {
    acc.gaps.push(`act ${op.id} carries a ${typeof payload.update} update`);
    return acc;
  }
  // `actor` and `person` ride along for attribution. They come from the
  // SIGNER the rail authenticated, never from anything inside the update.
  acc.acts.push({
    id: op.id,
    actor: op.actor,
    person: op.person,
    update: payload.update,
  });
  return acc;
}

export const initial = (doc = DOC_ID) => ({ doc, acts: [], gaps: [] });

/// Every `doc-change` act for `doc`, in the rail's order, voided ones already
/// gone.
///
/// `fold` is a parameter rather than a global because that is the whole reason
/// this file tests without a browser — `app.js` hands it `window.ring.fold`,
/// which is the one implementation of "which acts survived".
export function decodeActs(log, fold, doc = DOC_ID) {
  return fold(log, docReducer, initial(doc));
}

/// Apply the acts this document has not seen, and say which ids were applied.
///
/// `applied` is a `Set` of op ids the caller owns across polls. Deduping here
/// rather than trusting idempotency is not belt-and-braces: re-applying every
/// act on every 500 ms poll is O(journal) work per tick, and the cost shows up
/// as typing latency long before it shows up as a wrong document.
export function applyNew(ydoc, acts, applied) {
  const fresh = [];
  for (const act of acts) {
    if (applied.has(act.id)) continue;
    applied.add(act.id);
    fresh.push(act);
  }
  if (fresh.length === 0) return fresh;
  // One transaction for the batch: Yjs fires one update event for the whole
  // transaction, so a poll that pulls twenty peer acts does not produce twenty
  // observer passes over the editor.
  Y.transact(
    ydoc,
    () => {
      for (const act of fresh) Y.applyUpdate(ydoc, b64ToBytes(act.update));
    },
    FROM_RAIL,
  );
  return fresh;
}
