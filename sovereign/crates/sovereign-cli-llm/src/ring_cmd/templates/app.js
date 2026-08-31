// Scaffolded by `svrn ring new`. The page. Everything it talks to is
// `window.ring`; everything it KNOWS about money is in `./expenses.js`.
//
// That split is the shape of a ring app. The rail hands you signed acts in
// one agreed order; the reducer turns them into whatever your app is about.
// If you are writing a different app, replace `expenses.js` and this file and
// keep the two calls below.
import * as expenses from "./expenses.js";

const el = (id) => document.getElementById(id);

// The roster the last render read, hoisted so the SUBMIT HANDLER can reach it.
// The door and the reducer have to judge an act against the same names, or
// "this app never writes what it would later report as a gap" is a comment
// rather than a fact.
let roster = {};

async function render() {
  el("err").textContent = "";
  let log;
  try {
    log = await window.ring.log();
  } catch (e) {
    el("err").textContent = String(e.message || e);
    return;
  }

  // The whole fold, in one call. `ring.fold` walks the acts in the rail's
  // order and skips the ones a correction voided — do not iterate `log.ops`
  // yourself, or a voided entry gets counted twice.
  // `.members`, not the whole roster — the rail ships the `Roster` value and
  // the reducer wants the person→keys map inside it. `initial` throws rather
  // than accept the wrapper, because the wrong one produces correct balances
  // beside a page of spurious "not in the roster" gaps.
  roster = (log.roster || {}).members || {};
  const book = window.ring.fold(log, expenses.reducer, expenses.initial(roster));

  el("scope").textContent =
    `${window.ring.namespace} — ${book.counted} entr${book.counted === 1 ? "y" : "ies"}`;

  const rows = Object.entries(book.balances);
  el("balances").innerHTML = rows.length
    ? rows
        .map(([person, cents]) => {
          const cls = cents > 0 ? "owed" : cents < 0 ? "owes" : "";
          const verdict = cents > 0 ? "is owed" : cents < 0 ? "owes" : "settled";
          return `<div class="row"><span>${person}</span><span class="${cls}">${verdict} ${expenses.money(
            Math.abs(cents),
          )}</span></div>`;
        })
        .join("")
    : "<p>Nothing recorded yet.</p>";

  // History, corrections included. A voided entry stays visible and struck
  // through: showing the correction without the thing it corrected leaves a
  // reader unable to check the change.
  el("history").innerHTML = log.ops
    .slice()
    .reverse()
    .map((op) => {
      const what = op.corrects
        ? `corrected an earlier entry — ${expenses.describe(op.payload)}`
        : expenses.describe(op.payload);
      return `<div class="row${op.voided ? " voided" : ""}"><span>${
        op.person
      }</span><span>${what}</span></div>`;
    })
    .join("");

  // **Two kinds of gap, and both must be shown.**
  //
  // `log.gaps` come from the RAIL: an op that has not reached this node, a
  // signature that does not verify, a line a newer build wrote. They mean the
  // numbers above cover a subset.
  //
  // `book.gaps` come from THIS app: an amount that is not money, a name the
  // roster does not know. They mean an act could not be read as an expense.
  //
  // Never hide either panel to make the page look tidier. A total shown
  // without its gaps is a confident number over a subset, which is the one
  // failure this whole stack exists to avoid.
  const gaps = [
    // `message` is the rail's own sentence for this gap — the same words
    // `svrn ring log` prints. Render that, not the tag.
    ...(log.gaps || []).map((g) => g.message || g.gap),
    ...book.gaps.map((g) => g.message),
  ];
  el("gaps").hidden = gaps.length === 0;
  el("gaplist").innerHTML = gaps.map((g) => `<li>${g}</li>`).join("");
}

el("add").addEventListener("submit", async (e) => {
  e.preventDefault();
  const f = new FormData(e.target);
  const act = {
    kind: expenses.EXPENSE,
    payer: String(f.get("payer")).trim(),
    // Cents, always. The rail refuses a fractional number outright — two
    // nodes must derive identical bytes from an act and JSON does not promise
    // that for fractions.
    amount_cents: Math.round(parseFloat(f.get("amount")) * 100),
    description: String(f.get("description")),
    participants: String(f.get("participants"))
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean),
  };
  // The door runs the SAME validator the reducer runs, so this app never
  // writes an act it would later report as a gap. One validator, two callers
  // — a second copy here would be a second answer to "is this writable", and
  // the door would drift from the reader.
  //
  // Only the FATAL gaps stop a write. A balance held against a name the
  // roster does not know yet is real money and must be recordable; it shows
  // up in the gap panel, which is the honest place for it.
  if (!expenses.writable(act, roster)) {
    el("err").textContent = expenses
      .validate(act, roster)
      .filter((g) => g.fatal)
      .map((g) => g.message)
      .join("; ");
    return;
  }

  try {
    await window.ring.record(act);
    e.target.reset();
  } catch (err) {
    el("err").textContent = String(err.message || err);
  }
  render();
});

render();
setInterval(render, 5000);
