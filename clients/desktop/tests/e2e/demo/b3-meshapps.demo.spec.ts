// SPDX-License-Identifier: AGPL-3.0-or-later
// B3 — Mesh Apps: the Enron task force, and today's news.
//
// These are RAW beats: the gate below runs in the suite, the footage is
// recorded by hand. That is not a convenience. A mesh app only sees real
// data from inside a window labelled `meshapp-<app_id>`:
//
//   meshapp.rs `authorize()` derives the calling app from the webview
//   label and is fail-closed on anything else, and command_bridge.rs
//   always invokes as MAIN_WINDOW_LABEL ("main"). So every host op a
//   bundle makes through the test bridge is denied — including from a
//   page Playwright navigated to `/meshapp/enron/index.html` with the
//   shipped shim injected, because the shim's only transport IS that
//   bridge.
//
// The earlier version of this beat did exactly that and filmed the
// bundle in Chromium. It could never have shown a real number; it only
// ever skipped at a preflight that used the equally-gated
// `meshapp_corpus_stats` and read the denial as "no atlas". Filming the
// bundle with mocked host data would have been worse: a clip whose whole
// claim is "every pixel came from the daemon", where none of it did.
//
// The other direction — teaching the test bridge to assert a mesh-app
// label — was considered and rejected. That label IS the security
// boundary (`capabilities/meshapp.json` scopes the bridge commands to
// `windows: ["meshapp-*"]`); a test-only escape hatch through it is a
// production-shaped hole that exists so a demo can be convenient.
//
// So: the machine proves the claim, the human shoots the window.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { rawBeatTest, expect } from "./beat";
import { atlasBuilt, corpusEntry, hasCorpus, meshAppInstall } from "./preflight";

const fmt = (n: number) => Number(n).toLocaleString("en-US");

/** Where the operator's real index for `corpusId` lives. Attach mode
 *  films the operator's live daemon, so this is their real path. */
const indexDir = (corpusId: string) =>
  path.join(process.env.SOVEREIGN_INDEX_DIR ?? path.join(os.homedir(), ".sovereign/indexes"), corpusId);

/** Ask the host to open the app's real window. This is the one check the
 *  Chromium route could never make: `meshapp_open` refuses an app with
 *  no install record, and the window it builds is the labelled,
 *  CSP-clamped webview whose ops actually resolve. If this returns, the
 *  surface the operator is about to record is authorized for real. */
