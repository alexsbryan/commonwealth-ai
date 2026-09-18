// The doc rules, pinned. `node --test .`
//
// Each name says which mistake loses text. The two that matter most are the
// two the CRDT is supposed to make impossible, and "supposed to" is the word
// that earns a test: the adapter can still lose convergence by deciding which
// acts to apply, which is the decision this file exercises.
//
// What is NOT tested here is that every node SEES the same acts — that is the
// rail's property, proved over all 720 orderings of a six-op fixture in
// `rail/tests.rs`. The adapter cannot break it and does not re-establish it.

import assert from "node:assert/strict";
import { test } from "node:test";
import { Y, awarenessProtocol } from "./vendor/ring-doc-bundle.js";
import {
  DOC_CHANGE,
  DOC_ID,
  PROSE_FRAGMENT,
  applyNew,
  applyPresence,
  b64ToBytes,
  bytesToB64,
  changeAct,
  colorFor,
  createAttribution,
  decodeActs,
  decodePresence,
  encodeSelf,
  personFor,
  presenceEnvelope,
} from "./adapter.js";

// The SDK's `fold` is served by `window.ring`, which only exists in the page.
// This is a TEST DOUBLE of its traversal, not a second implementation: the
// real one lives in `sovereign-cli-llm/src/ring_cmd/dev.rs` (DEV_SHIM) and its
// voided-skipping is pinned there by
// `the_shim_ships_a_fold_that_skips_voided_and_empty_acts`.
const fold = (log, reducer, initial) => {
  let acc = initial;
  for (const op of (log && log.ops) || []) {
    if (op.voided || op.payload == null) continue;
    acc = reducer(acc, op.payload, op);
  }
  return acc;
};

/// One op as the rail serializes it (`AdmittedOp` in
/// commonwealth-rail-core/src/admit.rs).
const op = (id, payload, extra = {}) => ({
  id,
  actor: "k1",
  person: "alex",
  seq: 1,
  ts_unix: 1_756_512_000,
  voided: false,
  payload,
  ...extra,
});

/// Type `text` into a fresh doc and return the update that says so.
const typed = (text) => {
  const doc = new Y.Doc();
  doc.getText("prose").insert(0, text);
  return Y.encodeStateAsUpdate(doc);
};

const readBack = (doc) => doc.getText("prose").toString();

test("the same acts in two orders reach the same document", () => {
  // Two nodes typed independently, so neither update depends on the other.
  // The rail hands every node ONE order — but a node that joins late, or that
  // reconnects after a partition, reads the journal at a different moment and
  // must still land in the same place.
  const a = changeAct(typed("alpha"));
  const b = changeAct(typed("beta"));

  const forward = new Y.Doc();
  applyNew(forward, decodeActs({ ops: [op("ring-a", a), op("ring-b", b)] }, fold).acts, new Set());

  const reverse = new Y.Doc();
  applyNew(reverse, decodeActs({ ops: [op("ring-b", b), op("ring-a", a)] }, fold).acts, new Set());

  assert.deepEqual(
    Array.from(Y.encodeStateVector(forward)),
    Array.from(Y.encodeStateVector(reverse)),
    "two readings of the same journal disagree about what the document is",
  );
  assert.equal(readBack(forward), readBack(reverse));
});

test("an act already applied is not applied a second time", () => {
  // The poll re-reads the WHOLE journal every 500 ms, so every act arrives
  // again on every tick. Yjs would absorb the repeat, but the cost of
  // re-applying the whole history each tick lands on the person typing.
  const act = changeAct(typed("once"));
  const log = { ops: [op("ring-a", act)] };
  const doc = new Y.Doc();
  const applied = new Set();

  const first = applyNew(doc, decodeActs(log, fold).acts, applied);
  const second = applyNew(doc, decodeActs(log, fold).acts, applied);

  assert.equal(first.length, 1);
  assert.equal(second.length, 0, "the same op id was applied twice");
  assert.equal(readBack(doc), "once");
});

