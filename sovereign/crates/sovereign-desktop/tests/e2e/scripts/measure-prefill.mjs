// SPDX-License-Identifier: AGPL-3.0-or-later
// Prefill-state A/B instrument (Win 1 measurement). Sends a FIXED list
// of distinct in-corpus questions, each in a fresh conversation, and
// records per-question TTFT + total latency. Run once per arm:
//
//   arm OFF: daemon restarted with SOVEREIGN_PREFIX_STATE=0
//   arm ON : daemon restarted with the cache enabled (default)
//
//   node tests/e2e/scripts/measure-prefill.mjs --label off
//   node tests/e2e/scripts/measure-prefill.mjs --label on
//
// Questions differ per turn ON PURPOSE: identical prompts are
// unpinnable by design (no tail → no fresh logits), while distinct
// questions share exactly the stable prefix, so turns 1-2 learn the
// pin and turns 3+ restore it. Pair per-question across arms.
// Output: test-artifacts/prefill-ab-<label>.json + a stdout table.
import fs from "node:fs";
import path from "node:path";
import {
  makeBridge,
  spawnDesktop,
  awaitBackendReady,
  ARTIFACTS,
} from "./lib/harness.mjs";

const argv = process.argv.slice(2);
const flag = (name, fb) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : fb;
};
const LABEL = flag("label", "run");

// Distinct, corpus-plausible knowledge questions (SEP/wikipedia are
// resident on this box). Fixed list = paired across arms.
const QUESTIONS = [
  "What is the categorical imperative according to Kant?",
  "How does utilitarianism define the good?",
  "What was the main argument of Hume against induction?",
  "What is the difference between rationalism and empiricism?",
  "How did Aristotle define virtue?",
  "What does compatibilism claim about free will?",
];

const bridge = makeBridge();
const say = (m) => console.log(`[prefill-ab:${LABEL}] ${m}`);

async function main() {
  const app = await spawnDesktop({
    bridge,
    attach: true,
    autoCollaborate: true,
    tag: `prefill-ab-${LABEL}`,
  });
  const rows = [];
  const madeConvos = [];
  try {
    await awaitBackendReady(bridge);
    for (const ev of ["message-chunk", "message-complete", "message-error"])
      await bridge.listen(ev);
    say(`backend ready — ${QUESTIONS.length} questions, fresh conversation each`);

    for (const [i, q] of QUESTIONS.entries()) {
      const convo = (await bridge.invoke("create_conversation", {})).id;
      madeConvos.push(convo);
      const since = await bridge.lastSeq();
      const t0 = Date.now();
      let messageId;
      try {
        const res = await bridge.invoke(
          "send_message_stream",
          { message: q, conversationId: convo },
          150_000,
        );
        messageId = res?.message_id ?? res;
      } catch (e) {
        rows.push({ i, q, error: String(e).slice(0, 200) });
        say(`  q${i + 1} SEND ERROR ${String(e).slice(0, 80)}`);
        continue;
      }
      let ttft = null;
      let cursor = since;
      let terminal = null;
      const deadline = Date.now() + 300_000;
      while (!terminal && Date.now() < deadline) {
        const events = await bridge.recent(cursor).catch(() => []);
        if (events.length) cursor = events[events.length - 1].seq;
        for (const r of events) {
          if (r.event === "message-chunk" && ttft == null) {
            const mid = r.payload?.message_id ?? r.payload?.messageId;
            if (!mid || mid === messageId) ttft = Date.now() - t0;
          }
          if (r.event === "message-complete" && r.payload?.message_id === messageId)
            terminal = r;
          if (r.event === "message-error") terminal = terminal ?? r;
        }
        if (!terminal) await new Promise((r) => setTimeout(r, 500));
      }
      const total = Date.now() - t0;
      const answerLen = String(terminal?.payload?.full_text ?? "").length;
      rows.push({ i, q, ttftMs: ttft, totalMs: total, answerLen });
      say(
        `  q${i + 1} ttft=${ttft == null ? "-" : (ttft / 1000).toFixed(1)}s ` +
          `total=${(total / 1000).toFixed(1)}s len=${answerLen}`,
      );
    }
  } finally {
    for (const id of madeConvos)
      await bridge.invoke("delete_conversation", { conversationId: id }, 10_000).catch(() => {});
    await app.killGroup();
  }

  const out = path.join(ARTIFACTS, `prefill-ab-${LABEL}.json`);
  fs.writeFileSync(out, JSON.stringify({ label: LABEL, ts: Date.now(), rows }, null, 2));
  const ttfts = rows.map((r) => r.ttftMs).filter((x) => x != null).sort((a, b) => a - b);
  const med = ttfts.length ? ttfts[Math.floor(ttfts.length / 2)] : null;
  say(`done — ttft median ${med == null ? "-" : (med / 1000).toFixed(1)}s → ${out}`);
}

main().catch((e) => {
  console.error(`[prefill-ab] fatal: ${e}`);
  process.exit(1);
});
