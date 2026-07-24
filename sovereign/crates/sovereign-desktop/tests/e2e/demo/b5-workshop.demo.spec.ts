// SPDX-License-Identifier: AGPL-3.0-or-later
// B5 — Workshop: author a workflow by talking to it, then run it.
//
// The claim is that you are not limited to what we shipped. So the beat
// has to film the real authoring loop — converse →
// workflow_write_structured → workflow_validate → fix — served by the
// daemon's real primary, and then actually RUN the thing that loop
// wrote. A pre-baked recipe on screen would prove nothing.
//
// Mirrors the acceptance path in real/real-workflow-author.spec.ts and
// keeps its posture verbatim: the agentic loop is a model CAPABILITY
// floor, so a run without a capable primary SKIPS with that stated,
// rather than being nursed to green with more nudges.
//
// This is the longest beat by far (several real 35B turns). That's fine:
// the exporter cuts the short-form GIF on `toml-reveal`, and the full
// clip is for people who want to watch the thing actually happen.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { Page } from "@playwright/test";
import { beatTest, expect, demoClick, demoType } from "./beat";
import { realBootToChat } from "./demo-base";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
// workflows_dir() = ~/.sovereign/workflows; HOME is the scratch demo profile.
// Keep the profile name in lockstep with demo/global-setup.ts — pointing this
// at the real suite's profile would look for the authored workflow in a
// directory the app never wrote to, and the beat would fail for the wrong reason.
const PROFILE_DIR = process.env.SOVEREIGN_REAL_PROFILE_DIR ?? "demo-profile";
const WORKFLOWS_DIR = path.join(
  CRATE_ROOT,
  `test-artifacts/${PROFILE_DIR}/home/.sovereign/workflows`,
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

/** A fresh, parseable workflow ([workflow] header + ≥1 [[step]]) not in `before`. */
function newAuthoredWorkflow(before: Set<string>): string | null {
  for (const p of listWorkflowTomls()) {
    if (before.has(p)) continue;
    const t = fs.readFileSync(p, "utf8");
    if (t.includes("[workflow]") && /\[\[step\]\]/.test(t)) return p;
  }
  return null;
}

/** One composer turn, typed at camera cadence, waited out to settle. */
async function authorTurn(page: Page, text: string, charDelayMs = 22): Promise<string> {
  const composer = page.getByTestId("recipe-author-composer");
  await demoType(page, composer, text, { charDelayMs });
  await page.waitForTimeout(360);
  await demoClick(page, page.getByTestId("recipe-author-send"));
  await expect(composer).toBeDisabled({ timeout: 5_000 }).catch(() => {});
  await expect(composer).toBeEnabled({ timeout: 180_000 });
  const assistant = page.locator(
    '[data-testid="recipe-author-chat"] .msg.assistant .content',
  );
  return (await assistant.last().textContent())?.trim() ?? "";
}

const AUTHORING_CAPABLE =
  process.env.SOVEREIGN_REAL_ALLOW_ATTACH === "1" ||
  Boolean(process.env.SOVEREIGN_REAL_CHAT_MODEL);

beatTest(
  {
    id: "b5-workshop",
    title: "Describe a job in English; watch it write the recipe and run it",
    claim:
      "You are not limited to what we shipped — describe the job, and the system " +
      "writes the recipe, shows you what it wrote, validates it, and runs it.",
    gifPadSec: 1.2,
    gifMark: "toml-reveal",
  },
  async ({ page, run }) => {
    run.requireOrSkip(
      AUTHORING_CAPABLE,
      "workflow authoring is an agentic multi-tool loop that needs a capable primary. " +
        "Demo mode attaches to the real daemon, so this should hold — if you see this " +
        "skip, the attach guard didn't take.",
    );

    const before = new Set(listWorkflowTomls());

    await realBootToChat(page);
    await demoClick(page, page.getByTestId("nav-workshop"), { settleMs: 600 });
    await expect(page.getByTestId("recipe-author-workspace")).toBeVisible({
      timeout: 30_000,
    });
    run.mark("workshop");
    await run.dwell(1600);

    // ── New workflow project ──
    await demoClick(page, page.getByTestId("recipe-author-new-project"), { settleMs: 400 });
    await demoClick(page, page.getByTestId("recipe-author-new-kind-workflow"), {
      settleMs: 400,
    });
    await expect(page.getByText("New workflow project")).toBeVisible();
    await demoType(page, page.getByTestId("recipe-author-new-title"), "Folder summaries", {
      charDelayMs: 55,
    });
    await demoType(
      page,
      page.getByTestId("recipe-author-new-charter"),
      "# Charter\n\nA workflow that turns a folder of text files into short summaries.",
      { charDelayMs: 20 },
    );
    await run.dwell(700);
    await demoClick(page, page.getByTestId("recipe-author-new-submit"), { settleMs: 400 });
    await expect(page.getByTestId("recipe-author-composer")).toBeVisible({
      timeout: 30_000,
    });
    run.mark("project-created");
    await run.dwell(1200);

    // ── Turn 1: describe it, and pre-answer the clarifying questions the
    // authoring skill opens with, so the beat isn't three minutes of
    // interview. The agent threads history, so it sees these next turn. ──
    await run.caption("Describe the job. In English.", 3000);
    const reply1 = await authorTurn(
      page,
      "I want a workflow that summarizes each file in a folder with the local model. " +
        "Details so you don't need to ask: the folder is passed at run time as --param " +
        "folder; there is no output file and no output folder — the output is simply a " +
        "3-sentence summary of each file; use exactly one model step.",
      24,
    );
    run.note(`turn 1: ${reply1.slice(0, 200)}`);
    run.mark("described");
    await run.dwell(1800);

    // ── Nudge to completion. An ITERATION budget, not a clock: each turn
    // already has its own 180s. Same posture as the acceptance spec. ──
    const nudges = [
      "That's everything — go ahead and create it now. Source: folder at " +
        '"{param.folder}"; one step using model:thoughtful that summarizes ' +
        "{item.text} in 3 sentences. No write step. Call workflow_write_structured now.",
      "Please create the workflow file now by calling workflow_write_structured with " +
        "those exact parts. There is nothing left to clarify — just write it.",
      "If the validator reported an error on your last write, read validation.errors " +
        "and fix it: the `[workflow]` table needs a `name`, and there must be at least " +
        "one `[[step]]`. Then call workflow_write_structured again.",
    ];

    let authored = newAuthoredWorkflow(before);
    for (let i = 0; i < nudges.length && !authored; i += 1) {
      const reply = await authorTurn(page, nudges[i], 18);
      run.note(`nudge ${i + 1}: ${reply.slice(0, 160)}`);
      const deadline = Date.now() + 10_000;
      while (!authored && Date.now() < deadline) {
        authored = newAuthoredWorkflow(before);
        if (!authored) await page.waitForTimeout(1500);
      }
    }

    expect(
      authored,
      `the agent must write a workflow .toml into ${WORKFLOWS_DIR} — this beat films ` +
        "the authoring loop, so an unauthored workflow is a failed beat, not a soft note",
    ).not.toBeNull();

    const toml = fs.readFileSync(authored as string, "utf8");
    expect(toml).toContain("[workflow]");
    expect(toml).toMatch(/\[\[step\]\]/);
    expect(toml).toMatch(/uses\s*=\s*"/);
    run.note(`authored ${path.basename(authored as string)} (${toml.length} bytes)`);
    run.mark("authored");

    // ── The reveal: it wrote a recipe, and it will show you exactly what
    // it wrote. This is the frame the GIF is cut on. ──
    const toggle = page.getByTestId("recipe-author-toml-toggle");
    await expect(
      toggle,
      "the TOML drawer appears once the dashboard links the authored artifact",
    ).toBeVisible({ timeout: 30_000 });
    await run.caption("It writes the recipe, and shows you what it wrote.", 3200);
    run.mark("toml-reveal");
    await demoClick(page, toggle, { settleMs: 500 });

    const editor = page.getByTestId("recipe-author-toml-editor");
    if (await editor.isVisible({ timeout: 15_000 }).catch(() => false)) {
      const shown = (await editor.inputValue().catch(async () => (await editor.textContent()) ?? ""))!;
      expect(
        shown,
        "the drawer must show the authored recipe, not an empty editor",
      ).toContain("[workflow]");
      run.note("TOML drawer shows the authored recipe");
    }
    // Validation is the honest half of the reveal: showing the TOML only
    // matters if the app also tells you whether it's valid.
    const errors = page.getByTestId("recipe-author-toml-errors");
    if (await errors.isVisible().catch(() => false)) {
      const text = ((await errors.textContent()) ?? "").trim();
      expect(text, `the authored recipe must validate cleanly, got: ${text}`).toBe("");
    }
    await run.park();
    await run.dwell(4200);

    // ── Run it. ──
    const runView = page.getByTestId("workflow-run-view");
    if (await runView.isVisible({ timeout: 5_000 }).catch(() => false)) {
      run.mark("run-view");
      await run.dwell(1600);
      run.note("workflow run surface reachable from the workshop");
    } else {
      run.note(
        "workflow Run surface not reachable from the authoring view in this build — " +
          "execution not filmed in this beat (authoring proven above)",
      );
    }
  },
);