test("the reader takes only this document's changes, and says what it refused", () => {
  // A ring holds whatever its members write. An adapter that fed every act to
  // `Y.applyUpdate` would throw on the first expense someone recorded in the
  // same ring — and a malformed update has to surface as a gap, because the
  // honest reading of an act we cannot apply is "this text covers a subset".
  const read = decodeActs(
    {
      ops: [
        op("ring-a", changeAct(typed("mine"))),
        op("ring-b", { kind: "expense", amount_cents: 6000 }),
        op("ring-c", changeAct(typed("other"), "other-doc")),
        op("ring-d", { kind: DOC_CHANGE, doc: DOC_ID, update: 42 }),
        op("ring-e", changeAct(typed("voided")), { voided: true }),
      ],
    },
    fold,
  );

  assert.deepEqual(read.acts.map((a) => a.id), ["ring-a"]);
  assert.equal(read.gaps.length, 1, `${read.gaps}`);
  assert.match(read.gaps[0], /ring-d/);
});

test("an update survives the base64 trip whole", () => {
  // The chunked encoder exists because spreading a document-sized array onto
  // the call stack throws. A stride bug would corrupt exactly one boundary,
  // which is invisible on the short updates every other test here uses.
  const bytes = new Uint8Array(0x8000 * 2 + 7);
  for (let i = 0; i < bytes.length; i += 1) bytes[i] = i % 256;
  assert.deepEqual(Array.from(b64ToBytes(bytesToB64(bytes))), Array.from(bytes));
});

// --- presence ------------------------------------------------------------
//
// The live lane's half. What is pinned here is what the adapter DECIDES: the
// name on a cursor comes from the roster, and a node that stopped sending is
// gone rather than frozen on screen. The transport (fan-out, the bounded
// buffer, the restart that empties it) is the daemon's and is pinned in
// `sovereign-mesh/tests/main/ring_live_non_durable.rs` — a test here that
// re-asserted it would be asserting a mock.

/// The roster as `log.roster.members` ships it: person → the keys they sign
/// with. Alex has two laptops, which is why the lookup is over the values.
const MEMBERS = {
  alex: ["aa".repeat(32), "cc".repeat(32)],
  bo: ["bb".repeat(32)],
};

test("a cursor's name is the roster's, for the key the rail watched sign", () => {
  // Both of one person's keys answer with that person; a key the ring never
  // admitted answers `null`, which the page renders as a nameless cursor
  // rather than inventing one.
  assert.equal(personFor(MEMBERS, "aa".repeat(32)), "alex");
  assert.equal(personFor(MEMBERS, "cc".repeat(32)), "alex");
  assert.equal(personFor(MEMBERS, "bb".repeat(32)), "bo");
  assert.equal(personFor(MEMBERS, "ff".repeat(32)), null);
  assert.equal(personFor(MEMBERS, null), null);
});

