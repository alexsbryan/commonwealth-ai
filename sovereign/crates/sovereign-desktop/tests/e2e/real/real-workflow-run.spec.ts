// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Real-stack proof of the desktop **Run a workflow** surface: pick the shipped
// `notebook` starter, point it at a folder of text files, and run it through the
// in-process Runner (fed the desktop's daemon-routed provider in attach mode).
// Asserts the per-step progress streams to the UI, the run completes, the corpus
// it built is searchable, and the "chat with it" handoff appears — VISION's
// author→use loop closed inside the app, no terminal.
//
// Heavy: a real embed over each file + a LanceDB index build. Generous timeout.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { expect, realBootToChat, test } from "./test-base-real";

test("real stack: run the notebook workflow over a folder from the desktop UI", async ({
  sovereignPage: page,
}) => {
  test.setTimeout(420_000);

  // A small folder of plain-text files for the notebook to ingest.
  const folder = fs.mkdtempSync(path.join(os.tmpdir(), "wf-run-"));
  fs.writeFileSync(
    path.join(folder, "kestrel.txt"),
    "The kestrel hovers over the meadow, holding station against the wind before it stoops.",
  );
  fs.writeFileSync(
    path.join(folder, "tide.txt"),
    "Spring tides arrive at the new and full moon, when sun and moon pull along the same line.",
  );
  const corpus = "e2e-notebook";

  await realBootToChat(page);

  // Enter the Run view: Workshop rail → Run tab (the standalone Run nav folded
  // into the Workshop in P0; ingest moves to Library → Add in P1).
  await page.getByTestId("nav-workshop").click();
  await page.getByTestId("workshop-tab-run").click();
  await expect(page.getByTestId("workflow-run-view")).toBeVisible();

  // Pick the shipped flagship starter explicitly (the list auto-selects the
  // first entry alphabetically, which isn't necessarily notebook).
  await page.getByTestId("workflow-pick-notebook").click();
  await expect(page.getByTestId("workflow-run-form")).toBeVisible();

  // Fill the folder (editable text input) + a known corpus name so the
  // completion assertion is stable.
  await page.getByTestId("workflow-folder-value").fill(folder);
  await page.getByTestId("workflow-param-corpus").fill(corpus);

  // Run, then watch it go — per-step progress streams onto the run panel.
  await page.getByTestId("workflow-run-button").click();
  await expect(page.getByTestId("workflow-run-progress")).toBeVisible();
  await expect(page.getByTestId("workflow-run-steps")).toBeVisible({ timeout: 60_000 });

  // Terminal: the run completes and offers to chat with the corpus it built.
  await expect(page.getByTestId("workflow-run-complete")).toBeVisible({
    timeout: 360_000,
  });
  const cta = page.getByTestId("workflow-chat-cta");
  await expect(cta).toBeVisible();
  await expect(cta).toContainText(corpus);

  // The handoff lands the user in chat over the freshly-built notebook.
  await cta.click();
  await expect(page.getByTestId("chat-input")).toBeVisible({ timeout: 30_000 });

  fs.rmSync(folder, { recursive: true, force: true });
});
