// Scaffolded by `svrn ring new`, then made into a document. The page.
// Everything it talks to is `window.ring`; everything it KNOWS about Yjs acts
// is in `./adapter.js`.
//
// That split is the shape of a ring app. The rail hands you signed acts in one
// agreed order; here the reducer turns them into a CRDT document rather than a
// set of balances. This file owns the three things a pure function cannot be
// given: the DOM, the clock, and `window.ring`.
import {
  Collaboration,
  CollaborationCaret,
  Editor,
  StarterKit,
  Y,
  awarenessProtocol,
} from "./vendor/ring-doc-bundle.js";
import {
  DOC_ID,
  FROM_LIVE,
  FROM_RAIL,
  applyNew,
  applyPresence,
  changeAct,
  colorFor,
  decodeActs,
  decodePresence,
  encodeSelf,
  personFor,
  presenceEnvelope,
} from "./adapter.js";

const el = (id) => document.getElementById(id);

// One act per window, not one per keystroke. A keystroke is 18–19 bytes of
// Yjs update and a journal line is signed, so the debounce is the difference
// between a document and a signature benchmark.
const DEBOUNCE_MS = 250;
// How long a peer's text waits before it is on screen. The daemon nudges its
// peers the moment an act lands (see `ring_write_nudge`), so this poll is what
// turns an arrived act into rendered text, not what fetches it.
const POLL_MS = 500;
// Presence moves on the LIVE lane, which writes nothing, so it can move far
// faster than the journal: a cursor that lags a third of a second reads as a
// broken cursor. 100 ms out, 250 ms back — inside y-protocols' 30 s
// `outdatedTimeout` by two orders of magnitude, which is the margin a peer
// needs to survive a few dropped posts without fading off the other screens.
const PRESENCE_THROTTLE_MS = 100;
const LIVE_POLL_MS = 250;

const ydoc = new Y.Doc();
// Presence for this document. Constructed here and not inside the editor
// because the live lane, not Tiptap, is what carries it — the caret extension
// is a renderer over this map.
const awareness = new awarenessProtocol.Awareness(ydoc);
// Op ids this document has already absorbed. Owned here, across polls —
// `applyNew` reads and extends it.
const applied = new Set();

let pending = [];
let timer = null;

// The roster the last poll read (`person → [keys]`), and this node's own
// signing key once the rail has told us what it is. The key is NOT in the log
// response — every op carries an actor and none of them is marked "yours" —
// so it arrives on the first successful append, which is the one moment the
// rail says who signed. Until then this node has no name, and `personFor`
// returns `null` rather than something invented (ARCH §6).
let rosterMembers = {};
let myActor = null;
// Gaps from the LIVE lane, held across polls because `poll()` owns the panel
// and runs on a different clock.
let liveGaps = [];
let presenceTimer = null;

/// Send everything typed in the last window as ONE act.
///
/// `Y.mergeUpdates` is why this is one act and not N: Yjs updates compose, so
/// nine keystrokes merge to 27 bytes rather than nine signed lines.
async function flush() {
  timer = null;
  const batch = pending;
  pending = [];
  if (batch.length === 0) return;
  try {
    const written = await window.ring.record(changeAct(Y.mergeUpdates(batch), DOC_ID));
    // The one place the rail says which key THIS daemon signs with. Keep it
    // and name the cursor from it.
    if (written && written.actor && written.actor !== myActor) {
      myActor = written.actor;
      nameSelf();
    }
    el("err").textContent = "";
  } catch (e) {
    // Put the batch back at the front. Dropping it silently would leave the
    // typist's text on screen and absent from every other node — the one
    // failure this stack exists to avoid.
    pending = batch.concat(pending);
    el("err").textContent = `not recorded: ${String(e.message || e)}`;
  }
}

ydoc.on("update", (update, origin) => {
  // An update we just applied FROM the rail must not go back to it. Without
  // this test the two nodes append to the journal forever over a document that
  // has stopped changing.
  if (origin === FROM_RAIL) return;
  pending.push(update);
  if (timer === null) timer = setTimeout(flush, DEBOUNCE_MS);
});

/// Put the roster's name for this node's key on this node's cursor.
///
/// **The name is never typed.** It is the person the ring's roster binds to
/// the key the rail watched sign, so a cursor cannot claim to be somebody it
/// is not on the machine that draws it. What this does NOT do is make the name
/// checkable by the peer that receives it — awareness is unauthenticated, and
/// the attribution a reader can verify is the act-level one under the text.
function nameSelf() {
  const name = personFor(rosterMembers, myActor);
  if (name === null) return;
  awareness.setLocalStateField("user", { name, color: colorFor(name) });
}

