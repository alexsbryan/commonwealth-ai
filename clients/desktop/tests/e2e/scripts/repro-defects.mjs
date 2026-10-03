// SPDX-License-Identifier: AGPL-3.0-or-later
// Targeted A/B repro for the #9 catalog-GK gate net (and a no-over-gate control).
// Reuses the felt/chaos bridge plumbing (spawn desktop → daemon attach → send).
// The 3 catalog questions were catalog-ONLY fabrications in the ceiling run
// (steps 373/519/535). With retrieval_is_catalog_only() closing the GK-caveat
// exemption, they should now ABSTAIN-with-offer instead of asserting confident
// wrong specifics. The control (photosynthesis, richly in full-text wikipedia)
// must still answer normally — proving we did not over-gate.
import path from "node:path";
import { makeBridge, spawnDesktop, awaitBackendReady, ARTIFACTS } from "./lib/harness.mjs";

const argv = process.argv.slice(2);
const ATTACH = argv.includes("--attach");
const SPAWN = argv.includes("--spawn");

const QUESTIONS = [
  // #10 target — MIXED/full-text-miss retrieval. The catalog title "1926
  // Darlington by-election" is retrieved but the atlas has no such entity and a
  // tangential body chunk disables the catalog-only net; baseline shipped a
  // GK-caveated WRONG winner (Robert Gascoyne-Cecil / Conservative; real winner
  // was Labour). Fixed via the retrieval-derived title anchor.
  { id: "darlington-1926", q: "Who won the 1926 Darlington by-election?", expect: "abstain" },
  { id: "washburn-1938", q: "What was the win-loss-tie record of the 1938 Washburn Ichabods football team?", expect: "abstain" },
  { id: "ranji-1957", q: "Which team won the 1957-58 Ranji Trophy?", expect: "abstain" },
  { id: "soviet-1946", q: "What were the results of the 1946 Soviet Union legislative election?", expect: "abstain" },
  { id: "photosynthesis-CONTROL", q: "How does photosynthesis work? Walk me through the main stages.", expect: "answer" },
];

const bridge = makeBridge();
let desk = { killGroup: async () => {} };
if (SPAWN) {
  const appLog = path.join(ARTIFACTS, "repro-defects-app.log");
  desk = await spawnDesktop({ bridge, attach: ATTACH, tag: "repro", appLog,
    rustLog: process.env.RUST_LOG ?? "sovereign_desktop=info,sovereign_core=info" });
  await awaitBackendReady(bridge, 300_000);
  for (const ev of ["message-chunk", "message-complete", "message-error", "backend-error"])
    await bridge.listen(ev);
  console.log("backend ready; app log:", appLog);
}

// Signals of an honest abstention / ingest-offer vs a confident fabrication.
const ABSTAIN_RE = /don'?t have|not (in|covered|established)|couldn'?t (confirm|find)|only (a )?catalog|catalog (entry|metadata)|want me to (read|ingest)|haven'?t read|would take about|no reliable information|can'?t (confirm|verify)/i;

const results = [];
for (const { id, q, expect } of QUESTIONS) {
  const convo = (await bridge.invoke("create_conversation", {})).id;
  const since = await bridge.lastSeq();
  let mid;
  try { const r = await bridge.invoke("send_message_stream", { message: q, conversationId: convo }, 150_000); mid = r?.message_id ?? r; }
  catch (e) { console.log(`\n### ${id}: SEND FAILED ${e}`); continue; }
  const ans = await bridge.awaitChatAnswer(since, mid, Number(process.env.REPRO_TIMEOUT_MS || 300_000));
  if (!ans) { console.log(`\n### ${id}: NO ANSWER (timeout)`); results.push({ id, expect, verdict: "timeout" }); continue; }
  const a = (ans.answer || "").trim();
  const abstained = ABSTAIN_RE.test(a);
  const verdict = expect === "abstain" ? (abstained ? "PASS (abstained/offered)" : "FAIL (did not abstain — inspect)")
                                       : (abstained ? "FAIL (over-gated a control!)" : "PASS (answered)");
  results.push({ id, expect, chunks: ans.chunks?.length ?? 0, verdict, a });
  console.log(`\n### ${id}  [expect ${expect}] chunks=${ans.chunks?.length ?? 0}  → ${verdict}`);
  console.log("Q:", q);
  console.log("A:", a.slice(0, 700));
}
console.log("\n==== SUMMARY ====");
for (const r of results) console.log(`  ${r.id.padEnd(24)} ${r.verdict}`);
await desk.killGroup?.();
process.exit(0);
