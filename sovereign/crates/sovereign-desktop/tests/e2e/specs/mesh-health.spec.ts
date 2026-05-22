import { test, expect, bootToChat, type Page } from "../fixtures/test-base";

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

// ─── UI-rendering coverage ─────────────────────────────────────────
//
// The tests above pin the command-surface contract; these mount the
// real Svelte tree (Settings → Mesh tab → MeshSettings) and assert
// the operator sees the dimensional payload — three blocks per peer,
// no collapsed score — and that the affinity slider dispatches the
// right Tauri call. This is the user-visible regression net for
// commit 4b.

const SELF_NODE_HEX = "11".repeat(16);
const PEER_NODE_HEX = "22".repeat(16);

interface MeshHealthFixture {
  members: Array<{
    name: string;
    node_id: string;
    is_self: boolean;
    status: "online" | "offline" | "busy" | "away";
  }>;
  contributions: Array<Record<string, unknown>>;
  preferences: Array<Record<string, unknown>>;
}

/** Stub the mesh-side Tauri commands so the Mesh tab renders one
 *  self-row + one peer-row with dimensional contributions. Mounting
 *  the panel off the chat view is the cheapest way to exercise the
 *  real Svelte template — there's no isolated MeshSettings story. */
async function primeMeshFixture(
  page: Page,
  fixture: MeshHealthFixture,
): Promise<void> {
  await page.evaluate((f) => {
    window.__sovereign_test__.setHandler("mesh_is_running", () => true);
    window.__sovereign_test__.setHandler("mesh_get_state", () => ({
      status: {
        name: "Test Mesh",
        members_online: f.members.length,
        members_total: f.members.length,
        model_name: null,
        knowledge_corpora: [],
        is_connected: true,
        join_link: null,
        join_key: null,
      },
      members: f.members.map((m) => ({
        ...m,
        contribution_level: 0,
        contribution_label: "",
      })),
      corpora: [],
      contribution: null,
    }));
    window.__sovereign_test__.setHandler(
      "mesh_get_contributions",
      () => f.contributions,
    );
    window.__sovereign_test__.setHandler(
      "mesh_list_peer_preferences",
      () => f.preferences,
    );
  }, fixture);
}

/** Boot to the chat surface (so the conversation toolbar with the
 *  settings button mounts), prime the mesh-health fixtures so the
 *  Tab body sees a "running" mesh, then click into Settings → Mesh.
 *
 *  Order matters: `bootToChat` calls `page.goto("/")`, which resets
 *  the shim. Priming after that ensures the handlers are in place by
 *  the time the user clicks the Mesh tab and `MeshSettings` mounts. */
async function openMeshTab(
  page: Page,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  chat: any,
  fixture: MeshHealthFixture,
): Promise<void> {
  await bootToChat(page, chat);
  await primeMeshFixture(page, fixture);
  await page.getByTestId("nav-settings").click();
  // Click the Mesh nav item by its visible label.
  await page.getByRole("button", { name: /^Mesh$/ }).click();
  // The members card renders only when meshIsRunning resolves true,
  // which is on the next tick.
  await page.locator(".members-card").waitFor();
}

