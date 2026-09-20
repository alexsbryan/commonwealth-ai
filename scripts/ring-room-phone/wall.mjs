// SPDX-License-Identifier: AGPL-3.0-or-later
// The wall: the member's screen in the room, watching its own node's rail.
//
// It is a reader, not a writer. It polls the page's log through BeefyMac's own
// `svrn ring dev` proxy — the member's page, unchanged, loopback as it has
// always been — and writes one NDJSON line the first time each act appears,
// carrying the name the page renders for it. That name comes from the ring's
// own `displayName`, so what is recorded is what a person in the room reads,
// not a re-derivation of it.
//
// A wall holding two apps runs one of these per app. The SCAFFOLD's screen is
// the same reader against a different page: `templates/app.js` renders
// `op.person` verbatim in its history row and never resolves a key, so what a
// person reads there is whatever the log route handed it — which is the whole
// claim the second app exists to test. Nothing about a guest is derived here.
//
//   PA        the wall page's url
//   KIND      `doc` (ring-doc's adapter) or `expenses` (a `svrn ring new` app)
//   APP_DIR   for `expenses`: the scaffolded directory, for its own `describe`
//   POLL_MS   how often (250 by default: the 1.4 s window is measured here)
//   WATCH_S   how long to watch
//   REPO      repo root, for the adapter
//   OUT       an NDJSON path; each line {id, actor, guest, name, at}
import { appendFileSync } from "node:fs";

const { PA, REPO, OUT, KIND = "doc", APP_DIR = "", POLL_MS = "250", WATCH_S = "600" } = process.env;

const A = KIND === "doc" ? await import(`file://${REPO}/sovereign/apps/ring-doc/adapter.js`) : null;
const E = KIND === "expenses" ? await import(`file://${APP_DIR}/expenses.js`) : null;
const fold = (log, reducer, init) => {
  let acc = init;
  for (const op of (log && log.ops) || []) {
    if (op.voided || op.payload == null) continue;
    acc = reducer(acc, op.payload, op);
  }
  return acc;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const seen = new Set();
const deadline = Date.now() + Number(WATCH_S) * 1000;

while (Date.now() < deadline) {
  try {
    const r = await fetch(`${PA}/__ring/log`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
      signal: AbortSignal.timeout(10000),
    });
    const log = await r.json();
    const at = Date.now() / 1000;
    const members = (log.roster || {}).members || {};
    if (KIND === "expenses") {
      // The scaffold's history row, built exactly as `templates/app.js` builds
      // it: `op.person` on the left, `expenses.describe(op.payload)` on the
      // right. `name` is that left-hand span, verbatim.
      for (const op of log.ops || []) {
        if (seen.has(op.id)) continue;
        seen.add(op.id);
        const what = op.corrects
          ? `corrected an earlier entry — ${E.describe(op.payload)}`
          : E.describe(op.payload);
        appendFileSync(
          OUT,
          `${JSON.stringify({
            id: op.id,
            actor: op.actor,
            guest: op.guest || null,
            name: op.person,
            row: `${op.person} ${what}`,
            voided: !!op.voided,
            corrects: op.corrects || null,
            roster: Object.keys(members),
            at,
          })}\n`,
        );
      }
      await sleep(Number(POLL_MS));
      continue;
    }
    for (const act of A.decodeActs(log, fold).acts) {
      if (seen.has(act.id)) continue;
      seen.add(act.id);
      appendFileSync(
        OUT,
        `${JSON.stringify({
          id: act.id,
          actor: act.actor,
          guest: act.guest || null,
          // What the wall shows under the paragraph, verbatim.
          name: A.displayName(members, act.actor, act.guest || null),
          // What the LOG route handed the page, beside what the page rendered.
          // ring-doc still composes its own line from the payload's guest
          // field; `person` is the door's stamp, and the two must agree.
          person: act.person,
          roster: Object.keys(members),
          at,
        })}\n`,
      );
    }
  } catch (e) {
    appendFileSync(OUT, `${JSON.stringify({ error: String(e.message || e), at: Date.now() / 1000 })}\n`);
  }
  await sleep(Number(POLL_MS));
}
