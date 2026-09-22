// SPDX-License-Identifier: AGPL-3.0-or-later
// B11 — A map in your own words.
//
// The ei7-ans corpus was built from a DECLARED ontology (types: hoard,
// mint, coin, ruler). This beat films the stage-0b rendering: the Map
// coloured by the author's own nouns rather than the generic eleven-kind
// palette, and then one answer's evidence path lit across it — the walk
// ledger's atom ids fed through `?highlight=`, no new plumbing.
//
// What it shows, and what it does NOT claim: the map and the reach are
// real and measured; the ANSWER-quality claim is a separate study and
// currently negative-with-attribution (see PRE-REG Deviations). This
// beat films the shape of the thing, not a win.
import { beatTest, expect, demoClick } from "./beat";
import { realBootToChat } from "./demo-base";
import { hasCorpus } from "./preflight";
import fs from "node:fs";
import path from "node:path";

const CORPUS = "ei7-ans";
const INDEX_ROOT =
  process.env.SOVEREIGN_INDEX_ROOT ??
  path.join(process.env.HOME ?? "", ".svrnmesh", "indexes");

/** The corpus's atoms carry declared nouns (`hoard`, `mint`) — checked
 *  from the same file the Tauri command reads, so the beat cannot film an
 *  undeclared map and call it the feature. */
function declaredTypesPresent(): boolean {
  const p = path.join(INDEX_ROOT, CORPUS, "atlas", "atoms.json");
  if (!fs.existsSync(p)) return false;
  try {
    const doc = JSON.parse(fs.readFileSync(p, "utf8")) as {
      atoms?: { data?: { entity_type?: string } }[];
    };
    const nouns = new Set(
      (doc.atoms ?? []).map((a) => a.data?.entity_type ?? ""),
    );
    return nouns.has("hoard") && nouns.has("mint");
  } catch {
    return false;
  }
}

// The demonstration path: atom ids reached by a real board answer
// (`lookup-agrinion-acquired`, full arm, run 1 — the walk's own ledger).
// Baked, as the pre-reg's map shot specifies: "replayed from the study's
// own data, not computed live."
const PATH_IDS = [
  "claim-0333",
  "state-0173",
  "claim-0472",
  "entity-0436",
  "claim-0331",
  "claim-0445",
  "claim-0446",
  "entity-0499",
  "entity-1223",
];

beatTest(
  {
    id: "b11-custom-ontology-map",
    title: "An ontology you wrote, seen as your own kinds",
    claim:
      "Declare how your field thinks — hoards, mints, rulers — and the map reads " +
      "in those words, with the path one answer took lit across it.",
    gifPadSec: 1.0,
    gifMark: "path-lit",
  },
  async ({ page, run }) => {
    const corpus = await hasCorpus(CORPUS);
    if (!corpus) {
      throw new Error(
        `the ${CORPUS} corpus must be installed (svrn corpus install …); ` +
          `this beat films the declared-type map and cannot fake it`,
      );
    }
    if (!declaredTypesPresent()) {
      throw new Error(
        `${CORPUS} has no atlas atoms with declared nouns (hoard, mint); ` +
          `build it first (enrich init --from-corpus / build)`,
      );
    }

    await realBootToChat(page);
    await demoClick(page, page.getByTestId("nav-library"), { settleMs: 600 });
    run.mark("library");

    const card = page
      .getByTestId("notebook-card")
      .filter({ hasText: /numsmatic|numismat|ANS/i })
      .first();
    await expect(card, "the ANS notebook must be on the shelf").toBeVisible({
      timeout: 20_000,
    });
    await demoClick(page, card.getByTestId("notebook-explore").first(), {
      settleMs: 600,
    });
    run.mark("explore");

    // The Map toggle, then let the declared colours read on camera.
    await demoClick(page, page.getByRole("button", { name: "Map" }), {
      settleMs: 800,
    });
    run.mark("map-declared-types");
    await run.caption("Coloured by the ontology's own nouns.", 2600);
    await run.dwell(2600);

    // The answer's path, lit: reload the same view with the ledger's ids.
    const url = new URL(page.url());
    url.searchParams.set("highlight", PATH_IDS.join(","));
    await page.goto(url.toString(), { waitUntil: "load" });
    await demoClick(page, page.getByTestId("nav-library"), { settleMs: 600 });
    const card2 = page
      .getByTestId("notebook-card")
      .filter({ hasText: /numsmatic|numismat|ANS/i })
      .first();
    await demoClick(page, card2.getByTestId("notebook-explore").first(), {
      settleMs: 600,
    });
    await demoClick(page, page.getByRole("button", { name: "Map" }), {
      settleMs: 1000,
    });
    run.mark("path-lit");
    await run.caption("One answer's path, lit across the map.", 3000);
    await run.dwell(3200);
  },
);
