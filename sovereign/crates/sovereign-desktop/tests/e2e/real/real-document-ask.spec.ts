// SPDX-License-Identifier: AGPL-3.0-or-later
// Attach-document flow against the real DocumentAssetManager: upload a
// real text file through the production command, wait for the real
// indexing pipeline, ask a question, and verify the answer is grounded
// in the document (the book-report bench's flow, exercised through the
// desktop's own command surface — Phase 3 replays the full bench here).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, realBootToChat, test } from "./test-base-real";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_INFO = path.resolve(__dirname, "../../../test-artifacts/real-fixture.json");

test("upload a document, ask about it, get a grounded sourced answer", async ({
  sovereignPage: page,
  bridge,
}) => {
  test.setTimeout(300_000); // real indexing + skeleton build + RAG ask
  const fixture = JSON.parse(fs.readFileSync(FIXTURE_INFO, "utf8")) as {
    attach_doc: string;
  };

  await realBootToChat(page);

  const uploaded = await bridge.invoke<{ asset: { id: string; title?: string } }>(
    "upload_document_asset",
    { filePath: fixture.attach_doc },
  );
  const assetId = uploaded.asset.id;
  expect(assetId.length).toBeGreaterThan(0);

  // Wait for the real indexing pipeline to leave the pending state.
  await expect
    .poll(
      async () => {
        const asset = await bridge.invoke<unknown>("get_document_asset", { assetId });
        const state = JSON.stringify(asset).toLowerCase();
        if (state.includes("failed") || state.includes("error")) return "failed";
        return state.includes("ready") ? "ready" : "pending";
      },
      { timeout: 240_000, intervals: [2000, 5000] },
    )
    .toBe("ready");

  const conv = await bridge.invoke<{ id: string }>("create_conversation");
  const answer = await bridge.invoke<{ response: string; sources: string[] }>(
    "ask_document",
    {
      assetId,
      question:
        "How many violet anemone individuals were counted, and on which vessel did the expedition travel?",
      conversationId: conv.id,
    },
  );

  // Grounding floor: both facts are verbatim in the notes.
  expect(answer.response).toMatch(/312|Periwinkle/i);
  expect(answer.sources.length).toBeGreaterThan(0);

  // The asset is listed — the DocumentLibrary surface's data source.
  const assets = await bridge.invoke<Array<{ id: string }>>("list_document_assets");
  expect(assets.some((a) => a.id === assetId)).toBe(true);
});
