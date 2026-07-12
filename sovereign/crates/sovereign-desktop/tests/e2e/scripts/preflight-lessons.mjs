// SPDX-License-Identifier: AGPL-3.0-or-later
// TEACHABLE P0 — observability preflight (TEACHABLE.md §8; the gap-card
// preflight precedent, preflight-gap.mjs). Three legs, glassbox-first:
//
//   1. CRUD leg (runtime-independent): save_lesson with a synthetic
//      draft → list_lessons contains it (enabled=true, enforcement
//      echoed) → set_lesson_enabled(false) → re-list shows disabled →
//      delete_lesson → gone. Validates the command surface + NoteStore
//      round-trip over the bridge before any UI or capture is trusted.
//   2. Event leg: a durative coaching message in a real conversation →
//      lesson-proposed within 240s → save the ACTUAL draft →
//      list-verify → one ordinary follow-up turn carries
//      metadata.kept_lesson (the whisper source). SOFT-reported: a
//      missing card is a capture finding, not a harness failure.
//   3. Negative probe: one ordinary question → NO lesson-proposed in
//      the observe window (capture precision).
//
// Usage: node tests/e2e/scripts/preflight-lessons.mjs   (dev daemon on :9741)
// Exits 0 with a signal-by-signal report; nonzero on hard-leg failure.
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
const say = (m) => console.log(`[preflight-lessons ${el()}] ${m}`);
const report = []; // {signal, ok, hard, detail}

const COACHING_MSG =
  "From now on, keep your answers short — a paragraph at most unless I ask for more.";
const ORDINARY_Q = "whats a decent way to keep basil alive on a kitchen windowsill";

async function sendAndComplete(convo, message, timeoutMs = 300_000) {
  const since = await bridge.lastSeq();
  const res = await bridge.invoke(
    "send_message_stream",
    { message, conversationId: convo },
    150_000,
  );
  const messageId = res?.message_id ?? res;
  const done = await bridge.awaitEvent(
    since,
    (r) => r.event === "message-complete" && r.payload?.message_id === messageId,
    timeoutMs,
  );
  return { messageId, done, seq: done?.seq ?? since };
}

