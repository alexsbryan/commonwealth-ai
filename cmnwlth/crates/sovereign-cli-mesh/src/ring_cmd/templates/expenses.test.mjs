// The money rules, pinned. `node --test .`
//
// These tests used to be in Rust, inside the rail, back when the rail's
// journal line WAS an expense. They came here with the arithmetic. Each name
// says which mistake loses money, because a test called `test_fold_2` teaches
// the next reader nothing about why the rule exists.
//
// What is NOT tested here is convergence: that every node sees the same acts
// in the same order is the rail's property, proved exhaustively over all 720
// orderings of a six-op fixture in `rail/tests.rs`. A reducer cannot break it
// and does not have to re-establish it.

import assert from "node:assert/strict";
import { test } from "node:test";
import {
  EXPENSE,
  SETTLE,
  describe as describeAct,
  initial,
  money,
  reducer,
  settleKey,
  splitShares,
  today,
  validate,
  writable,
} from "./expenses.js";

const RING = { alex: ["k1"], bo: ["k2"], cy: ["k3"] };
const ALL = ["alex", "bo", "cy"];

const expense = (payer, cents, what, participants) => ({
  kind: EXPENSE,
  payer,
  amount_cents: cents,
  description: what,
  participants,
});

const settle = (from, to, cents, day) => ({
  kind: SETTLE,
  from,
  to,
  amount_cents: cents,
  key: settleKey(from, to, cents, day),
});

/** Fold a list of acts the way `window.ring.fold` would. */
function fold(acts, roster = RING) {
  return acts.reduce((s, p, i) => reducer(s, p, { id: `ring-${i}` }), initial(roster));
}

const cents = (state, who) => state.balances[who] || 0;
const sum = (state) => Object.values(state.balances).reduce((a, b) => a + b, 0);

// ── the split ────────────────────────────────────────────────

test("ten dollars three ways keeps the penny and gives it to the first name", () => {
  assert.deepEqual(splitShares(1000, ALL), [
    ["alex", 334],
    ["bo", 333],
    ["cy", 333],
  ]);
});

test("shares always sum to the amount", () => {
  const people = ["a", "b", "c", "d", "e", "f", "g"];
  for (let n = 1; n <= people.length; n++) {
    for (const amount of [1, 2, 3, 99, 100, 1000, 1001, 123457]) {
      const shares = splitShares(amount, people.slice(0, n));
      const total = shares.reduce((a, [, c]) => a + c, 0);
      assert.equal(total, amount, `${amount} split ${n} ways`);
    }
  }
});

test("the split does not depend on the order the participants were listed in", () => {
  assert.deepEqual(splitShares(1000, ["cy", "alex", "bo"]), splitShares(1000, ALL));
});

// ── the book ─────────────────────────────────────────────────

test("every fold sums to zero", () => {
  const state = fold([
    expense("alex", 6000, "groceries", ALL),
    expense("bo", 1000, "beer", ALL),
    settle("cy", "alex", 2000, "2026-08-30"),
    expense("cy", 777, "odd amount", ["alex", "cy"]),
  ]);
  assert.equal(sum(state), 0);
  assert.equal(state.gaps.length, 0, JSON.stringify(state.gaps));
});

test("a settlement both parties recorded is counted once", () => {
  const day = "2026-08-30";
  // Bo records it, and so does Alex — same four facts, so the same key.
  const both = fold([settle("bo", "alex", 2000, day), settle("bo", "alex", 2000, day)]);
  assert.equal(cents(both, "bo"), 2000);
  assert.equal(cents(both, "alex"), -2000);
  assert.equal(both.counted, 1, "the debt must not be paid twice");
});

test("a whole month settles to zero", () => {
  const state = fold([
    expense("alex", 12000, "groceries", ALL),
    expense("bo", 6000, "internet", ALL),
    expense("cy", 3000, "cleaning", ALL),
  ]);
  assert.equal(sum(state), 0);
  assert.equal(cents(state, "alex"), 12000 - 7000);
  assert.equal(cents(state, "bo"), 6000 - 7000);
  assert.equal(cents(state, "cy"), 3000 - 7000);
});

/**
 * **The reason `participants` is an explicit list.** If a split read the
 * roster, moving a housemate in would silently re-divide every expense
 * already in the book — including ones recorded before they arrived.
 */
test("adding a housemate does not re-divide past expenses", () => {
  const acts = [expense("alex", 900, "groceries", ALL)];
  const small = fold(acts, RING);
  const bigger = fold(acts, { ...RING, dee: ["k4"] });
  assert.deepEqual(small.balances, bigger.balances);
  assert.equal(cents(small, "alex"), 900 - 300);
});

// ── refusals ─────────────────────────────────────────────────

test("an expense with no participants is refused rather than divided by zero", () => {
  const state = fold([expense("alex", 1000, "x", [])]);
  assert.deepEqual(state.balances, {});
  assert.equal(state.gaps[0].code, "no_participants");
});

