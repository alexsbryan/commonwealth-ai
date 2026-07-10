// SPDX-License-Identifier: AGPL-3.0-or-later
// Persona-QA Increment 0 — observability preflight (see PERSONA_QA_DESIGN.md §8).
//
// Drives ONE real gap → search → refine loop through the command bridge and
// reports whether every signal the persona harness leans on is machine-visible:
//   1. information-request fires on an out-of-corpus ask (needs
//      auto_collaborate=true in the baked profile — chaos bakes false, which is
//      why chaos runs never saw a gap card).
//   2. submit_information_search(key, gap, conversationId) is invokable and
//      returns SearchAugmentation {query, backend_id, sources, accepted}.
//   3. message-refined lands on the SAME message_id with new_content.
//   4. turn-narration / GapCheckFired chips are visible (the "did the gap
//      check even run" signal — the ≥4000-char pre-skip detector).
//   5. Open question: what does IGNORING the card do to the next turn in the
//      same conversation? (run_collaboration blocks on the pending oneshot.)
//
// Usage: node tests/e2e/scripts/preflight-gap.mjs   (dev daemon on :9741 required)
// Exits 0 with a signal-by-signal report; nonzero on harness-level failure.
import {
  makeBridge,
  spawnDesktop,
  awaitBackendReady,
  ARTIFACTS,
} from "./lib/harness.mjs";
import path from "node:path";
import fs from "node:fs";

const bridge = makeBridge();
const t0 = Date.now();
const el = () => `+${((Date.now() - t0) / 1000).toFixed(0)}s`;
const say = (m) => console.log(`[preflight ${el()}] ${m}`);
const report = []; // {signal, ok, detail}
const seenNarration = [];

// An everyday frontier-user ask with zero relation to any resident corpus —
// and deliberately web-answerable, so live DDG has something to rescue with.
const OUT_OF_CORPUS_Q = "whats a good way to get rid of fruit flies in my kitchen";
const OUT_OF_CORPUS_Q2 = "best time of year to visit yellowstone to avoid crowds?";

