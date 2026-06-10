// SPDX-License-Identifier: AGPL-3.0-or-later
// The flagship fault: SIGKILL the supervised daemon child while a turn
// is streaming. Contract under test (supervisor.rs + ReconnectBanner):
//   1. the in-flight stream terminates (error or partial complete) —
//      never a permanent hang,
//   2. the supervisor broadcasts Restarting → Healthy with a NEW pid,
//   3. the ReconnectBanner surfaces while degraded,
//   4. the next turn completes normally (no orphaned session state).
//
// NOTE on inference placement: in supervised mode the desktop still
// runs chat inference in-process; the child daemon owns mesh/knowledge
// surfaces and the heartbeat target. Killing it therefore must not
// corrupt an in-flight turn — but the stream-termination assertion
// stays: whatever the wiring, a user-visible turn must reach a
// terminal state.
import { expect, test } from "../test-base-real";
import { awaitSticky, bridgeInvoke, eventsRecent } from "./spawn";

const BRIDGE = "http://127.0.0.1:9745";

test(
  "daemon child killed mid-stream: stream terminates, supervisor recovers, next turn works",
  async ({ sovereignPage: page }) => {
    // The supervisor's sticky state carries the child pid.
    // supervisor-state payloads are kind-tagged: {"kind":"healthy","pid":N,...}
    // Read the LATEST state from the sticky replay (the ring may have
    // been flushed by earlier specs' token streams — 4096-row cap).
    const sticky = await awaitSticky(BRIDGE, "supervisor-state", 15_000);
    const state = sticky as { kind?: string; pid?: number };
    expect(state?.kind, `supervisor not healthy: ${JSON.stringify(sticky)}`).toBe(
      "healthy",
    );
    const childPid = state.pid;
    expect(childPid).toBeGreaterThan(1);

    // Start a long streaming turn through the UI.
    await page.goto("/");
    await page.locator(".chat-view").waitFor({ state: "visible", timeout: 30_000 });
    await page
      .locator(".input-area textarea")
      .fill("Tell me a long, winding story about a lighthouse keeper.");
    const seqBefore = (await eventsRecent(BRIDGE)).at(-1)?.seq ?? 0;
    await page.locator(".send-btn").click();
    await expect
      .poll(
        async () =>
          (await eventsRecent(BRIDGE, seqBefore)).filter((r) => r.event === "message-chunk")
            .length,
        { timeout: 120_000 },
      )
      .toBeGreaterThan(3);

    // KILL the child mid-stream.
    process.kill(childPid!, "SIGKILL");

    // 1. The stream reaches a terminal state (complete or error).
    await expect
      .poll(
        async () =>
          (await eventsRecent(BRIDGE, seqBefore)).some(
            (r) => r.event === "message-complete" || r.event === "message-error",
          ),
        { timeout: 120_000, intervals: [1000, 2000] },
      )
      .toBe(true);

    // 2. Supervisor: Restarting → Healthy with a NEW pid.
    await expect
      .poll(
        async () => {
          const all = await eventsRecent(BRIDGE, seqBefore);
          const newHealthy = [...all]
            .reverse()
            .find(
              (r) =>
                r.event === "supervisor-state" &&
                (r.payload as { kind?: string })?.kind === "healthy",
            );
          const pid = (newHealthy?.payload as { pid?: number })?.pid;
          return pid && pid !== childPid ? "recovered" : "waiting";
        },
        { timeout: 180_000, intervals: [2000, 5000] },
      )
      .toBe("recovered");

    // 3. The degraded window surfaced the ReconnectBanner at some point
    //    (Restarting state was broadcast — the banner mirrors it). We
    //    assert on the event record rather than racing the DOM, then
    //    confirm the banner is GONE now that we're Healthy again.
    const sawRestarting = (await eventsRecent(BRIDGE, seqBefore)).some(
      (r) =>
        r.event === "supervisor-state" &&
        (r.payload as { kind?: string })?.kind === "restarting",
    );
    expect(sawRestarting, "no Restarting supervisor-state was broadcast").toBe(true);
    await expect(page.locator(".banner")).toHaveCount(0, { timeout: 30_000 });

    // 4. Recovery: a fresh turn completes.
    const seqRecovery = (await eventsRecent(BRIDGE)).at(-1)?.seq ?? 0;
    await page.locator(".input-area textarea").fill("Reply with the single word: recovered");
    await page.locator(".send-btn").click();
    await expect
      .poll(
        async () =>
          (await eventsRecent(BRIDGE, seqRecovery)).some(
            (r) => r.event === "message-complete",
          ),
        { timeout: 150_000, intervals: [1000, 2000] },
      )
      .toBe(true);

    // The conversation store survived the crash.
    const convs = await bridgeInvoke<Array<{ id: string }>>(BRIDGE, "list_conversations");
    expect(convs.length).toBeGreaterThan(0);
  },
);