async function openRealWindow(
  bridge: { invoke<T = unknown>(c: string, a?: Record<string, unknown>): Promise<T> },
  appId: string,
): Promise<string | null> {
  try {
    await bridge.invoke("meshapp_open", { appId });
    return null;
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
}

// ─────────────────────────────────────────────────────────────────────
// B3a — Enron
// ─────────────────────────────────────────────────────────────────────
rawBeatTest(
  {
    id: "b3-enron",
    capture: "raw",
    title: "A task force over 3,722 emails nobody read",
    claim:
      "A corpus is a substrate: same data, purpose-built lens, and every pixel " +
      "dereferences to the document it came from.",
    gifPadSec: 1.0,
    recordingGuide: [
      "Open the real app (not the harness) and launch the Enron mesh app from the Library.",
      "Record the mesh-app window only, sized 1280×800 — anything else gets letterboxed.",
      "Beats to hit, in order: the scale banner settles · the graph comes to rest · " +
        "open one entity · scroll to the reconciliation merges · open a timeline column · " +
        "drill through to a source email.",
      "Check the banner against the atlas counts this gate printed before you keep the take.",
    ],
    script: [
      { text: "3,722 emails. Nobody read them.", holdMs: 3000 },
      { text: "Same company, three spellings. Reconciled.", holdMs: 3200 },
      { text: "Down to the source email.", holdMs: 2800 },
    ],
  },
  async ({ bridge, run }) => {
    const CORPUS = "enron-sample-multi-wide";
    const APP = "enron";

    // ── 1. the corpus is hosted ──
    run.requireOrSkip(await hasCorpus(CORPUS), `the \`${CORPUS}\` corpus is not hosted`);

    // ── 2. it has a BUILT ATLAS — mesh-app ops read the atlas, not chunks ──
    const atlas = await atlasBuilt(bridge, CORPUS);
    run.requireOrSkip(
      atlas !== null,
      `\`${CORPUS}\` has chunks but no built atlas. Mesh-app ops read the atlas. ` +
        `Build it with \`sovereign enrich init ${CORPUS}\` then \`sovereign enrich build ${CORPUS}\`. ` +
        `NOTE: a watched-folder ingest runs the folder_tiered (RAPTOR) pipeline, which ` +
        `completes without building an atom graph — "it was enriched" and "it has an atlas" ` +
        `are different facts.`,
    );

    // ── 3. the app is installed AND granted the read permission ──
    const install = await meshAppInstall(bridge, APP);
    run.requireOrSkip(
      install !== null,
      `the \`${APP}\` mesh app is not installed. It ships in public/meshapp/ but is not in ` +
        `your desktop.toml [[meshapp_installs]] (uap/lvt/wrapped/explorer/federalist/today are). ` +
        `Install it from the Library and grant it mesh_store_read — the consent sheet is itself ` +
        `a good on-camera gesture. The demo harness mirrors your host grants, so installing it ` +
        `in the real app is what makes this gate pass.`,
    );
    run.requireOrSkip(
      install!.granted?.mesh_store_read === true,
      `\`${APP}\` is installed but was not granted mesh_store_read — every op the bundle makes ` +
        `will be denied and the window will land in its error state. Re-install and accept the ` +
        `read permission.`,
    );

    // ── 4. the real, labelled window actually opens ──
    const openError = await openRealWindow(bridge, APP);
    expect(
      openError,
      `meshapp_open("${APP}") must succeed — this is the window whose label makes ` +
        `authorize() resolve, and the only surface where the bundle sees real data`,
    ).toBeNull();
    run.mark("real-window-opened");

    // ── 5. the numbers the operator must check the take against ──
    // The banner's honesty was asserted in code when this beat filmed the
    // bundle in Chromium. It can't be, now — so it is not quietly dropped:
    // the counts go in the ledger and into MANIFEST.md, and the guide says
    // to check the take against them. A human check that is written down
    // and printed is weaker than an assertion and much stronger than a
    // habit.
    const counts = atlas!.atom_counts ?? {};
    const shown = Object.entries(counts)
      .sort((a, b) => b[1] - a[1])
      .map(([k, v]) => `${k}: ${fmt(v)}`)
      .join(" · ");
    run.note(
      `atlas \`${CORPUS}\` — ${fmt(atlas!.total_atoms)} atoms (${shown || "no per-type counts"}). ` +
        `The scale banner in the take must agree with these; they come from atlas_list_corpora, ` +
        `the same store meshapp_corpus_stats reads.`,
    );
    run.note(
      `\`${APP}\` granted: ` +
        Object.entries(install!.granted ?? {})
          .filter(([, v]) => v)
          .map(([k]) => k)
          .join(", "),
    );
  },
);

// ─────────────────────────────────────────────────────────────────────
// B3b — Today
// ─────────────────────────────────────────────────────────────────────
rawBeatTest(
  {
    id: "b3-today",
    capture: "raw",
    title: "Current events, on your machine, with no feed and no server",
    claim:
      "The same machinery reads the news: ingested locally, on a daily tick, with " +
      "no server in the loop and no telemetry going out.",
    gifPadSec: 1.0,
    recordingGuide: [
      "Open the real app and launch the Today mesh app from the Library.",
      "Record the mesh-app window only, sized 1280×800.",
      "Beats: the feed of ingested days · the freshness line · open one story and let " +
        "the body render from the local corpus.",
      "Freshness is the load-bearing claim — if the newest day on screen is not the one " +
        "this gate printed, re-run the ingest before shooting.",
    ],
    script: [
      { text: "Today's news, ingested on your machine.", holdMs: 3000 },
      { text: "No feed. No server. No telemetry.", holdMs: 3000 },
    ],
  },
  async ({ bridge, run }) => {
    const CORPUS = "wikipedia-newsworthy";
    const APP = "today";
    const FRESH_DAYS = Number(process.env.SOVEREIGN_DEMO_FRESH_DAYS ?? 3);

    run.requireOrSkip(await hasCorpus(CORPUS), `the \`${CORPUS}\` corpus is not hosted`);

    // Deliberately NOT an atlas gate. The Today bundle's host ops resolve
    // through `document_feed`, which reads the corpus's DOCUMENTS, not the
    // atom graph — `wikipedia-newsworthy` piggybacks on `wikipedia`'s atlas
    // by design and its own `atlas/` dir is empty and correct. An atlas
    // gate here would skip a working app.
    const entry = await corpusEntry(bridge, CORPUS);
    run.requireOrSkip(
      !!entry && (entry.chunks_count ?? 0) > 0,
      `\`${CORPUS}\` reports no indexed chunks (list_corpora: ` +
        `${entry ? `status=${entry.status}, chunks=${entry.chunks_count ?? "null"}` : "absent"}). ` +
        `The feed reads documents, so an empty index is an empty feed.`,
    );

    // ── freshness: "today" that is three weeks old is a different product ──
    const freshnessFile = path.join(indexDir(CORPUS), "_doc_freshness.json");
    run.requireOrSkip(
      fs.existsSync(freshnessFile),
      `cannot verify freshness — no ${freshnessFile}. The beat's whole claim is that the ` +
        `feed is current; set SOVEREIGN_INDEX_DIR if your indexes live elsewhere.`,
    );
    const days = Object.keys(
      JSON.parse(fs.readFileSync(freshnessFile, "utf8")) as Record<string, number>,
    ).sort();
    const newest = days[days.length - 1];
    const ageDays = Math.floor(
      (Date.now() - Date.parse(`${newest}T00:00:00Z`)) / 86_400_000,
    );
    run.requireOrSkip(
      Number.isFinite(ageDays) && ageDays <= FRESH_DAYS,
      `the newest ingested day is ${newest} (${ageDays}d old) — past the ${FRESH_DAYS}d window ` +
        `this beat claims. Run the ingest before shooting, or raise SOVEREIGN_DEMO_FRESH_DAYS ` +
        `if you are deliberately filming an older snapshot.`,
    );

    const install = await meshAppInstall(bridge, APP);
    run.requireOrSkip(install !== null, `the \`${APP}\` mesh app is not installed`);
    run.requireOrSkip(
      install!.granted?.mesh_store_read === true,
      `\`${APP}\` is installed but was not granted mesh_store_read`,
    );

    const openError = await openRealWindow(bridge, APP);
    expect(openError, `meshapp_open("${APP}") must succeed`).toBeNull();
    run.mark("real-window-opened");

    run.note(
      `feed: ${days.length} ingested day(s), newest ${newest} (${ageDays}d old), ` +
        `${fmt(entry!.chunks_count ?? 0)} chunks. The take must show ${newest} at the top.`,
    );
  },
);