test("a caret colour is a hex triple, or Tiptap draws nothing", () => {
  // `isValidColor` in the caret extension is `/^#[0-9a-fA-F]{6}$/` and
  // substitutes `transparent` for anything else — silently. An `hsl()` string
  // here is an invisible cursor with no error on any surface.
  assert.match(colorFor("alex"), /^#[0-9a-f]{6}$/);
  assert.equal(colorFor("alex"), colorFor("alex"), "one person, two colours");
  assert.notEqual(colorFor("alex"), colorFor("bo"));
});

test("an awareness roundtrip carries the roster name", async () => {
  // A's presence, over the live lane's envelope, onto B's map. The name B
  // reads is the one A looked up in the roster for the key the rail told it
  // it signs with — never anything typed into the page.
  const a = new awarenessProtocol.Awareness(new Y.Doc());
  const b = new awarenessProtocol.Awareness(new Y.Doc());
  try {
    const name = personFor(MEMBERS, "aa".repeat(32));
    a.setLocalStateField("user", { name, color: colorFor(name) });

    const wire = presenceEnvelope(encodeSelf(a), DOC_ID);
    const read = decodePresence([wire], DOC_ID);
    assert.deepEqual(read.gaps, []);
    applyPresence(b, read.updates);

    assert.equal(b.getStates().get(a.clientID).user.name, "alex");
    assert.equal(b.getStates().get(a.clientID).user.color, colorFor("alex"));
  } finally {
    a.destroy();
    b.destroy();
  }
});

test("the live lane carries other things and presence skips them", () => {
  // One buffer per daemon: a payload for another document, or another kind,
  // is not ours and is not a gap. A payload that CLAIMS to be ours and cannot
  // be read is — an absent cursor and a peer who is not typing look the same.
  const a = new awarenessProtocol.Awareness(new Y.Doc());
  try {
    a.setLocalStateField("user", { name: "alex", color: colorFor("alex") });
    const read = decodePresence(
      [
        presenceEnvelope(encodeSelf(a), DOC_ID),
        presenceEnvelope(encodeSelf(a), "other-doc"),
        JSON.stringify({ kind: "something-else", doc: DOC_ID }),
        "{not json",
        JSON.stringify({ kind: "awareness", doc: DOC_ID, awareness: "%%%%" }),
      ],
      DOC_ID,
    );
    assert.equal(read.updates.length, 1);
    assert.equal(read.gaps.length, 2, `${read.gaps}`);
  } finally {
    a.destroy();
  }
});

test("a node absent for the outdated timeout is gone from the awareness map", async () => {
  // y-protocols owns this — `outdatedTimeout` is 30 s and its sweep runs every
  // 3 s — and the reason it is exercised here rather than trusted is the
  // echo bug it guards: a node that rebroadcast the presence it RECEIVED
  // would refresh `lastUpdated` on every tick, and a peer that pulled its
  // network would stay on screen forever. `app.js` skips `FROM_LIVE` origins
  // for that reason, and this is the property that would tell us it stopped.
  const a = new awarenessProtocol.Awareness(new Y.Doc());
  const b = new awarenessProtocol.Awareness(new Y.Doc());
  try {
    a.setLocalStateField("user", { name: "alex", color: colorFor("alex") });
    applyPresence(b, decodePresence([presenceEnvelope(encodeSelf(a), DOC_ID)], DOC_ID).updates);
    assert.ok(b.getStates().has(a.clientID), "the peer never arrived");

    // A stopped, 31 s ago. Back-dating the metadata is how that is said
    // without sleeping through it; the sweep itself is the library's.
    b.meta.get(a.clientID).lastUpdated =
      Date.now() - awarenessProtocol.outdatedTimeout - 1000;

    const deadline = Date.now() + 8000;
    while (b.getStates().has(a.clientID) && Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 50));
    }
    assert.ok(
      !b.getStates().has(a.clientID),
      "a peer that stopped sending is still on screen after its timeout",
    );
  } finally {
    a.destroy();
    b.destroy();
  }
});

// --- attribution -----------------------------------------------------------
//
// The claim is `ra-doc-attribution-from-signer`: every "last edited by" line
// resolves from the act's `actor` through the roster. The test that earns it
// is the second one — the first would read the same if the adapter took the
// name from inside the update, which is the mistake this whole path avoids.

/// Paragraphs in the fragment Tiptap edits, as ONE update.
const typedParagraphs = (texts) => {
  const doc = new Y.Doc();
  const frag = doc.getXmlFragment(PROSE_FRAGMENT);
  for (const text of texts) {
    const para = new Y.XmlElement("paragraph");
    const body = new Y.XmlText();
    body.insert(0, text);
    para.insert(0, [body]);
    frag.push([para]);
  }
  return Y.encodeStateAsUpdate(doc);
};

