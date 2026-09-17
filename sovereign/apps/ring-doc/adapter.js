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
import { Y, awarenessProtocol } from "./vendor/ring-doc-bundle.js";

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

// ---------------------------------------------------------------------------
// Presence — the LIVE lane, not the rail.
//
// A cursor is delivery, never record: y-protocols drops a peer whose last
// awareness update is `outdatedTimeout` old (30 s), and `ring_sync` runs on a
// 60 s tick, so a cursor carried by the journal would have faded before it
// arrived. Everything below travels `POST /v1/rail/live` and is gone on a
// restart, which `sovereign-api/src/routes_rail_live.rs` proves rather than
// asserts.
//
// **The name on a cursor is self-asserted and this file knows it.** The
// SENDER looks its own key up in the roster and puts the person it found in
// its awareness state; a receiver has no way to check that, because awareness
// is unauthenticated by its own spec (`ring-apps-shelf.md` §identity). That is
// why a name is the MOST a cursor carries and why per-character colouring is
// not shown — attribution that a reader can check is act-level, from the
// signer, and lives in the `doc-change` acts above.

/// The envelope kind on the live lane. The buffer is one per daemon, so a
/// payload says what it is; anything else on the lane is skipped, not guessed
/// at.
export const PRESENCE_KIND = "awareness";

/// The `origin` stamped on every `applyAwarenessUpdate` that came from a peer.
///
/// `app.js` checks for it the way it checks [`FROM_RAIL`]: without it, applying
/// a peer's presence fires a local awareness update, which posts it back to
/// every peer, which applies it — three nodes saturating the live lane over a
/// document nobody is typing in.
export const FROM_LIVE = "live";

/// The person this key signs for, or `null` when the roster does not know it.
///
/// `members` is the `person → [keys]` map (`log.roster.members`), the same one
/// `expenses.js` takes — a key can belong to two laptops of one person, so the
/// lookup is over the values and the answer is the person.
///
/// `null` is a real answer and is rendered as one. A page that fell back to
/// the key, or to "someone", would be substituting for an absence (ARCH §6).
export function personFor(members, key) {
  if (!key) return null;
  for (const [person, keys] of Object.entries(members || {})) {
    if ((keys || []).includes(key)) return person;
  }
  return null;
}

/// A stable colour for a name, so one person is the same colour on all three
/// screens without anyone agreeing on a palette.
///
/// Derived from the NAME rather than the Yjs clientID: a clientID is new on
/// every reload, which would recolour everyone whenever anybody refreshed.
///
/// **`#rrggbb`, and that is not a style preference.** Tiptap's caret runs the
/// colour through `isValidColor`, whose regex is exactly that, and substitutes
/// `transparent` for anything else — so an `hsl()` string renders an invisible
/// cursor with no error anywhere. The hue is spread over the wheel and
/// converted here rather than left to CSS for that reason alone.
export function colorFor(name) {
  const text = String(name || "");
  let hue = 0;
  for (let i = 0; i < text.length; i += 1) {
    hue = (hue * 31 + text.charCodeAt(i)) % 360;
  }
  // hsl(hue, 70%, 45%) by hand: chroma 0.63, mid 0.135.
  const c = 0.63;
  const x = c * (1 - Math.abs(((hue / 60) % 2) - 1));
  const m = 0.45 - c / 2;
  const sector = Math.floor(hue / 60) % 6;
  const rgb = [
    [c, x, 0],
    [x, c, 0],
    [0, c, x],
    [0, x, c],
    [x, 0, c],
    [c, 0, x],
  ][sector];
  const hex = (v) =>
    Math.round((v + m) * 255)
      .toString(16)
      .padStart(2, "0");
  return `#${hex(rgb[0])}${hex(rgb[1])}${hex(rgb[2])}`;
}

/// One awareness update as a live-lane payload.
///
/// Base64 inside JSON text, the same spelling a `doc-change` act uses: the
/// lane carries text (`routes_rail_live.rs`), and one encoding for both lanes
/// is one decoder to get wrong.
export const presenceEnvelope = (update, doc = DOC_ID) =>
  JSON.stringify({ kind: PRESENCE_KIND, doc, awareness: bytesToB64(update) });

/// The awareness updates in a drain, and what could not be read.
///
/// Takes what `GET /v1/rail/live` returned — payload strings — and returns
/// `Uint8Array`s to hand to `applyAwarenessUpdate`. A payload for another doc
/// or another kind is skipped silently (it is not ours); a payload that claims
/// to be ours and cannot be read is a GAP, because a cursor that never appears
/// otherwise looks exactly like a peer who is not typing.
export function decodePresence(payloads, doc = DOC_ID) {
  const updates = [];
  const gaps = [];
  for (const raw of payloads || []) {
    let env;
    try {
      env = JSON.parse(raw);
    } catch (e) {
      gaps.push(`a live payload is not JSON: ${String(e.message || e)}`);
      continue;
    }
    if (!env || env.kind !== PRESENCE_KIND || env.doc !== doc) continue;
    try {
      updates.push(b64ToBytes(env.awareness));
    } catch (e) {
      gaps.push(`a live presence payload is not base64: ${String(e.message || e)}`);
    }
  }
  return { updates, gaps };
}

/// Apply a drain's presence onto this node's awareness map.
///
/// Stamped [`FROM_LIVE`] so the sender in `app.js` does not echo it back.
export function applyPresence(awareness, updates) {
  for (const update of updates) {
    awarenessProtocol.applyAwarenessUpdate(awareness, update, FROM_LIVE);
  }
}

/// This node's own awareness state, as bytes to send.
///
/// Only THIS client: a node that re-broadcast the peers it had heard from
/// would keep a departed cursor alive on the third node forever, because every
/// rebroadcast refreshes the `lastUpdated` that `outdatedTimeout` reads.
export const encodeSelf = (awareness) =>
  awarenessProtocol.encodeAwarenessUpdate(awareness, [awareness.clientID]);
