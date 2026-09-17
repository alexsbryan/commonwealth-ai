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
  Editor,
  StarterKit,
  Y,
} from "./vendor/ring-doc-bundle.js";
import {
  DOC_ID,
  FROM_RAIL,
  applyNew,
  changeAct,
  decodeActs,
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

const ydoc = new Y.Doc();
// Op ids this document has already absorbed. Owned here, across polls —
// `applyNew` reads and extends it.
const applied = new Set();

let pending = [];
let timer = null;

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
    await window.ring.record(changeAct(Y.mergeUpdates(batch), DOC_ID));
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
  const gaps = [
    ...(log.gaps || []).map((g) => g.message || g.gap),
    ...read.gaps,
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
  ],
});

poll();
setInterval(poll, POLL_MS);
