// SPDX-License-Identifier: AGPL-3.0-or-later
// B4 — Atlas: explore your own notes on the commons.
//
// Point it at an Obsidian vault and it doesn't just search it — it reads
// it. Entities, claims, the positions a text takes and what opposes
// them, each dereferenced back to the paragraph it came from.
//
// The beat is anchored on Elinor Ostrom because the vault's atlas
// carries a genuinely good chain around her: the entity, the Nobel
// event, the states around it, and the relation to an economics
// discipline that treated the finding as a curiosity. That chain is what
// makes the surface legible in ten seconds.
//
// Two preconditions, checked separately, because they fail separately
// and the remediation differs:
//   1. the corpus has a BUILT ATLAS  (atlas_list_corpora reports atoms)
//   2. the corpus is an EXPLORABLE NOTEBOOK (notebook_list, explorable)
// A corpus can satisfy one and not the other — on this machine (as of
// 2026-07-24) the hosted vault has chunks with no atlas while an older
// index has the Ostrom atlas with no chunks. Filming half of that would
// show an Explore tab whose evidence links dereference to nothing, which
// is precisely the claim this beat exists to make.
import { beatTest, expect, demoClick, demoType } from "./beat";
import { realBootToChat } from "./demo-base";

// The HOSTED vault, not the bare `obsidian-vault` index directory.
//
// `obsidian-vault` holds the 1349-atom Ostrom atlas but has no
// `chunks.lance` and no `_corpus_meta.json`, so it is not an installed
// notebook and never appears on the shelf — which is why this beat used to
// skip with "has an atlas but is not an explorable notebook". That message
// was describing a DIRECTORY, not a notebook, and sent a prior session
// hunting for a registration step that could never apply.
//
// The hosted vault below has the chunks. Its `_enrichment_state.json` shows
// a COMPLETE run 8 days ago — but with `pipeline_id: "folder_tiered"`
// (RAPTOR tiered summaries), which does not build an atom graph. The atlas
// pipeline is `enrich init` + `enrich build`, and it has not been run here.
// So the honest precondition is "this notebook has no atlas yet", and the
// remediation is a build — not a re-registration and not an overlay.
const CORPUS = process.env.SOVEREIGN_DEMO_ATLAS_CORPUS ?? "obsidian-vault-959ee8a8f330";
const ANCHOR = process.env.SOVEREIGN_DEMO_ATLAS_ANCHOR ?? "Ostrom";

interface AtlasCorpus {
  corpus_id: string;
  display_name: string;
  total_atoms: number;
  atom_counts?: Record<string, number>;
}
interface Notebook {
  id: string;
  name: string;
  explorable?: boolean;
}