async function main() {
  say("spawning bridged desktop (attach mode)…");
  const app = await spawnDesktop({
    bridge,
    attach: true,
    autoCollaborate: true,
    tag: "lessons-preflight",
  });
  const madeConvos = [];
  try {
    await awaitBackendReady(bridge);
    say("backend-ready ✓");
    for (const ev of [
      "message-chunk",
      "message-complete",
      "message-error",
      "lesson-proposed",
      "turn-narration",
      "backend-error",
    ])
      await bridge.listen(ev);
    say("subscribed ✓ (lesson-proposed rides listen_any — zero bridge changes)");

    // ── Leg 1: CRUD over the bridge (runtime-independent) ───────────
    say("leg 1: lesson CRUD round-trip…");
    let noteId = null;
    try {
      noteId = await bridge.invoke(
        "save_lesson",
        {
          draft: {
            id: `preflight-${Date.now()}`,
            conversation_id: "preflight",
            message_id: "",
            display: "Preflight synthetic lesson — safe to delete.",
            prompt_form: "",
            enforcement: "param",
            params: { soft_target_cap: 300 },
            taught_from: "preflight synthetic draft",
            drafted_display: null,
          },
        },
        30_000,
      );
    } catch (e) {
      report.push({
        signal: "save_lesson invokable via bridge",
        ok: false,
        hard: true,
        detail: `error: ${String(e).slice(0, 240)}`,
      });
    }
    if (noteId) {
      const listed = ((await bridge.invoke("list_lessons", {}, 15_000).catch(() => [])) ?? []).find(
        (l) => l.id === noteId,
      );
      report.push({
        signal: "save_lesson → list_lessons round-trip",
        ok: !!listed && listed.enabled === true && listed.enforcement === "param",
        hard: true,
        detail: listed
          ? `note=${noteId} enabled=${listed.enabled} enforcement=${listed.enforcement} display="${listed.display}"`
          : `note ${noteId} not present in list_lessons`,
      });
      const toggled = await bridge
        .invoke("set_lesson_enabled", { id: noteId, enabled: false }, 15_000)
        .catch((e) => String(e));
      const relisted = ((await bridge.invoke("list_lessons", {}, 15_000).catch(() => [])) ?? []).find(
        (l) => l.id === noteId,
      );
      report.push({
        signal: "set_lesson_enabled toggles without deleting",
        ok: toggled === true && relisted?.enabled === false,
        hard: true,
        detail: `toggle=${JSON.stringify(toggled)} relisted.enabled=${relisted?.enabled}`,
      });
      const deleted = await bridge
        .invoke("delete_lesson", { id: noteId }, 15_000)
        .catch((e) => String(e));
      const gone = !((await bridge.invoke("list_lessons", {}, 15_000).catch(() => [])) ?? []).some(
        (l) => l.id === noteId,
      );
      report.push({
        signal: "delete_lesson is a real hard delete",
        ok: deleted === true && gone,
        hard: true,
        detail: `delete=${JSON.stringify(deleted)} gone=${gone}`,
      });
    }

    // ── Leg 2: capture → save → whisper (soft) ──────────────────────
    say("leg 2: durative coaching → lesson-proposed → save → whisper…");
    const convo = (await bridge.invoke("create_conversation", {})).id;
    madeConvos.push(convo);
    // A prior turn gives the conation transform an artifact to work on
    // (mirrors real usage; capture itself doesn't require it).
    await sendAndComplete(convo, ORDINARY_Q);
    const teach = await sendAndComplete(convo, COACHING_MSG);
    report.push({
      signal: "coaching turn completes normally (capture never blocks the turn)",
      ok: !!teach.done,
      hard: true,
      detail: teach.done
        ? `answer ${String(teach.done.payload?.full_text ?? "").length} chars`
        : "no message-complete in 300s",
    });
    const cardRow = teach.done
      ? await bridge.awaitEvent(teach.seq, (r) => r.event === "lesson-proposed", 240_000)
      : null;
    report.push({
      signal: "lesson-proposed fires on a durative coaching turn",
      ok: !!cardRow,
      hard: false, // capture is routing-dependent: a miss is a FINDING, not a harness failure
      detail: cardRow
        ? `enforcement=${cardRow.payload?.enforcement} display="${String(
            cardRow.payload?.display ?? "",
          ).slice(0, 100)}" keys=${Object.keys(cardRow.payload ?? {}).join(",")}`
        : "no card in 240s — check routing (must classify as conation) + durative floor",
    });
    if (cardRow?.payload) {
      let savedId = null;
      try {
        savedId = await bridge.invoke(
          "save_lesson",
          { draft: { ...cardRow.payload, drafted_display: null } },
          30_000,
        );
      } catch (e) {
        savedId = null;
        report.push({
          signal: "save_lesson accepts the ACTUAL event payload",
          ok: false,
          hard: true,
          detail: String(e).slice(0, 240),
        });
      }
      if (savedId) {
        const listed = ((await bridge.invoke("list_lessons", {}, 15_000).catch(() => [])) ?? []).some(
          (l) => l.id === savedId,
        );
        report.push({
          signal: "save_lesson accepts the ACTUAL event payload",
          ok: listed,
          hard: true,
          detail: `note=${savedId} listVerified=${listed}`,
        });
        // Whisper: the next influenced answer carries metadata.kept_lesson.
        const follow = await sendAndComplete(convo, "and how often should i water it");
        const kept = follow.done?.payload?.metadata?.kept_lesson ?? null;
        report.push({
          signal: "first influenced answer carries metadata.kept_lesson (whisper source)",
          ok: !!kept,
          hard: false, // depends on the lesson engaging this turn's path
          detail: kept
            ? `kept_lesson=${JSON.stringify(kept)}`
            : `absent; lessons_applied=${JSON.stringify(
                follow.done?.payload?.metadata?.lessons_applied ?? null,
              )}`,
        });
        // Tidy: remove the saved lesson so preflight runs stay hermetic-ish.
        await bridge.invoke("delete_lesson", { id: savedId }, 15_000).catch(() => {});
      }
    }

    // ── Leg 3: negative probe ───────────────────────────────────────
    say("leg 3: ordinary question must NOT propose a lesson…");
    const convo2 = (await bridge.invoke("create_conversation", {})).id;
    madeConvos.push(convo2);
    const plain = await sendAndComplete(convo2, "best time of year to visit yellowstone to avoid crowds?");
    const falseFire = plain.done
      ? await bridge.awaitEvent(plain.seq, (r) => r.event === "lesson-proposed", 20_000)
      : null;
    report.push({
      signal: "no lesson-proposed on a non-coaching turn (capture precision)",
      ok: !falseFire,
      hard: true,
      detail: falseFire
        ? `FALSE FIRE: ${JSON.stringify(falseFire.payload).slice(0, 200)}`
        : "quiet for 20s post-answer ✓",
    });
  } finally {
    for (const id of madeConvos)
      await bridge.invoke("delete_conversation", { conversationId: id }, 10_000).catch(() => {});
    await app.killGroup();
  }

  console.log("\n══ TEACHABLE lessons preflight report ══");
  let hardFail = false;
  for (const r of report) {
    const mark = r.ok ? "✓" : r.hard ? "✗" : "△";
    console.log(`${mark} ${r.signal}\n    ${r.detail}`);
    if (!r.ok && r.hard) hardFail = true;
  }
  fs.writeFileSync(
    path.join(ARTIFACTS, "lessons-preflight-report.json"),
    JSON.stringify({ ts: Date.now(), report }, null, 2),
  );
  console.log(`\nreport → test-artifacts/lessons-preflight-report.json`);
  process.exit(hardFail ? 2 : 0);
}

main().catch((e) => {
  console.error(`[preflight-lessons] fatal: ${e}`);
  process.exit(1);
});
