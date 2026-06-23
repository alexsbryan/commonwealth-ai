// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Dev-runnable accessibility insight report — `npm run a11y`.
//
// GLASSBOX, NON-BLOCKING by design. This script gives developers
// visibility into the app's accessibility shortcomings; it does NOT gate
// CI or fail the build. It always exits 0. The point is insight, not
// enforcement (see the a11y-pass plan).
//
// What it does:
//   1. Ensures a vite dev server is up (reuses an existing one on :5173,
//      otherwise spawns `npm run dev` and tears it down on exit).
//   2. Loads each key surface in headless Chromium and runs axe-core
//      against WCAG 2.1 A/AA.
//   3. Writes a readable report to test-artifacts/a11y/report.md (+ the
//      full machine-readable report.json) and prints a summary table.
//
// Coverage is honest about its edges (no silent caps): the surface list
// below is what gets scanned, each row records the STATE it was scanned
// in, and `ACCEPTED_RULES` is an annotated allow-list (shown in the
// report, never hidden). Deeper interactive states (post-turn chat,
// opened dialogs) are not yet driven here — that's a deliberate
// follow-up, called out in the report footer.
//
// Why a self-contained .mjs (not a Playwright @a11y project / a shared
// .ts helper): the user asked for a dev-runnable report, not a CI gate,
// and a plain ESM script can't import a .ts fixture at runtime. When the
// project later wants per-surface artifacts in the e2e suite, the scan
// logic here is the thing to extract into a shared helper.

import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const BASE = "http://localhost:5173";
const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
// tests/e2e/scripts → the sovereign-desktop crate root (where `npm run
// dev` and test-artifacts/ live).
const DESKTOP_ROOT = resolve(SCRIPT_DIR, "../../..");
const SHIM_PATH = resolve(SCRIPT_DIR, "../fixtures/tauri-shim.js");
const OUT_DIR = resolve(DESKTOP_ROOT, "test-artifacts/a11y");

// axe rule-set: WCAG 2.0 + 2.1, levels A and AA.
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

// Known-accepted rules. Each MUST carry a reason — this is an audit
// trail, NOT a silencer. Accepted violations still appear in the report,
// in their own section; they're just not counted as "flagged". Add an
// entry only with a tracking note for why it's tolerated for now.
const ACCEPTED_RULES = {
  // "color-contrast": "tracked in A11Y-xxx — lavender chips on dark bg",
};

// Permissive in-page host bridge so the static mesh-app bundles render
// past their loading gate without the real Tauri host. Returns benign
// empty data for ANY method a bundle calls — enough to exercise the
// static markup (landmarks, labels, contrast). The data-rich states are
// already covered by the per-bundle meshapp-*.spec.ts role-locator
// tests; this is about the static a11y layer.
function installPermissiveMeshAppBridge() {
  // Runs in-page as a classic init script before the bundle boots.
  window.meshApp = new Proxy(
    {},
    { get: () => async () => [] },
  );
}

// The surfaces we scan. `waitFor` is the selector that signals the
// surface has rendered; if it doesn't appear we still scan the current
// DOM and mark the row `degraded` so the report never overclaims.
// Drive the same backend-ready handshake `bootToChat` (test-base.ts)
// uses: App.svelte wires the backend-ready listener asynchronously, so
// we poll-emit the signal via the shim's `__sovereign_test__` control
// surface until the chat view mounts. Idempotent + cheap (in-page event).
async function bootMainAppToChat(page) {
  for (let i = 0; i < 50; i++) {
    try {
      await page.evaluate(() =>
        window.__sovereign_test__?.signalBackendReady?.(),
      );
    } catch {
      /* listener not wired yet — keep polling */
    }
    if ((await page.locator(".chat-view").count()) > 0) return;
    await page.waitForTimeout(150);
  }
}