beatTest(
  {
    id: "b4-atlas-commons",
    title: "Your own notes on the commons, read back as structure",
    claim:
      "It doesn't just search your vault — it reads it: entities, claims, positions " +
      "and oppositions, every one dereferenced to the paragraph it came from.",
    gifPadSec: 1.2,
    gifMark: "atom-detail",
  },
  async ({ page, bridge, run }) => {
    // ── Precondition 1: a built atlas ──
    const atlasCorpora = await bridge
      .invoke<AtlasCorpus[]>("atlas_list_corpora")
      .catch(() => [] as AtlasCorpus[]);
    const target = atlasCorpora.find((c) => c.corpus_id === CORPUS);
    run.requireOrSkip(
      !!target && target.total_atoms > 0,
      `no built atlas for \`${CORPUS}\`. atlas_list_corpora reports: ` +
        `[${atlasCorpora.map((c) => `${c.corpus_id}:${c.total_atoms}`).join(", ") || "none"}]. ` +
        `Build one with \`sovereign enrich init ${CORPUS}\` then \`sovereign enrich build ${CORPUS}\`, ` +
        `or point SOVEREIGN_DEMO_ATLAS_CORPUS at a corpus that has one. NOTE: a watched-folder ` +
        `ingest runs the \`folder_tiered\` (RAPTOR) pipeline, which completes without building an ` +
        `atom graph — so "it was enriched" and "it has an atlas" are different facts. ` +
        `Do NOT overlay an atlas from a different ingest — the chunk ids won't match and ` +
        `every "dereferences to source" claim in this beat becomes silently false.`,
    );

    // ── Precondition 2: it's an explorable notebook ──
    const notebooks = await bridge
      .invoke<Notebook[]>("notebook_list")
      .catch(() => [] as Notebook[]);
    const notebook = notebooks.find((n) => n.id === CORPUS);
    run.requireOrSkip(
      !!notebook && notebook.explorable !== false,
      `\`${CORPUS}\` has an atlas (${target!.total_atoms} atoms) but is not an explorable ` +
        `notebook — the Library shelf has no Explore tab to open. Install/register it first.`,
    );

    run.note(
      `atlas: ${target!.total_atoms} atoms — ` +
        Object.entries(target!.atom_counts ?? {})
          .map(([k, v]) => `${k}:${v}`)
          .join(" · "),
    );

    await realBootToChat(page);
    await demoClick(page, page.getByTestId("nav-library"), { settleMs: 600 });
    run.mark("library");
    await run.dwell(1400);

    // Open THIS notebook's Explore tab (not "the first card" — the shelf
    // holds thirty-odd notebooks and order is not ours to assume).
    const card = page
      .getByTestId("notebook-card")
      .filter({ hasText: notebook!.name })
      .first();
    await expect(
      card,
      `the "${notebook!.name}" notebook must be on the shelf`,
    ).toBeVisible({ timeout: 20_000 });
    await demoClick(page, card.getByTestId("notebook-explore").first(), { settleMs: 600 });
    run.mark("explore");

    const scroll = page.locator(".atom-scroll");
    await expect(scroll, "the Explore tab must mount the atom map").toBeVisible({
      timeout: 30_000,
    });
    const rows = page.locator('[data-testid="atlas-atom-row"]');
    await expect(rows.first()).toBeVisible({ timeout: 20_000 });
    await run.dwell(2400); // let the type tabs + counts read on camera
    run.mark("atom-map");

    // ── Find the anchor. ──
    await run.caption("Everything it found in my own notes.", 2800);
    const search = page.locator(".search-input");
    await expect(search).toBeVisible();
    await demoType(page, search, ANCHOR, { charDelayMs: 90 });
    await run.dwell(1200);

    const hit = rows.filter({ hasText: new RegExp(ANCHOR, "i") }).first();
    await expect(
      hit,
      `the atlas must surface an atom matching "${ANCHOR}"`,
    ).toBeVisible({ timeout: 20_000 });
    run.mark("anchor-found");
    await demoClick(page, hit, { settleMs: 500 });

    // ── The atom detail. ──
    const detail = page.locator(".atom-detail");
    await expect(detail).toBeVisible({ timeout: 20_000 });
    const title = (await detail.locator(".atom-title").textContent())?.trim() ?? "";
    expect(title.length, "the atom detail must carry a title").toBeGreaterThan(0);
    const body = (await detail.locator(".body-section").first().textContent())?.trim() ?? "";
    expect(
      body.length,
      "the atom detail must render the description the enrichment pass wrote",
    ).toBeGreaterThan(20);
    run.note(`atom "${title}": ${body.slice(0, 160)}`);
    run.mark("atom-detail");
    await run.park();
    await run.dwell(3800); // the money frame — let the description be read

    // ── Dereference. The claim this whole beat rests on. ──
    const evidence = detail.locator(".evidence-button");
    if ((await evidence.count()) > 0) {
      await run.caption("Every atom points back at the paragraph it came from.", 3000);
      run.mark("dereference");
      await demoClick(page, evidence.first(), { settleMs: 500 });
      const surface = page.locator(".reading-surface");
      await expect(
        surface,
        "an evidence excerpt must open the reading surface on its source passage",
      ).toBeVisible({ timeout: 20_000 });
      await expect(
        surface.locator(".content"),
        "the dereferenced passage must be real text, not an empty shell — this is the " +
          "assertion that catches an atlas overlaid onto the wrong ingest",
      ).toContainText(/\S/);
      await run.park();
      await run.dwell(3400);
      run.note("evidence excerpt dereferenced to its source passage");
      await page.keyboard.press("Escape");
      await run.dwell(600);
    } else {
      run.note("this atom carries no evidence excerpts — dereference not filmed");
    }

    // ── Carry it into a question. ──
    const askAbout = detail.getByTestId("atom-ask-about");
    if (await askAbout.isVisible().catch(() => false)) {
      await demoClick(page, askAbout, { settleMs: 500 });
      run.mark("ask-about");
      await expect(page.locator(".chat-view, .notebook-ask, [data-testid='notebook-ask']").first())
        .toBeVisible({ timeout: 20_000 });
      await run.dwell(2600);
      run.note("carried the atom into a scoped question");
    } else {
      run.note("no ask-about affordance on this atom — handoff to chat not filmed");
    }
  },
);
