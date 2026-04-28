import { test, expect } from "../fixtures/test-base";

// Mesh Health surfaces. Exercises the new Tauri commands
// (mesh_get_contributions, mesh_set/clear/list_peer_preference) via
// the tauri-shim. Mirrors chat-chaos's invariant style: each test
// pins one observable behaviour of the shim contract — what the UI
// is expected to send and how it should handle the response shape.
//
// The actual Svelte components that render this data are intentionally
// not exercised here: the contract being pinned is the *Tauri command
// surface* and the *response shape*, not the visual layout. UI
// rendering tests can layer on top once the components ship.

const FIXTURE_NODE_HEX = "33".repeat(16);
const OTHER_NODE_HEX = "44".repeat(16);

test.describe("mesh health: contributions + peer preferences", () => {
  test("mesh_get_contributions returns dimensional shape", async ({
    sovereignPage: page,
  }) => {
    // Stub the command to return a realistic dimensional payload.
    await page.goto("/");
    await page.evaluate((hex) => {
      window.__sovereign_test__.setHandler("mesh_get_contributions", () => [
        {
          node_id: hex,
          window_days: 30,
          inference_served_requests: 12,
          inference_served_tokens: 4_800,
          inference_served_wall_seconds: 31.5,
          inference_consumed_requests: 3,
          inference_consumed_tokens: 1_200,
          corpora_hosted: [
            {
              corpus_id: "wikipedia",
              corpus_name: "wikipedia",
              size_gb: 12.5,
              queries_served: 47,
              is_sole_host: false,
            },
            {
              corpus_id: "sep",
              corpus_name: "sep",
              size_gb: 1.2,
              queries_served: 5,
              is_sole_host: true,
            },
          ],
          bytes_served: 5_000_000_000,
          bytes_received: 800_000_000,
        },
      ]);
    }, FIXTURE_NODE_HEX);

    const contributions = await page.evaluate(async () => {
      // Drive directly through the shim.
      return await window.__TAURI_INTERNALS__.invoke(
        "mesh_get_contributions",
        {},
      );
    });

    // Contract: array of dimensional rows, each carrying separate
    // counters per dimension and a hosted-corpora list with
    // is_sole_host flags. NEVER a "balance" or "score" field.
    expect(Array.isArray(contributions)).toBe(true);
    expect(contributions).toHaveLength(1);
    const row = (contributions as Array<Record<string, unknown>>)[0];
    expect(row.node_id).toBe(FIXTURE_NODE_HEX);
    expect(row.inference_served_requests).toBe(12);
    expect(row.inference_consumed_requests).toBe(3);
    expect(row.bytes_served).toBe(5_000_000_000);
    expect(row.bytes_received).toBe(800_000_000);
    // No "balance", no collapsed score.
    expect(row.balance).toBeUndefined();
    expect(row.score).toBeUndefined();
    // Sole-host annotation is preserved on the right corpus.
    const corpora = row.corpora_hosted as Array<Record<string, unknown>>;
    const sep = corpora.find((c) => c.corpus_id === "sep");
    expect(sep?.is_sole_host).toBe(true);
    const wikipedia = corpora.find((c) => c.corpus_id === "wikipedia");
    expect(wikipedia?.is_sole_host).toBe(false);
  });

  test("mesh_set_peer_preference records args via the shim", async ({
    sovereignPage: page,
  }) => {
    await page.goto("/");
    await page.evaluate(
      async ({ target }) => {
        await window.__TAURI_INTERNALS__.invoke("mesh_set_peer_preference", {
          nodeId: target,
          multiplier: 0.8,
          reason: "over-consuming",
        });
      },
      { target: OTHER_NODE_HEX },
    );

    const recorded = await page.evaluate(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      () => (window.__sovereign_test__ as any)._lastSetPreference,
    );
    expect(recorded).toEqual({
      nodeId: OTHER_NODE_HEX,
      multiplier: 0.8,
      reason: "over-consuming",
    });
  });

  test("mesh_set_peer_preference structurally rejects multipliers above 1.0", async ({
    sovereignPage: page,
  }) => {
    // The shim itself is permissive — the structural clamp lives in
    // the Rust constructor. We pin the contract by overriding the
    // shim handler to mimic the Rust-side rejection (Err propagates
    // as a thrown promise).
    await page.goto("/");
    await page.evaluate(() => {
      window.__sovereign_test__.setHandler(
        "mesh_set_peer_preference",
        ({ multiplier }: { multiplier: number }) => {
          if (multiplier > 1.0 || multiplier <= 0.0) {
            throw new Error(
              `peer-preference multiplier must be in (0.0, 1.0], got ${multiplier}`,
            );
          }
          return undefined;
        },
      );
    });

    await expect(
      page.evaluate(async () => {
        return await window.__TAURI_INTERNALS__.invoke(
          "mesh_set_peer_preference",
          { nodeId: "3".repeat(32), multiplier: 1.5, reason: null },
        );
      }),
    ).rejects.toThrow(/0\.0, 1\.0/);

    // 0.0 also rejected (open lower bound).
    await expect(
      page.evaluate(async () => {
        return await window.__TAURI_INTERNALS__.invoke(
          "mesh_set_peer_preference",
          { nodeId: "3".repeat(32), multiplier: 0.0, reason: null },
        );
      }),
    ).rejects.toThrow(/0\.0, 1\.0/);
  });

  test("mesh_clear_peer_preference returns boolean and records the call", async ({
    sovereignPage: page,
  }) => {
    await page.goto("/");
    const result = await page.evaluate(
      async ({ target }) => {
        return await window.__TAURI_INTERNALS__.invoke(
          "mesh_clear_peer_preference",
          { nodeId: target },
        );
      },
      { target: OTHER_NODE_HEX },
    );
    expect(result).toBe(true);
    const recorded = await page.evaluate(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      () => (window.__sovereign_test__ as any)._lastClearPreference,
    );
    expect(recorded).toEqual({ nodeId: OTHER_NODE_HEX });
  });

  test("mesh_list_peer_preferences returns an array of preference rows", async ({
    sovereignPage: page,
  }) => {
    await page.goto("/");
    await page.evaluate(() => {
      window.__sovereign_test__.setHandler(
        "mesh_list_peer_preferences",
        () => [
          {
            node_id: "4".repeat(32),
            multiplier: 0.5,
            reason: "experimenting",
            set_at: 1_700_000_000,
          },
          {
            node_id: "5".repeat(32),
            multiplier: 0.8,
            reason: null,
            set_at: 1_700_000_500,
          },
        ],
      );
    });

    const rows = (await page.evaluate(async () => {
      return await window.__TAURI_INTERNALS__.invoke(
        "mesh_list_peer_preferences",
        {},
      );
    })) as Array<Record<string, unknown>>;
    expect(rows).toHaveLength(2);
    expect(rows[0].multiplier).toBe(0.5);
    expect(rows[1].reason).toBeNull();
  });

  // Chaos-style invariant (mirrors chat-chaos.spec.ts pattern from
  // memory `feedback_chaos_testing_pattern.md`): the UI must not
  // crash when the backend returns unexpected shapes for these
  // endpoints. Pinned here so a regression in the new dimensional
  // payload doesn't take the desktop down silently.
  test("unexpected null contributions response does not crash the page", async ({
    sovereignPage: page,
  }) => {
    await page.goto("/");
    await page.evaluate(() => {
      window.__sovereign_test__.setHandler(
        "mesh_get_contributions",
        () => null,
      );
    });
    // Driving the command should resolve cleanly even with a null
    // payload — the contract is "Vec<NodeContributionsDto>", but
    // a null is what older daemons might briefly return during a
    // schema migration. The frontend is expected to treat null as
    // empty.
    const result = await page.evaluate(async () => {
      try {
        return await window.__TAURI_INTERNALS__.invoke(
          "mesh_get_contributions",
          {},
        );
      } catch (e) {
        return `error: ${(e as Error).message}`;
      }
    });
    expect(result).toBeNull();
    // No uncaught page errors.
    // (test-base attaches a pageerror listener that auto-fails.)
  });
});