test("a non-positive amount is refused", () => {
  for (const bad of [0, -1]) {
    const state = fold([expense("alex", bad, "free", ALL)]);
    assert.deepEqual(state.balances, {});
    assert.equal(state.gaps[0].code, "non_positive_amount");
  }
});

test("paying yourself back is refused", () => {
  const state = fold([settle("alex", "alex", 100, "2026-08-30")]);
  assert.deepEqual(state.balances, {});
  assert.equal(state.gaps[0].code, "self_settle");
});

/**
 * Charged once, because that is what the writer meant — and reported, because
 * silently collapsing it changes what they asked for without telling them.
 */
test("a participant listed twice is charged once and reported", () => {
  const state = fold([expense("alex", 1000, "x", ["alex", "bo", "bo"])]);
  assert.equal(cents(state, "bo"), -500);
  assert.equal(sum(state), 0);
  assert.equal(state.gaps[0].code, "duplicate_participant");
});

/**
 * The money moved and the balances must still sum to zero, so this is folded
 * — but a balance held against a name nobody can pay needs saying out loud.
 */
test("a name the roster does not know is folded and flagged", () => {
  const state = fold([expense("alex", 1000, "x", ["alex", "ghost"])]);
  assert.equal(cents(state, "ghost"), -500);
  assert.equal(sum(state), 0);
  assert.equal(state.gaps[0].code, "unknown_person");
  assert.equal(state.counted, 1, "not fatal — the money really did move");
});

test("an act from a newer version of this app is reported, not misread", () => {
  const state = fold([{ kind: "tip-jar", amount_cents: 500 }]);
  assert.deepEqual(state.balances, {});
  assert.equal(state.gaps[0].code, "unknown_kind");
});

/**
 * **One validator, two callers.** The door refuses exactly what the reducer
 * would refuse — the property the rail used to hold and handed up with the
 * arithmetic. Two copies of this judgement is how a book fills with entries
 * that render as gaps forever.
 */
test("the door refuses exactly what the reducer refuses", () => {
  const cases = [
    expense("alex", 0, "free", ALL),
    expense("alex", 100, "nobody", []),
    settle("alex", "alex", 100, "2026-08-30"),
    { kind: "tip-jar", amount_cents: 500 },
  ];
  for (const bad of cases) {
    assert.equal(writable(bad, RING), false, JSON.stringify(bad));
    assert.deepEqual(fold([bad]).balances, {}, JSON.stringify(bad));
  }
  // And a good one passes both.
  const good = expense("alex", 100, "milk", ALL);
  assert.equal(writable(good, RING), true);
  assert.equal(fold([good]).counted, 1);
});

/** Not fatal is not the same as not reported. */
test("a flagged-but-applied act is still writable", () => {
  assert.equal(writable(expense("alex", 100, "x", ["alex", "ghost"]), RING), true);
  assert.equal(validate(expense("alex", 100, "x", ["alex", "ghost"]), RING).length, 1);
});

// ── the bits a person reads ──────────────────────────────────

test("money renders the way a person reads it", () => {
  assert.equal(money(0), "$0.00");
  assert.equal(money(5), "$0.05");
  assert.equal(money(1234), "$12.34");
  assert.equal(money(-1234), "-$12.34");
  assert.equal(money(100000), "$1000.00");
});

test("a correction that states nothing reads as a withdrawal", () => {
  assert.match(describeAct(null), /withdrew/);
  assert.match(describeAct(expense("alex", 2450, "milk", ALL)), /alex paid \$24\.50/);
  assert.match(describeAct(settle("bo", "alex", 2000, "2026-08-30")), /bo paid alex/);
});

/**
 * The shape mistake that produced correct balances beside a page of spurious
 * "not in the roster" gaps on a live daemon. The rail ships the whole
 * `Roster`; the reducer wants the map inside it.
 */
test("passing the whole roster object instead of its members is refused", () => {
  assert.throws(() => initial({ members: RING }), /log\.roster\.members/);
  assert.doesNotThrow(() => initial(RING));
});

/**
 * Before anyone is added, "we know nobody" is the honest answer — a book that
 * went quiet on an empty roster would hide the one thing a new housemate has
 * to do, while every other node reported their ops as unknown-signer gaps.
 */
test("an empty roster flags every name rather than going quiet", () => {
  const state = fold([expense("alex", 100, "x", ["alex", "bo"])], {});
  assert.ok(state.gaps.every((g) => g.code === "unknown_person"));
  assert.ok(state.gaps.length > 0);
  assert.equal(state.counted, 1, "still not fatal — the money moved");
});

test("the settlement key is a calendar day, so both parties derive it", () => {
  assert.equal(today(new Date("2026-08-30T23:59:00Z")), "2026-08-30");
  assert.equal(
    settleKey("bo", "alex", 2000, "2026-08-30"),
    '{"from":"bo","to":"alex","cents":2000,"day":"2026-08-30"}',
  );
});