const SURFACES = [
  {
    name: "Main app — chat surface",
    url: `${BASE}/`,
    initScriptPaths: [SHIM_PATH],
    prepare: bootMainAppToChat,
    waitFor: ".chat-view",
  },
  ...["enron", "lvt", "uap", "wrapped"].map((app) => ({
    name: `Mesh app bundle — ${app}`,
    url: `${BASE}/meshapp/${app}/index.html`,
    initFns: [installPermissiveMeshAppBridge],
    waitFor: "main, [role='main'], body",
  })),
];

const IMPACTS = ["critical", "serious", "moderate", "minor"];

async function serverIsUp() {
  try {
    const res = await fetch(BASE, { method: "GET" });
    return res.status < 500;
  } catch {
    return false;
  }
}

async function waitForServer(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await serverIsUp()) return true;
    await new Promise((r) => setTimeout(r, 500));
  }
  return false;
}

async function scanSurface(browser, surface) {
  const context = await browser.newContext();
  const page = await context.newPage();
  let state = "loaded";
  try {
    for (const p of surface.initScriptPaths ?? []) {
      await page.addInitScript({ path: p });
    }
    for (const fn of surface.initFns ?? []) {
      await page.addInitScript(fn);
    }
    await page.goto(surface.url, {
      waitUntil: "domcontentloaded",
      timeout: 20_000,
    });
    if (surface.prepare) await surface.prepare(page);
    try {
      await page.waitForSelector(surface.waitFor, { timeout: 8_000 });
    } catch {
      state = `degraded — "${surface.waitFor}" never appeared; scanned the DOM as-is`;
    }
    // Let any post-load rendering settle so axe sees a stable tree.
    await page.waitForTimeout(600);

    const axe = await new AxeBuilder({ page }).withTags(TAGS).analyze();
    const accepted = [];
    const flagged = [];
    for (const v of axe.violations) {
      (ACCEPTED_RULES[v.id] ? accepted : flagged).push(v);
    }
    return { surface: surface.name, url: surface.url, state, flagged, accepted };
  } catch (e) {
    return {
      surface: surface.name,
      url: surface.url,
      state: `error — ${e?.message ?? e}`,
      flagged: [],
      accepted: [],
    };
  } finally {
    await context.close();
  }
}

function countByImpact(violations) {
  const counts = { critical: 0, serious: 0, moderate: 0, minor: 0 };
  for (const v of violations) {
    const impact = v.impact ?? "minor";
    if (impact in counts) counts[impact] += v.nodes.length;
  }
  return counts;
}

