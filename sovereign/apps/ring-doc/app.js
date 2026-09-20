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
  citationLines,
  colorFor,
  createAttribution,
  decodeActs,
  decodePresence,
  encodeSelf,
  personFor,
  presenceEnvelope,
} from "./adapter.js";

const el = (id) => document.getElementById(id);

// Guest mode: the page was opened from the wall grant's link, whose bearer
// rides the fragment (`guest_door::ring_shim`). The name is the DOOR's to ask
// and to carry, not this page's — the shim holds it and the rail stamps it
// into `op.person`, so nothing here knows a guest from a member. Wall mode
// (`?wall`) is the member's screen: the roster and the grant's QR, which
// `svrn mesh grant --qr-svg` writes as `wall-qr.svg` beside this page.
const GUEST = new URLSearchParams(location.hash.slice(1)).has("token");
const WALL = new URLSearchParams(location.search).has("wall");

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
// Who last edited each paragraph. Owned here across polls for the same reason
// `applied` is: it replays each act once, not once per tick.
const attribution = createAttribution();

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
// Peers the last presence send did not reach. `sendPresence` owns it alone, so
// a drain every 250 ms cannot clear what a send found.
let deliveryGaps = [];
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
    const sent = await window.ring.live.send(presenceEnvelope(encodeSelf(awareness), DOC_ID));
    deliveryGaps = ((sent && sent.peers) || [])
      .filter((p) => !p.delivered)
      .map((p) => `presence not delivered to ${p.name || p.node}: ${p.error}`);
  } catch (e) {
    // A cursor nobody can see is worth saying out loud once. Held as a gap
    // rather than rethrown: the document still converges with the live lane
    // down, and a page that stopped rendering text because presence failed
    // would be trading the durable half for the ephemeral one.
    deliveryGaps = [`presence not delivered: ${String(e.message || e)}`];
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

/// Put each paragraph's line beside the paragraph it is about.
///
/// The notes live in a gutter OUTSIDE the editable element and are positioned
/// against it, rather than written into the paragraphs: text inside the
/// contenteditable belongs to ProseMirror, and a page that put its own nodes
/// there would have them redrawn away — or worse, folded into the document and
/// appended to the rail as somebody's writing.
///
/// A paragraph the replay has no act for (the line is `null`) gets no note.
/// That is the paragraph the typist is making right now, before its act has
/// been read back: saying nothing for a third of a second is honest, and a
/// placeholder name would not be.
function renderAttribution(lines) {
  const prose = el("editor").querySelector(".tiptap");
  const gutter = el("attribution");
  if (!prose) return;
  const blocks = Array.from(prose.children);
  gutter.replaceChildren();
  lines.forEach((line, i) => {
    const block = blocks[i];
    if (line === null || block === undefined) return;
    const note = document.createElement("div");
    note.className = "note";
    note.style.top = `${block.offsetTop}px`;
    note.textContent = line;
    gutter.appendChild(note);
  });
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
  if (WALL) el("roster").textContent = Object.keys(rosterMembers).join(", ");

  const read = decodeActs(log, window.ring.fold, DOC_ID);
  applyNew(ydoc, read.acts, applied);
  attribution.absorb(read.acts);
  renderAttribution(attribution.lines(Date.now()));

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
  //
  // `deliveryGaps` is the fourth: a peer this node's last presence send did
  // not reach, by name. It is how A and B say C is gone while C is gone.
  const gaps = [
    ...(log.gaps || []).map((g) => g.message || g.gap),
    ...read.gaps,
    ...liveGaps,
    ...deliveryGaps,
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

// The ask. A guest has a bearer in the fragment, so the shim can reach the
// door's route; a member reading this page under `ring dev` has no grant and
// the shim refuses by name, which is why the box is guest-only.
if (GUEST) {
  el("ask").hidden = false;
  el("ask-go").addEventListener("click", async () => {
    const question = el("ask-q").value.trim();
    if (!question) return;
    el("ask-go").disabled = true;
    el("ask-a").textContent = "asking the room…";
    el("ask-sources").replaceChildren();
    try {
      const { answer, epistemic_state } = await window.ring.ask(question);
      el("ask-a").textContent = answer || "";
      // Each source names the corpus and the mesh member that held it — the
      // whole point of asking a room rather than a box. A turn with no
      // citations shows none, and says so rather than showing an unsourced
      // answer as if it were sourced.
      const lines = citationLines(epistemic_state);
      el("ask-sources").replaceChildren(
        ...(lines.length
          ? lines.map((line) => {
              const li = document.createElement("li");
              li.textContent = line;
              return li;
            })
          : [Object.assign(document.createElement("li"), { textContent: "no sources cited" })]),
      );
    } catch (e) {
      el("ask-a").textContent = "";
      el("err").textContent = String(e.message || e);
    } finally {
      el("ask-go").disabled = false;
    }
  });
}
if (WALL) {
  el("wall").hidden = false;
  // An absent QR is said, not left as a broken image.
  el("wall-qr").addEventListener("error", () => {
    el("wall-qr").hidden = true;
    el("err").textContent = "no wall-qr.svg beside this page: svrn mesh grant --qr-svg writes it";
  });
}

poll();
setInterval(poll, POLL_MS);
pollLive();
setInterval(pollLive, LIVE_POLL_MS);