/// Send THIS node's presence to every online peer, now.
async function sendPresence() {
  presenceTimer = null;
  try {
    await window.ring.live.send(presenceEnvelope(encodeSelf(awareness), DOC_ID));
    liveGaps = [];
  } catch (e) {
    // A cursor nobody can see is worth saying out loud once. Held as a gap
    // rather than rethrown: the document still converges with the live lane
    // down, and a page that stopped rendering text because presence failed
    // would be trading the durable half for the ephemeral one.
    liveGaps = [`presence not delivered: ${String(e.message || e)}`];
  }
}

awareness.on("update", (_changes, origin) => {
  // A peer's presence we just applied must not go back out. Without this the
  // three nodes rebroadcast each other's cursors forever, and — worse — every
  // rebroadcast refreshes `lastUpdated`, so a node that left never fades.
  if (origin === FROM_LIVE) return;
  if (presenceTimer === null) {
    presenceTimer = setTimeout(sendPresence, PRESENCE_THROTTLE_MS);
  }
});

/// Drain what peers pushed and put it on the awareness map.
///
/// A drain, not a read: the daemon hands each payload out once, which is why
/// one page per daemon owns this poll.
async function pollLive() {
  let body;
  try {
    body = await window.ring.live.drain();
  } catch (e) {
    liveGaps = [`presence not read: ${String(e.message || e)}`];
    return;
  }
  const read = decodePresence(body.payloads || [], DOC_ID);
  applyPresence(awareness, read.updates);
  // `dropped` is the daemon saying its buffer overflowed. An absent cursor and
  // a cursor nobody sent look identical on screen, so the loss is named.
  liveGaps = body.dropped
    ? [...read.gaps, `${body.dropped} presence update(s) were dropped by the daemon's buffer`]
    : read.gaps;
}

async function poll() {
  let log;
  try {
    log = await window.ring.log();
  } catch (e) {
    el("err").textContent = String(e.message || e);
    return;
  }

  // The whole fold, in one call. `ring.fold` walks the acts in the rail's
  // order and skips the ones a correction voided — do not iterate `log.ops`
  // yourself, or a voided update gets applied and Yjs has no un-apply.
  // `.members`, not the whole roster — the rail ships the `Roster` value and
  // the name lookup wants the person→keys map inside it.
  rosterMembers = (log.roster || {}).members || {};
  nameSelf();

  const read = decodeActs(log, window.ring.fold, DOC_ID);
  applyNew(ydoc, read.acts, applied);

  el("scope").textContent =
    `${window.ring.namespace} — ${applied.size} change${applied.size === 1 ? "" : "s"}`;

  // **Two kinds of gap, and both must be shown.**
  //
  // `log.gaps` come from the RAIL: an act that has not reached this node, a
  // signature that does not verify. `read.gaps` come from THIS app: an act
  // whose update could not be read.
  //
  // Either one means the text on screen is a subset of what was written.
  // Never hide the panel to make the page look tidier.
  //
  // `liveGaps` is the third: the live lane refused, or the daemon's buffer
  // overflowed. A missing cursor is not a missing sentence, but it is still
  // something the page knows and the person does not.
  const gaps = [
    ...(log.gaps || []).map((g) => g.message || g.gap),
    ...read.gaps,
    ...liveGaps,
  ];
  el("gaps").hidden = gaps.length === 0;
  el("gaplist").innerHTML = gaps.map((g) => `<li>${g}</li>`).join("");
}

new Editor({
  element: el("editor"),
  extensions: [
    // Yjs owns undo: the built-in history stack would undo a peer's paragraph
    // on a local ctrl-Z, because it has no idea the document has other authors.
    StarterKit.configure({ undoRedo: false }),
    Collaboration.configure({ document: ydoc }),
    // A renderer over the awareness map, nothing more — it takes a `provider`
    // only to read `.awareness` off it, so the live lane above is what it
    // draws. The user starts nameless on purpose: a placeholder would be the
    // page inventing attribution, and the roster answers within one append.
    CollaborationCaret.configure({ provider: { awareness } }),
  ],
});

poll();
setInterval(poll, POLL_MS);
pollLive();
setInterval(pollLive, LIVE_POLL_MS);