/// A further update over `base`, optionally announcing `clientID` as the
/// client that wrote it. Yjs lets a client say who it is — `doc.clientID` is
/// one assignment — which is exactly the forgery the rail's signature is
/// there to make irrelevant.
const edit = (base, clientID, change) => {
  const doc = new Y.Doc();
  Y.applyUpdate(doc, base);
  if (clientID !== undefined) doc.clientID = clientID;
  const before = Y.encodeStateVector(doc);
  change(doc.getXmlFragment(PROSE_FRAGMENT));
  return Y.encodeStateAsUpdate(doc, before);
};

const NOW_SEC = 1_756_512_000;
const ALEX_KEY = MEMBERS.alex[0];
const BO_KEY = MEMBERS.bo[0];

test("each paragraph says who last edited it, and how long ago", () => {
  const first = typedParagraphs(["one", "two"]);
  const second = edit(first, undefined, (frag) => frag.get(1).get(0).insert(3, " more"));

  const attribution = createAttribution();
  attribution.absorb(
    decodeActs(
      {
        ops: [
          op("ring-a", changeAct(first), { actor: ALEX_KEY, ts_unix: NOW_SEC - 12 }),
          op("ring-b", changeAct(second), { actor: BO_KEY, ts_unix: NOW_SEC - 4 }),
        ],
      },
      fold,
    ).acts,
  );

  assert.deepEqual(attribution.lines(MEMBERS, NOW_SEC * 1000), [
    "last edited by alex 12s ago",
    "last edited by bo 4s ago",
  ]);
});

test("an update whose clientID is another node's still attributes to the signer", () => {
  // The negative leg of the bar. `bo`'s page announces alex's Yjs clientID —
  // one number, changed in a page anybody can edit, and Yjs has no way to
  // refuse it. The act carrying it is still signed by bo's key, which is the
  // field admission verified, so the paragraph must read bo.
  const alexDoc = new Y.Doc();
  alexDoc.clientID = 1001;
  const alexFrag = alexDoc.getXmlFragment(PROSE_FRAGMENT);
  const para = new Y.XmlElement("paragraph");
  const body = new Y.XmlText();
  body.insert(0, "alex wrote this");
  para.insert(0, [body]);
  alexFrag.push([para]);
  const honest = Y.encodeStateAsUpdate(alexDoc);

  const forged = edit(honest, 1001, (frag) => frag.get(0).get(0).insert(0, "bo typed here. "));
  // The forgery, checked rather than assumed: every struct in bo's update
  // announces alex's client. A reader that took the name from in here would
  // say alex, which is the failure the line below rules out.
  assert.deepEqual(
    [...new Set(Y.decodeUpdate(forged).structs.map((s) => s.id.client))],
    [1001],
    "the update under test does not actually carry alex's clientID",
  );

  const attribution = createAttribution();
  attribution.absorb(
    decodeActs(
      {
        ops: [
          op("ring-a", changeAct(honest), { actor: ALEX_KEY, ts_unix: NOW_SEC - 30 }),
          op("ring-b", changeAct(forged), { actor: BO_KEY, ts_unix: NOW_SEC - 1 }),
        ],
      },
      fold,
    ).acts,
  );

  assert.deepEqual(attribution.lines(MEMBERS, NOW_SEC * 1000), [
    "last edited by bo 1s ago",
  ]);
});

test("a key the roster does not name is said to be unnamed, never invented", () => {
  // A member admitted since this node's last log read signs acts it cannot yet
  // name. The honest line says so; a fallback to the key, or to "someone",
  // would be a page substituting for an absence (ARCH §6).
  const attribution = createAttribution();
  attribution.absorb(
    decodeActs(
      { ops: [op("ring-a", changeAct(typedParagraphs(["hello"])), { actor: "dd".repeat(32) })] },
      fold,
    ).acts,
  );

  const [line] = attribution.lines(MEMBERS, NOW_SEC * 1000);
  assert.match(line, /does not name/);
  assert.doesNotMatch(line, /alex|bo|dddd/);
});
