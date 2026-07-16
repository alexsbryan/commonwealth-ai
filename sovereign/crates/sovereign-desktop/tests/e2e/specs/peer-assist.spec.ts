// SPDX-License-Identifier: AGPL-3.0-or-later
// Peer-assisted ingest ("Blanket") — the standing renewable grant surface on
// the watched-folder detail panel. Drives the glassbox flow end to end against
// mocked Tauri commands: offer → pick peers → Get mesh help → progress panel,
// plus the two degrade paths the design promises (self-hide when no peer can
// help; revoke falls back to local-only).
//
// The local ingest is never modelled here — that's the point: the assist layer
// is purely additive, so these specs only exercise the mesh-help affordance.
import { test, expect, bootToChat } from "../fixtures/test-base";
import type { Page } from "@playwright/test";

const CORPUS = "watched-mock-assist";
const PEER_ID = "aaaa1111bbbb2222cccc3333dddd4444";

// Common watched-folder detail stubs (enrichment OFF; the Mesh-help section
// renders below it). `eligible`/`status` are layered per-test.
async function stubWatchedFolder(page: Page) {
  await page.evaluate(
    ({ corpus }) => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      const set = w.__sovereign_test__.setHandler;
      set("lc_watch_list", () => ({
        corpora: [
          {
            corpus_id: corpus,
            display_name: "Vault",
            root_path: "/tmp/vault",
            status: { kind: "idle", last_sweep_unix: 0, live_docs: 200, tombstones: 0 },
            sync_mode: "continuous",
            sensitive: false,
            additional_roots_count: 0,
          },
        ],
      }));
      set("lc_watch_details", () => ({
        corpus_id: corpus,
        display_name: "Vault",
        root_path: "/tmp/vault",
        status: { kind: "idle", last_sweep_unix: 0, live_docs: 200, tombstones: 0 },
        sync_mode: "continuous",
        sensitive: false,
        live_entries: 200,
        formats: { md: 200 },
        skipped_by_extension: {},
        failed_files: [],
        tombstones: 0,
        enrichment: { kind: "off" },
        last_sweep_unix: 0,
        roots: [
          { idx: 0, path: "/tmp/vault", added_at_unix: 0, doc_count: 200, primary: true },
        ],
      }));
      set("lc_watch_state", (args: unknown) => ({
        corpus_id: (args as { corpusId: string }).corpusId,
        status: { kind: "idle", last_sweep_unix: 0, live_docs: 200, tombstones: 0 },
        skipped_by_extension: {},
        failed_files: [],
        tombstones: 0,
        live_entries: 200,
      }));
      set("lc_enrichment_status", () => ({
        state: null,
        is_stalled: false,
        fraction_complete: 0,
      }));
    },
    { corpus: CORPUS },
  );
}

async function openDetail(page: Page) {
  await page.getByTestId("nav-library").click();
  await page.getByTestId("library-add").click();
  await page.getByTestId("add-section-files").click();
  await page
    .locator(".card")
    .filter({ hasText: "Vault" })
    .locator('button:has-text("Details")')
    .click();
}