async function main() {
  say("spawning bridged desktop (attach mode, auto_collaborate=ON)…");
  const app = await spawnDesktop({
    bridge,
    attach: true,
    autoCollaborate: true,
    tag: "persona-preflight",
  });
  try {
    await awaitBackendReady(bridge);
    say("backend-ready ✓");
    for (const ev of [
      "message-chunk",
      "message-complete",
      "message-error",
      "information-request",
      "message-refined",
      "turn-narration",
      "backend-error",
      "supervisor-state",
    ])
      await bridge.listen(ev);
    say("subscribed to the persona event set ✓");

    const corpora = ((await bridge.invoke("list_corpora", {}).catch(() => [])) ?? [])
      .filter((c) => c.status === "installed")
      .map((c) => c.id);
    say(`installed corpora: ${corpora.length} (${corpora.slice(0, 4).join(", ")}…)`);
    const scope = corpora[0] ?? null;

    const convo = (await bridge.invoke("create_conversation", {})).id;
    if (scope)
      await bridge.invoke("set_conversation_enabled_corpora", {
        conversationId: convo,
        enabledCorpora: [scope],
      });
    say(`conversation ${convo} scoped to ${scope ?? "(nothing installed)"}`);

    // ── Turn 1: out-of-corpus ask → expect gap card ─────────────────
    const since = await bridge.lastSeq();
    const sendT = Date.now();
    const res = await bridge.invoke("send_message_stream", {
      message: OUT_OF_CORPUS_Q,
      conversationId: convo,
    });
    const messageId = res?.message_id ?? res;
    say(`turn 1 sent (message_id=${messageId}): "${OUT_OF_CORPUS_Q}"`);

    const narrationSpy = (r) => {
      if (r.event === "turn-narration") {
        const p = r.payload?.event ?? r.payload ?? {};
        seenNarration.push(p);
        say(`  chip: [${p.phase ?? "?"}] ${String(p.text ?? "").slice(0, 90)}`);
      }
      if (r.event === "message-chunk" && !narrationSpy.ttft) {
        narrationSpy.ttft = Date.now() - sendT;
      }
    };
    const got = await bridge.awaitEvent(
      since,
      (r) => r.event === "message-complete" && r.payload?.message_id === messageId,
      300_000,
      narrationSpy,
    );
    if (!got) throw new Error("turn 1 never completed");
    const answer = String(got.payload?.full_text ?? "");
    const chunks = got.payload?.metadata?.retrieved_chunks ?? [];
    say(
      `turn 1 complete: ttft=${narrationSpy.ttft ?? "?"}ms, ${answer.length} chars, ` +
        `${chunks.length} retrieved chunks. Answer head: "${answer.slice(0, 140)}"`,
    );

    // ── Signal 1: information-request ───────────────────────────────
    say("waiting up to 240s for information-request (gap check runs post-stream on the Fast slot)…");
    const cards = [];
    const firstCard = await bridge.awaitEvent(
      got.seq,
      (r) => r.event === "information-request",
      240_000,
      narrationSpy,
    );
    if (firstCard) cards.push(firstCard);
    // Short grace window to see whether a SECOND card can arrive for one turn
    // (design open question 2).
    if (firstCard) {
      const second = await bridge.awaitEvent(
        firstCard.seq,
        (r) => r.event === "information-request",
        20_000,
        narrationSpy,
      );
      if (second) cards.push(second);
    }
    report.push({
      signal: "information-request fires on out-of-corpus ask",
      ok: cards.length > 0,
      detail: cards.length
        ? `payload keys: ${Object.keys(cards[0].payload ?? {}).join(", ")}; gap="${String(
            cards[0].payload?.gap ?? "",
          ).slice(0, 120)}"; hints=${JSON.stringify(cards[0].payload?.search_hints ?? null)}`
        : `no card in 240s. GapCheckFired chips seen: ${
            seenNarration.filter((n) => /gap/i.test(String(n.phase))).length
          }`,
    });
    report.push({
      signal: "multiple cards per turn (open question 2)",
      ok: true,
      detail: `${cards.length} card(s) observed for turn 1`,
    });
    report.push({
      signal: "turn-narration / GapCheckFired visible",
      ok: seenNarration.some((n) => /gap/i.test(String(n.phase ?? ""))),
      detail: `phases seen: ${[...new Set(seenNarration.map((n) => n.phase))].join(", ") || "(none)"}`,
    });

    // ── Signals 2+3: search affordance → message-refined ────────────
    if (cards.length) {
      const card = cards[0].payload;
      say(`clicking "Search the web": key=${card.key}, query=gap text (mirrors InformationRequestCard.handleSearch)`);
      const refinedSince = cards[0].seq;
      let augmentation = null;
      let searchErr = null;
      try {
        augmentation = await bridge.invoke(
          "submit_information_search",
          { key: card.key, query: card.gap, conversationId: convo },
          90_000,
        );
      } catch (e) {
        searchErr = String(e).slice(0, 300);
      }
      report.push({
        signal: "submit_information_search invokable via bridge",
        ok: !!augmentation,
        detail: augmentation
          ? `backend=${augmentation.backend_id}, accepted=${augmentation.accepted}, sources=${
              (augmentation.sources ?? []).length
            } ${(augmentation.sources ?? [])
              .slice(0, 3)
              .map((s) => s.url)
              .join(" | ")}`
          : `error: ${searchErr}`,
      });
      if (augmentation) {
        say("waiting up to 300s for message-refined on the same message_id…");
        const refined = await bridge.awaitEvent(
          refinedSince,
          (r) => r.event === "message-refined",
          300_000,
          narrationSpy,
        );
        report.push({
          signal: "message-refined lands on the SAME message_id",
          ok: !!refined && refined.payload?.message_id === messageId,
          detail: refined
            ? `message_id match=${refined.payload?.message_id === messageId}, new_content ${String(
                refined.payload?.new_content ?? "",
              ).length} chars, head: "${String(refined.payload?.new_content ?? "").slice(0, 140)}"`
            : "no message-refined in 300s",
        });
      }
    } else {
      say("skipping search-affordance signals (no card to click)");
    }

    // ── Signal 5: ignore the card, keep talking (fresh conversation) ─
    say("open question 5: ignoring a gap card — does the NEXT turn still work?");
    const convo2 = (await bridge.invoke("create_conversation", {})).id;
    if (scope)
      await bridge.invoke("set_conversation_enabled_corpora", {
        conversationId: convo2,
        enabledCorpora: [scope],
      });
    const since2 = await bridge.lastSeq();
    const r2 = await bridge.invoke("send_message_stream", {
      message: OUT_OF_CORPUS_Q2,
      conversationId: convo2,
    });
    const mid2 = r2?.message_id ?? r2;
    const done2 = await bridge.awaitEvent(
      since2,
      (r) => r.event === "message-complete" && r.payload?.message_id === mid2,
      300_000,
    );
    if (!done2) throw new Error("turn 2 never completed");
    const card2 = await bridge.awaitEvent(
      done2.seq,
      (r) => r.event === "information-request",
      240_000,
    );
    if (card2) {
      say("card arrived — IGNORING it and sending a follow-up turn…");
      const since3 = await bridge.lastSeq();
      const r3 = await bridge.invoke("send_message_stream", {
        message: "ok different question - how do i sharpen a kitchen knife",
        conversationId: convo2,
      });
      const mid3 = r3?.message_id ?? r3;
      const done3 = await bridge.awaitEvent(
        since3,
        (r) => r.event === "message-complete" && r.payload?.message_id === mid3,
        300_000,
      );
      report.push({
        signal: "next turn works while a gap card is pending-ignored",
        ok: !!done3,
        detail: done3
          ? `follow-up completed (${String(done3.payload?.full_text ?? "").length} chars) with card un-answered`
          : "follow-up turn never completed — pending card may block the conversation",
      });
    } else {
      report.push({
        signal: "next turn works while a gap card is pending-ignored",
        ok: true,
        detail: "(no second card fired — could not exercise; retry with another question)",
      });
    }

    // cleanup
    for (const id of [convo, convo2])
      await bridge.invoke("delete_conversation", { conversationId: id }, 10_000).catch(() => {});
  } finally {
    await app.killGroup();
  }

  console.log("\n══ persona-QA preflight report ══");
  let allOk = true;
  for (const r of report) {
    console.log(`${r.ok ? "✓" : "✗"} ${r.signal}\n    ${r.detail}`);
    if (!r.ok) allOk = false;
  }
  fs.writeFileSync(
    path.join(ARTIFACTS, "persona-preflight-report.json"),
    JSON.stringify({ ts: Date.now(), report, narration: seenNarration }, null, 2),
  );
  console.log(`\nreport → test-artifacts/persona-preflight-report.json`);
  process.exit(allOk ? 0 : 2);
}

main().catch((e) => {
  console.error(`[preflight] fatal: ${e}`);
  process.exit(1);
});
