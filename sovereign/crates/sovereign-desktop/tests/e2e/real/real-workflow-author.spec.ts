// SPDX-License-Identifier: AGPL-3.0-or-later
// Real-mode: generate a WORKFLOW through the actual desktop UI + a real model.
//
// Drives the recipe-author workspace's workflow-author path end to end against
// the REAL sovereign-desktop process: open the workspace, create a Workflow-kind
// project, and converse with the agent until the REAL authoring loop
// (workflow_write_structured -> workflow_validate, inference served by the
// daemon's primary slot in attach mode) composes a workflow.
//
// The authoring skill opens turn 1 with a framing/clarifying reply ("no draft
// yet unless they said go"), so this drives it like a real user would: describe,
// then say "go". The proof is the artifact itself — a valid <id>.toml landing in
// the hermetic ~/.sovereign/workflows/, written by the model during the chat.
//
// Run: SOVEREIGN_REAL_ALLOW_ATTACH=1 npm run test:e2e:real -- real-workflow-author
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, realBootToChat, test } from "./test-base-real";
import type { Page } from "@playwright/test";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
// workflows_dir() = ~/.sovereign/workflows; HOME is the scratch profile.
const WORKFLOWS_DIR = path.join(
  CRATE_ROOT,
  "test-artifacts/real-profile/home/.sovereign/workflows",
);

function listWorkflowTomls(): string[] {
  try {
    return fs
      .readdirSync(WORKFLOWS_DIR)
      .filter((f) => f.endsWith(".toml"))
      .map((f) => path.join(WORKFLOWS_DIR, f));
  } catch {
    return [];
  }
}

/** A fresh, parseable workflow (a [workflow] header + ≥1 [[step]]) not in `before`. */
function newAuthoredWorkflow(before: Set<string>): string | null {
  for (const p of listWorkflowTomls()) {
    if (before.has(p)) continue;
    const t = fs.readFileSync(p, "utf8");
    if (t.includes("[workflow]") && /\[\[step\]\]/.test(t)) return p;
  }
  return null;
}

/** Send one composer turn and wait for the agent's reply to settle (the composer
 *  re-enables when `sending` flips false on message-complete). Returns the last
 *  assistant message text for visibility into the conversation. */
async function sendAndSettle(page: Page, text: string): Promise<string> {
  const composer = page.getByTestId("recipe-author-composer");
  await composer.fill(text);
  await page.getByTestId("recipe-author-send").click();
  // `sending` disables the composer for the turn; wait it out (one real 35B
  // turn with reasoning can be ~90s).
  await expect(composer).toBeDisabled({ timeout: 5_000 }).catch(() => {});
  await expect(composer).toBeEnabled({ timeout: 150_000 });
  const assistant = page.locator(
    '[data-testid="recipe-author-chat"] .msg.assistant .content',
  );
  return (await assistant.last().textContent())?.trim() ?? "";
}

// Workflow authoring drives an agentic multi-tool loop (converse →
// workflow_write_structured → workflow_validate → fix). That is a genuine model
// CAPABILITY floor, not a timing one: the default managed 2B never emits the tool
// call at all, and a dense 4B does but inconsistently (measured: a valid workflow
// one run, an empty `[workflow]` the next) even with 150s/turn and the expanded
// nudge budget below. So this spec requires a capable primary — attach mode (the
// operator's real daemon runs a 9B+, which authors reliably) or an explicit
// SOVEREIGN_REAL_CHAT_MODEL override pointing at a capable GGUF. Under the default
// managed 2B it SKIPS (not fails): the small model can't drive the loop, and that
// is an honest capability statement, not a draconian gate.
const AUTHORING_CAPABLE =
  process.env.SOVEREIGN_REAL_ALLOW_ATTACH === "1" ||
  Boolean(process.env.SOVEREIGN_REAL_CHAT_MODEL);

