// SPDX-License-Identifier: AGPL-3.0-or-later
// B6 — Borrow a bigger brain from the mesh.
//
// The laptop is not the ceiling: a machine you trust lends you its GPU,
// the model's blocks split across both, and the app tells you exactly
// whose machine and which model. Sovereignty that scales past one
// device — with a receipt.
//
// This beat depends on ANOTHER PHYSICAL MACHINE being up. When it isn't,
// the beat skips with that stated. It is never faked, never re-cut from
// a previous run, and never quietly downgraded to "local only" footage
// narrated as if it were distributed — which would invert the one claim
// the beat exists to make.
import { beatTest, expect, demoClick } from "./beat";
import { realBootToChat } from "./demo-base";
import { meshOnline, placement } from "./preflight";

beatTest(
  {
    id: "b6-peer-compute",
    title: "A model too big for this laptop, split across the mesh",
    claim:
      "The laptop isn't the ceiling — a machine you trust lends its GPU, and the " +
      "app names whose machine and which model.",
    gifPadSec: 1.0,
    gifMark: "provenance-receipt",
  },
  async ({ page, bridge, run }) => {
    const mesh = await meshOnline();
    run.requireOrSkip(
      mesh.online >= 2,
      `only ${mesh.online}/${mesh.total} mesh members online on "${mesh.name}" — ` +
        "B6 needs a peer up to have anything to borrow. Bring the peer's daemon up " +
        "and re-shoot; do not film this beat local-only.",
    );

    const place = await placement(bridge);
    run.requireOrSkip(
      place !== null,
      "mesh_get_placement reports nothing resident — load the large model before capturing B6",
    );
    run.requireOrSkip(
      place!.mode === "distributed" && place!.workers.length > 0,
      `the primary slot is placed "${place!.mode ?? "unknown"}" with ` +
        `${place!.workers.length} workers. B6 films a DISTRIBUTED placement; a local-only ` +
        "slot has nothing to show. Bring the peer's rpc worker up so the blocks split.",
    );

    run.note(
      `placement: ${place!.modelId} — ${place!.mode}, ` +
        `${place!.blocksLocal}/${place!.blocksTotal} blocks local, ` +
        `workers: ${place!.workers.map((w) => `${w.endpoint}(${w.blocks})`).join(", ")}`,
    );
    run.note(`mesh "${mesh.name}": ${mesh.online}/${mesh.total} online`);

    await realBootToChat(page);
    await run.dwell(800);

    // ── The receipt surface ──
    await demoClick(page, page.getByTestId("nav-settings"), { settleMs: 500 });
    await demoClick(page, page.getByRole("button", { name: /^Mesh$/ }).first(), {
      settleMs: 600,
    });
    run.mark("mesh-settings");

    const placementEl = page.locator(".placement");
    await expect(
      placementEl,
      "the mesh diagnostics panel must render the shared-model placement",
    ).toBeVisible({ timeout: 30_000 });

    // ── Numeric honesty, again: the chip must say what the daemon says. ──
    const chip = placementEl.locator(".placement-chip.distributed");
    await expect(
      chip,
      "with a distributed placement the panel must show the distributed chip, not `local only`",
    ).toBeVisible({ timeout: 20_000 });
    await expect(
      chip,
      "the chip's block split must match mesh_get_placement, not a rounded-off story",
    ).toContainText(`${place!.blocksLocal}/${place!.blocksTotal} blocks local`);

    await expect(
      placementEl.locator(".placement-model"),
      "the panel must name the model that is actually placed",
    ).toContainText(place!.modelId ?? "");

    // The worker rows are the "whose machine" half of the receipt.
    const workers = placementEl.locator(".placement-worker");
    await expect(workers.first()).toBeVisible({ timeout: 20_000 });
    expect(
      await workers.count(),
      "every worker the daemon reports must be named on screen — a hidden peer is an " +
        "unaccountable one",
    ).toBe(place!.workers.length);

    run.mark("provenance-receipt");
    await run.caption(
      `${place!.blocksLocal} of ${place!.blocksTotal} blocks here. The rest, on a machine I trust.`,
      3600,
    );
    await run.park();
    await run.dwell(4000);

    // ── Now actually use it. ──
    await demoClick(page, page.getByTestId("nav-ask"), { settleMs: 500 });
    await expect(page.locator(".chat-view")).toBeVisible();
    await run.dwell(900);

    const facts = await run.turn(
      "Explain the difference between a model that is quantized and one that is " +
        "distributed across machines, and why only one of them costs you quality.",
      { charDelayMs: 26 },
    );
    run.note(
      `remote-assisted turn: ${facts.complete.full_text.length} chars, ` +
        `finish_reason=${facts.complete.metadata?.provenance?.finish_reason ?? "n/a"}`,
    );
    run.mark("answered");

    // Stream integrity is NOT relaxed because the compute moved — that is
    // the whole point. assertTurnInvariants already ran inside run.turn();
    // this re-read proves the split held for the duration of the turn
    // rather than collapsing to local mid-stream.
    const after = await placement(bridge);
    expect(
      after?.mode,
      "the placement must still be distributed after the turn — a mid-turn collapse to " +
        "local would make the footage a lie",
    ).toBe("distributed");

    await run.park();
    await run.dwell(2600);
  },
);