test.describe("mesh health UI: dimensional rendering + affinity slider", () => {
  test("each peer row renders three contribution blocks with no collapsed score", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openMeshTab(page, chat, {
      members: [
        { name: "you", node_id: SELF_NODE_HEX, is_self: true, status: "online" },
        { name: "peer", node_id: PEER_NODE_HEX, is_self: false, status: "online" },
      ],
      contributions: [
        {
          node_id: PEER_NODE_HEX,
          window_days: 30,
          inference_served_requests: 12,
          inference_served_tokens: 4_800,
          inference_served_wall_seconds: 31.5,
          inference_consumed_requests: 3,
          inference_consumed_tokens: 1_200,
          corpora_hosted: [
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
      ],
      preferences: [],
    });

    const peerRow = page.locator(`.member-row[data-node-id="${PEER_NODE_HEX}"]`);
    await expect(peerRow).toBeVisible();
    // Three labelled blocks, in order, no totals or aggregate score.
    const dts = peerRow.locator(".contribution-block dt");
    await expect(dts).toHaveText(["Inference", "Knowledge", "Network"]);
    // Inference numbers landed in the right block.
    const inference = peerRow.locator(".contribution-block").nth(0);
    await expect(inference).toContainText("12");
    await expect(inference).toContainText("served");
    await expect(inference).toContainText("3");
    await expect(inference).toContainText("consumed");
    // Knowledge surface includes the sole-host annotation that lets
    // an operator see "this peer is the only one hosting sep" — the
    // social-pressure surface from the spec.
    const knowledge = peerRow.locator(".contribution-block").nth(1);
    await expect(knowledge).toContainText("sep");
    await expect(knowledge.locator(".corpus-host-sole")).toHaveText(/sole host/);
    // Network shows bytes-served vs bytes-received separately.
    const network = peerRow.locator(".contribution-block").nth(2);
    await expect(network).toContainText("served");
    await expect(network).toContainText("received");
  });

  test("affinity slider sends the chosen multiplier to mesh_set_peer_preference", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openMeshTab(page, chat, {
      members: [
        { name: "you", node_id: SELF_NODE_HEX, is_self: true, status: "online" },
        { name: "peer", node_id: PEER_NODE_HEX, is_self: false, status: "online" },
      ],
      contributions: [],
      preferences: [],
    });

    // Self-row must NOT carry a preference control — operators don't
    // "ration" themselves. Pinned because dropping the `is_self`
    // guard is a plausible regression.
    const selfRow = page.locator(`.member-row[data-node-id="${SELF_NODE_HEX}"]`);
    await expect(selfRow.locator(".member-preference")).toHaveCount(0);

    const peerRow = page.locator(`.member-row[data-node-id="${PEER_NODE_HEX}"]`);
    await peerRow.locator(".member-preference > summary").click();
    // Default draft is 1.0 (=100% neutral). Drag the slider to ~80%.
    // Slider min=0.05, max=1.0, step=0.05. We use input.fill() — Svelte's
    // bind:value picks up the change event.
    const slider = peerRow.locator('input[type="range"][aria-label="affinity multiplier"]');
    await slider.evaluate((el: HTMLInputElement) => {
      el.value = "0.8";
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
    });
    // Optional reason; mirrors the field a real operator would type.
    await peerRow
      .locator('input[type="text"]')
      .fill("over-consuming relative to its hardware");
    await peerRow.getByRole("button", { name: /^Save$/ }).click();

    // The shim records the last set call.
    const recorded = await page.evaluate(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      () => (window.__sovereign_test__ as any)._lastSetPreference,
    );
    expect(recorded).toEqual({
      nodeId: PEER_NODE_HEX,
      multiplier: 0.8,
      reason: "over-consuming relative to its hardware",
    });
  });

  test("a phantom contribution row for an unknown peer does not crash the UI", async ({
    sovereignPage: page,
    chat,
  }) => {
    // Chaos invariant: gossip can deliver contribution events for a
    // node the local mesh roster doesn't know about (e.g. peer who
    // joined and left between two ticks). The UI must tolerate this
    // — the dimensional Map is keyed by node_id, so an extra entry
    // just goes unused. Test pins that we don't iterate the Map
    // looking for orphans (which would either crash or render a
    // ghost row).
    await openMeshTab(page, chat, {
      members: [
        { name: "you", node_id: SELF_NODE_HEX, is_self: true, status: "online" },
      ],
      contributions: [
        {
          node_id: "deadbeef".repeat(4),
          window_days: 30,
          inference_served_requests: 1,
          inference_served_tokens: 1,
          inference_served_wall_seconds: 1,
          inference_consumed_requests: 0,
          inference_consumed_tokens: 0,
          corpora_hosted: [],
          bytes_served: 0,
          bytes_received: 0,
        },
      ],
      preferences: [],
    });

    // Exactly one rendered row (self), no ghost row for the phantom.
    await expect(page.locator(".member-row")).toHaveCount(1);
    await expect(
      page.locator(`.member-row[data-node-id="${SELF_NODE_HEX}"]`),
    ).toBeVisible();
    // pageerror listener in test-base would fail the test if a render
    // exception fired.
  });
});
