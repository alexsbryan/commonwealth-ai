// The money rules. This file is the app, and the rail below it has never
// heard of an expense.
//
// The rail (`window.ring`) guarantees exactly four things about the acts it
// hands you, and no more:
//
//   1. every act was signed by a key this ring's roster claims,
//   2. duplicates are gone and equivocated ops are excluded,
//   3. corrections have been applied — voided acts are marked, and a
//      correction never resurrects what an earlier one voided,
//   4. the order is the same on every node in the ring.
//
// So a reducer over `ring.fold` cannot disagree with a housemate's laptop
// about which acts it saw or in what order. What it CAN get wrong is the
// arithmetic, which is why the arithmetic is here, in one file, with tests.
//
// ── the two rules worth reading before you change anything ──
//
// **The remainder goes to whoever sorts first.** $10.00 three ways is
// 334/333/333, never 333/333/333 with a penny evaporating and never
// 3.333… rounded per node. The rule is arbitrary; what matters is that it is
// computable from the act alone, so every node lands on the same cents with
// no coordination.
//
// **`participants` is an explicit list and the roster is never consulted for
// it.** The moment a split reads "everyone in the ring", the same acts divide
// differently on a node whose roster has one more person on it — and adding a
// housemate silently re-divides every expense in the history. There is a test
// named after that.

/** Every payload this app writes carries a `kind`. */
export const EXPENSE = "expense";
export const SETTLE = "settle";

/**
 * The one derivation of a settlement's idempotency key.
 *
 * Both parties to the same payment compute this from the same four facts and
 * land on the same string, so the second copy to arrive collapses into the
 * first rather than paying the debt twice. JSON rather than a joined string,
 * so a name containing the delimiter cannot make two different settlements
 * share a key.
 *
 * `day` is a calendar day rather than a timestamp because the two parties
 * record the same payment minutes or hours apart. The accepted cost is that
 * two genuinely separate identical payments between the same pair on the same
 * day collapse into one; write the second with a distinct key if that ever
 * happens.
 */
export function settleKey(from, to, cents, day) {
  return JSON.stringify({ from, to, cents, day });
}

/** Today as `YYYY-MM-DD`, UTC — the unit `settleKey` is denominated in. */
export function today(now = new Date()) {
  return now.toISOString().slice(0, 10);
}

/**
 * Divide `cents` between `participants`, to the cent.
 *
 * `participants` must be non-empty and deduplicated — `validate` has already
 * turned both of those into gaps.
 */
export function splitShares(cents, participants) {
  const sorted = [...participants].sort();
  const n = sorted.length;
  const base = Math.trunc(cents / n);
  const remainder = cents - base * n;
  return sorted.map((person, i) => [person, base + (i < remainder ? 1 : 0)]);
}

/**
 * Everything wrong with one act, as sentences.
 *
 * **The one validator** — called by this app's own door (`describe`'s callers
 * append only what passes) AND by the reducer. Two copies would be two
 * answers to "is this act writable", and the door would drift from the
 * reader. `fatal` distinguishes "this cannot be applied at all" from "this
 * applies and somebody should still look at it": a balance held against a
 * name nobody can pay is real money and must stay in the total, but saying so
 * out loud is the difference between a report and a guess.
 */
