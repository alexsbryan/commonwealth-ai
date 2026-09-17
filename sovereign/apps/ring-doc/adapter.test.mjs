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
import { Y } from "./vendor/ring-doc-bundle.js";
import {
  DOC_CHANGE,
  DOC_ID,
  applyNew,
  b64ToBytes,
  bytesToB64,
  changeAct,
  decodeActs,
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