test("real stack: author a workflow through the desktop UI with a real model", async ({
  sovereignPage: page,
}) => {
  test.skip(
    !AUTHORING_CAPABLE,
    "workflow authoring needs a capable primary the default managed 2B can't " +
      "provide; run in attach mode (SOVEREIGN_REAL_ALLOW_ATTACH=1) against a 9B+ " +
      "daemon, or set SOVEREIGN_REAL_CHAT_MODEL=<capable.gguf>.",
  );
  // Real inference over several conversational turns — generous budget.
  test.setTimeout(420_000);

  const before = new Set(listWorkflowTomls());

  await realBootToChat(page);

  // Enter the authoring workspace (nav gated by enable_recipe_authoring, baked
  // true in the real profile).
  await page.getByTestId("nav-workshop").click();
  await expect(page.getByTestId("recipe-author-workspace")).toBeVisible();

  // New project, switched to the Workflow kind.
  await page.getByTestId("recipe-author-new-project").click();
  await page.getByTestId("recipe-author-new-kind-workflow").click();
  await expect(page.getByText("New workflow project")).toBeVisible();
  await page.getByTestId("recipe-author-new-title").fill("Folder summaries");
  await page
    .getByTestId("recipe-author-new-charter")
    .fill(
      "# Charter\n\nA workflow that turns a folder of text files into short summaries.",
    );
  await page.getByTestId("recipe-author-new-submit").click();
  await expect(page.getByTestId("recipe-author-composer")).toBeVisible();

  // Turn 1 — describe the workflow AND preempt the clarifying questions the
  // skill opens with (output location / format), so the only thing left is "go".
  // The agent loop threads history, so it sees these answers on the next turn.
  const reply1 = await sendAndSettle(
    page,
    "I want a workflow that summarizes each file in a folder with the local model. " +
      "Details so you don't need to ask anything: the folder is passed at run time " +
      "as --param folder; there is NO output file and NO output folder — the " +
      "workflow's output is simply the 3-sentence summary of each file; use exactly " +
      "one model step (no write step).",
  );
  console.log(`[workflow-author real] turn 1 reply:\n${reply1}\n`);

  // Turns 2..N — answer "go" plainly; everything the agent needs is already on
  // the transcript. A capable model composes the workflow in a turn or two; a
  // smaller one may write something the validator rejects (e.g. an empty
  // `[workflow]` table) and needs a couple of reasonable retries to self-correct
  // off `validation.errors` — the write tool nudges exactly that ("call
  // workflow_write_structured AGAIN in this same turn"). This is an iteration
  // budget, not a hard clock: each turn already has 150s (below), so the extra
  // nudges give capability room to show, without a draconian gate.
  const nudges = [
    "That's everything — go ahead and create it now. Source: folder at " +
      "\"{param.folder}\"; one step using model:thoughtful that summarizes " +
      "{item.text} in 3 sentences. No write step. Call workflow_write_structured now.",
    "Please create the workflow file now by calling workflow_write_structured " +
      "with those exact parts. There is nothing left to clarify — just write it.",
    "If the validator reported an error on your last write, read " +
      "validation.errors and fix it: the `[workflow]` table needs a `name`, and " +
      "there must be at least one `[[step]]`. Then call workflow_write_structured " +
      "again with the corrected workflow.",
    "Write the complete workflow now in one call: a workflow table with a name, a " +
      "folder source at \"{param.folder}\", and one model:thoughtful step that " +
      "summarizes {item.text} in 3 sentences. Call workflow_write_structured with " +
      "all of it.",
  ];

  let authored = newAuthoredWorkflow(before);
  for (let i = 0; i < nudges.length && !authored; i++) {
    const reply = await sendAndSettle(page, nudges[i]);
    console.log(`[workflow-author real] nudge ${i + 1} reply:\n${reply}\n`);
    // The file may land mid-turn; poll briefly after the turn settles too.
    const deadline = Date.now() + 10_000;
    while (!authored && Date.now() < deadline) {
      authored = newAuthoredWorkflow(before);
      if (!authored) await page.waitForTimeout(1500);
    }
  }

  expect(
    authored,
    `the agent should have written a workflow .toml into ${WORKFLOWS_DIR}`,
  ).not.toBeNull();

  const toml = fs.readFileSync(authored as string, "utf8");
  console.log(
    `[workflow-author real] AUTHORED ${path.basename(authored as string)}:\n${toml}`,
  );
  expect(toml).toContain("[workflow]");
  expect(toml).toMatch(/\[\[step\]\]/);
  expect(toml).toMatch(/uses\s*=\s*"/);

  // The DASHBOARD now reflects the authored workflow: the chat surface links the
  // freshly-written artifact onto the project on turn-complete, and the 2s
  // dashboard poll surfaces it. The TOML drawer's "Show TOML" toggle only renders
  // once an artifact is linked — its presence proves the dashboard-display fix.
  await expect(page.getByTestId("recipe-author-toml-toggle")).toBeVisible({
    timeout: 20_000,
  });
});
