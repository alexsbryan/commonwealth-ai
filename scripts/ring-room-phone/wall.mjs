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
//   PA        the wall page's url
//   POLL_MS   how often (250 by default: the 1.4 s window is measured here)
//   WATCH_S   how long to watch
//   REPO      repo root, for the adapter
//   OUT       an NDJSON path; each line {id, actor, guest, name, at}
import { appendFileSync } from "node:fs";

const { PA, REPO, OUT, POLL_MS = "250", WATCH_S = "600" } = process.env;

const A = await import(`file://${REPO}/sovereign/apps/ring-doc/adapter.js`);
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