test.describe("peer-assisted ingest (watched folder)", () => {
  test("offer → pick peers → Get mesh help → glassbox progress panel", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubWatchedFolder(page);

    await page.evaluate(
      ({ corpus, peerId }) => {
        const w = window as unknown as {
          __sovereign_test__: {
            setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
          };
          __assist_started__?: boolean;
        };
        const set = w.__sovereign_test__.setHandler;
        set("mesh_assist_eligible_peers", () => ({
          grantable: true,
          peers: [
            {
              node_id: peerId,
              name: "Studio",
              online: true,
              eligible: true,
              reason: "ok",
            },
          ],
        }));
        set("mesh_assist_start", () => {
          w.__assist_started__ = true;
          return {
            corpus_id: corpus,
            handoff_id: "h-1",
            grant_expires_at_ms: 9_999_999_999_000,
            peer_count: 1,
          };
        });
        // A stable running snapshot — non-terminal so the panel never prunes
        // mid-assertion.
        set("mesh_assist_status", () => ({
          handoff_id: "h-1",
          corpus_id: corpus,
          phase: "Draining",
          total_units: 10,
          complete: 4,
          failed: 0,
          leased: 2,
          queued: 4,
          per_peer: [{ node_id: peerId, leased: 2, completed: 4, failed: 0 }],
          ephemeral: true,
          grant: {
            expires_at_ms: 9_999_999_999_000,
            revoked: false,
            allowed_peers: [peerId],
          },
          verification: null,
        }));
      },
      { corpus: CORPUS, peerId: PEER_ID },
    );

    await openDetail(page);

    // Mesh-help section + offer probe surfaces the eligible peer.
    await expect(
      page.locator(".section-title", { hasText: "Mesh help" }),
    ).toBeVisible();
    const offerToggle = page.getByRole("button", {
      name: /speed this up with your mesh/i,
    });
    await expect(offerToggle).toBeVisible();

    // Expand → picker (Studio, checked) + guarantees.
    await offerToggle.click();
    await expect(page.getByText("Studio")).toBeVisible();
    await expect(page.getByText(/nothing is kept/i)).toBeVisible();

    // Kick off the assist.
    const startBtn = page.getByRole("button", { name: /get mesh help/i });
    await expect(startBtn).toBeVisible();
    await startBtn.click();

    // The grant was issued and the glassbox progress panel renders the
    // running snapshot with per-peer tallies + a stop affordance.
    await expect
      .poll(() => page.evaluate(() => (window as unknown as { __assist_started__?: boolean }).__assist_started__))
      .toBe(true);
    await expect(page.getByText(/1 peer helping/i)).toBeVisible();
    await expect(page.getByText(/4\/10 units/)).toBeVisible();
    await expect(
      page.getByRole("button", { name: /stop using peers/i }),
    ).toBeVisible();
  });

  test("no eligible peer → offer self-hides (local-only degrade)", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubWatchedFolder(page);

    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      // Grantable, but no peer is eligible → the offer renders nothing.
      w.__sovereign_test__.setHandler("mesh_assist_eligible_peers", () => ({
        grantable: true,
        peers: [],
      }));
    });

    await openDetail(page);

    // Section header + lede present, but the offer disclosure is absent —
    // graceful local-only degrade.
    await expect(
      page.locator(".section-title", { hasText: "Mesh help" }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: /speed this up with your mesh/i }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: /get mesh help/i }),
    ).toHaveCount(0);
  });

  test("revoke stops peer help and reverts to the offer (local-only)", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubWatchedFolder(page);

    await page.evaluate(
      ({ corpus, peerId }) => {
        const w = window as unknown as {
          __sovereign_test__: {
            setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
          };
          __assist_revoked__?: boolean;
        };
        const set = w.__sovereign_test__.setHandler;
        set("mesh_assist_eligible_peers", () => ({
          grantable: true,
          peers: [
            { node_id: peerId, name: "Studio", online: true, eligible: true, reason: "ok" },
          ],
        }));
        set("mesh_assist_start", () => ({
          corpus_id: corpus,
          handoff_id: "h-1",
          grant_expires_at_ms: 9_999_999_999_000,
          peer_count: 1,
        }));
        set("mesh_assist_status", () => ({
          handoff_id: "h-1",
          corpus_id: corpus,
          phase: "Draining",
          total_units: 10,
          complete: 4,
          failed: 0,
          leased: 2,
          queued: 4,
          per_peer: [{ node_id: peerId, leased: 2, completed: 4, failed: 0 }],
          ephemeral: true,
          grant: { expires_at_ms: 9_999_999_999_000, revoked: false, allowed_peers: [peerId] },
          verification: null,
        }));
        set("mesh_assist_revoke", () => {
          w.__assist_revoked__ = true;
          return null;
        });
      },
      { corpus: CORPUS, peerId: PEER_ID },
    );

    await openDetail(page);
    await page.getByRole("button", { name: /speed this up with your mesh/i }).click();
    await page.getByRole("button", { name: /get mesh help/i }).click();
    const stop = page.getByRole("button", { name: /stop using peers/i });
    await expect(stop).toBeVisible();
    await stop.click();

    // Revoke reached the daemon, and after the terminal flash the panel is
    // pruned — the offer reappears (peer help is fully off, local-only).
    await expect
      .poll(() => page.evaluate(() => (window as unknown as { __assist_revoked__?: boolean }).__assist_revoked__))
      .toBe(true);
    await expect(
      page.getByRole("button", { name: /speed this up with your mesh/i }),
    ).toBeVisible();
  });
});