export function validate(payload, roster) {
  const gaps = [];
  const known = new Set(Object.keys(roster || {}));
  const checkPerson = (p) => {
    // No short-circuit on an empty roster. Before anyone has been added,
    // "we know nobody" is the honest answer and every name should flag —
    // which is what tells a new housemate to run `svrn ring roster add`
    // rather than leaving them with a book that looks fine and converges to
    // nothing but unknown-signer gaps on every other node.
    if (!known.has(p)) {
      gaps.push({
        code: "unknown_person",
        fatal: false,
        message: `a balance against \`${p}\`, who is not in the roster`,
      });
    }
  };
  const cents = payload.amount_cents;
  if (!Number.isInteger(cents) || cents <= 0) {
    gaps.push({
      code: "non_positive_amount",
      fatal: true,
      message:
        `an entry for ${cents} cents — money moves in one direction, so a ` +
        `refund is a correction or an expense the other way`,
    });
  }

  if (payload.kind === EXPENSE) {
    const parts = payload.participants || [];
    if (parts.length === 0) {
      gaps.push({
        code: "no_participants",
        fatal: true,
        message: "an expense split between nobody",
      });
    }
    const seen = new Set();
    for (const p of parts) {
      if (seen.has(p)) {
        gaps.push({
          code: "duplicate_participant",
          fatal: false,
          message: `\`${p}\` was listed twice in one split`,
        });
      }
      seen.add(p);
      checkPerson(p);
    }
    checkPerson(payload.payer);
  } else if (payload.kind === SETTLE) {
    if (payload.from === payload.to) {
      gaps.push({
        code: "self_settle",
        fatal: true,
        message: `\`${payload.from}\` settling with themselves`,
      });
    }
    checkPerson(payload.from);
    checkPerson(payload.to);
  } else {
    gaps.push({
      code: "unknown_kind",
      fatal: true,
      message:
        `an act of kind \`${payload.kind}\`, which this app does not know how ` +
        `to read — a newer version of it may`,
    });
  }
  return gaps;
}

/** Whether this app would write `payload`. The door's half of `validate`. */
export function writable(payload, roster) {
  return !validate(payload, roster).some((g) => g.fatal);
}

/**
 * The empty book.
 *
 * `roster` is the **`{person: [keys]}` map** — `log.roster.members`, not
 * `log.roster`. The rail ships the whole `Roster` value, and passing it whole
 * makes `members` look like the ring's only member and every real name look
 * like a stranger. It renders as a page full of "who is not in the roster"
 * beside balances that are otherwise correct, which reads like a roster
 * problem and is not one. Caught on a live daemon, not by a test — hence the
 * guard rather than a comment.
 *
 * The roster decides which names are payable. It is never consulted for how a
 * cost is divided; see the header.
 */
export function initial(roster = {}) {
  if (roster && Object.prototype.hasOwnProperty.call(roster, "members")) {
    throw new TypeError(
      "initial() wants the person→keys map, not the whole roster object — " +
        "pass `log.roster.members`",
    );
  }
  return { balances: {}, gaps: [], counted: 0, roster, seenSettlements: new Set() };
}

/**
 * Apply one act. Hand this to `window.ring.fold`, which supplies the acts in
 * the rail's order with voided ones already dropped.
 *
 * Every arm is individually zero-sum, and an act that cannot be applied is
 * dropped WHOLE — half an expense is not a conservative reading of it, it is
 * a wrong number.
 */
export function reducer(state, payload, op) {
  const gaps = validate(payload, state.roster);
  for (const g of gaps) state.gaps.push({ ...g, id: op && op.id });
  if (gaps.some((g) => g.fatal)) return state;

  const move = (person, cents) => {
    state.balances[person] = (state.balances[person] || 0) + cents;
  };

  if (payload.kind === EXPENSE) {
    const unique = [...new Set(payload.participants)];
    move(payload.payer, payload.amount_cents);
    for (const [person, share] of splitShares(payload.amount_cents, unique)) {
      move(person, -share);
    }
  } else {
    // One settlement recorded by both parties is one settlement.
    if (state.seenSettlements.has(payload.key)) return state;
    state.seenSettlements.add(payload.key);
    // `from` hands money over, so their debt shrinks and their balance rises
    // toward zero; `to` receives it and theirs falls.
    move(payload.from, payload.amount_cents);
    move(payload.to, -payload.amount_cents);
  }
  state.counted += 1;
  return state;
}

/** One act as a sentence, for the history list. */
export function describe(payload) {
  if (!payload) return "withdrew an earlier entry";
  if (payload.kind === EXPENSE) {
    return `${payload.payer} paid ${money(payload.amount_cents)} for ${
      payload.description
    }, split ${payload.participants.length} ways`;
  }
  if (payload.kind === SETTLE) {
    return `${payload.from} paid ${payload.to} ${money(payload.amount_cents)}`;
  }
  return JSON.stringify(payload);
}

export function money(cents) {
  const sign = cents < 0 ? "-" : "";
  const a = Math.abs(cents);
  return `${sign}$${Math.trunc(a / 100)}.${String(a % 100).padStart(2, "0")}`;
}