function renderMarkdown(results, generatedAt) {
  const lines = [];
  lines.push("# Accessibility insight report");
  lines.push("");
  lines.push(
    "Generated by `npm run a11y` — NON-BLOCKING. This report informs; it does not gate the build.",
  );
  lines.push("");
  lines.push(`- Generated: ${generatedAt}`);
  lines.push(`- Ruleset: axe-core against ${TAGS.join(", ")}`);
  lines.push(`- Surfaces scanned: ${results.length}`);
  lines.push("");

  // Summary table.
  lines.push(
    "| Surface | State | Critical | Serious | Moderate | Minor | Accepted |",
  );
  lines.push("|---|---|---:|---:|---:|---:|---:|");
  for (const r of results) {
    const c = countByImpact(r.flagged);
    const acceptedNodes = r.accepted.reduce((n, v) => n + v.nodes.length, 0);
    // Keep the state cell terse so the table stays scannable.
    const stateCell = r.state.startsWith("loaded") ? "ok" : r.state;
    lines.push(
      `| ${r.surface} | ${stateCell} | ${c.critical} | ${c.serious} | ${c.moderate} | ${c.minor} | ${acceptedNodes} |`,
    );
  }
  lines.push("");

  // Per-surface detail.
  for (const r of results) {
    lines.push(`## ${r.surface}`);
    lines.push("");
    lines.push(`- URL: \`${r.url}\``);
    lines.push(`- State: ${r.state}`);
    lines.push("");
    if (r.flagged.length === 0) {
      lines.push("No flagged violations.");
    } else {
      lines.push("### Flagged");
      lines.push("");
      for (const v of r.flagged) {
        lines.push(
          `- **[${v.impact ?? "minor"}] ${v.id}** — ${v.help} (${v.nodes.length} node${v.nodes.length === 1 ? "" : "s"})`,
        );
        lines.push(`  - ${v.helpUrl}`);
        for (const node of v.nodes.slice(0, 5)) {
          lines.push(`  - \`${(node.target ?? []).join(" ")}\``);
        }
        if (v.nodes.length > 5) {
          lines.push(`  - …and ${v.nodes.length - 5} more (see report.json)`);
        }
      }
    }
    lines.push("");
    if (r.accepted.length > 0) {
      lines.push("### Accepted (known, tracked — not counted as flagged)");
      lines.push("");
      for (const v of r.accepted) {
        lines.push(
          `- **${v.id}** — ${v.help} (${v.nodes.length} node${v.nodes.length === 1 ? "" : "s"}). Reason: ${ACCEPTED_RULES[v.id]}`,
        );
      }
      lines.push("");
    }
  }

  lines.push("---");
  lines.push("");
  lines.push(
    "Coverage note: surfaces are scanned in their default-rendered state. " +
      "Deeper interactive states (a completed chat turn, an opened dialog, " +
      "the Settings panels) are not yet driven by this report — a deliberate " +
      "follow-up. Dynamic a11y behaviours (live-region announcements, focus " +
      "trap/restore) are not detectable by axe and are verified by manual " +
      "screen-reader testing instead.",
  );
  return lines.join("\n") + "\n";
}

async function main() {
  let spawnedServer = null;
  if (!(await serverIsUp())) {
    console.log("[a11y] starting dev server (npm run dev)…");
    spawnedServer = spawn("npm", ["run", "dev"], {
      cwd: DESKTOP_ROOT,
      stdio: "ignore",
      env: process.env,
    });
    const ok = await waitForServer(90_000);
    if (!ok) {
      console.error(
        "[a11y] dev server did not come up on :5173 within 90s; aborting (exit 0, non-blocking).",
      );
      spawnedServer.kill();
      process.exit(0);
    }
  } else {
    console.log("[a11y] reusing dev server already on :5173");
  }

  const browser = await chromium.launch();
  const results = [];
  try {
    for (const surface of SURFACES) {
      console.log(`[a11y] scanning ${surface.name} …`);
      results.push(await scanSurface(browser, surface));
    }
  } finally {
    await browser.close();
    if (spawnedServer) spawnedServer.kill();
  }

  const generatedAt = new Date().toISOString();
  await mkdir(OUT_DIR, { recursive: true });
  await writeFile(
    resolve(OUT_DIR, "report.json"),
    JSON.stringify({ generatedAt, tags: TAGS, results }, null, 2),
  );
  const md = renderMarkdown(results, generatedAt);
  await writeFile(resolve(OUT_DIR, "report.md"), md);

  // Console summary so the dev sees shortcomings without opening a file.
  console.log("");
  for (const r of results) {
    const c = countByImpact(r.flagged);
    const total = c.critical + c.serious + c.moderate + c.minor;
    console.log(
      `  ${r.surface}: ${total} flagged (` +
        `${c.critical} critical, ${c.serious} serious, ${c.moderate} moderate, ${c.minor} minor)` +
        (r.state.startsWith("loaded") ? "" : `  [${r.state}]`),
    );
  }
  console.log("");
  console.log(`[a11y] full report → ${resolve(OUT_DIR, "report.md")}`);
  // Always non-blocking.
  process.exit(0);
}

main().catch((e) => {
  // Even on an unexpected failure we stay non-blocking — print and exit 0.
  console.error("[a11y] report run failed:", e);
  process.exit(0);
});
