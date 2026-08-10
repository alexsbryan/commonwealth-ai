// SPDX-License-Identifier: AGPL-3.0-or-later
// B4 — Your vault, read note by note.
//
// Point it at an Obsidian vault and it reads every note: a summary tree
// per note, the entities it found in each, built by the model on this
// laptop — and every cluster traceable to the exact paragraphs it was
// built from.
//
// ── Which enrichment system this beat films ──
//
// Canonical map: `corpus-engine/ENRICHMENT.md`. "Enrichment" denotes
// FOUR systems, selected per-corpus by `[enrichment] type`. Two of them
// both answer to the word "atlas", and that collision is what an earlier
// version of this beat died on (ENRICHMENT.md §"The 'atlas' name
// collision"):
//
//   · SYSTEM 2 — Atlas (v2), `type = "atlas"`. A typed ATOM GRAPH
//     (Entity/Claim/Event/Position/Opposition) in `atlas/atoms.json`,
//     read by `FileAtlasReader`, surfaced by `AtlasCorpusView`. Built by
//     the `sovereign enrich init|build` CLI.
//   · SYSTEM 3 — Tiered retrieval (RAPTOR + GLiNER), `type = "tiered"`.
//     A structural SUMMARISATION TREE in SQLite (`conv_skeletons` +
//     `conv_raptor_nodes`), surfaced by `AtlasConvCorpusView` →
//     `ConvDetail`. Built in-process by the daemon.
//
// They do not interoperate: RAPTOR nodes never become atoms, and atoms
// never seed RAPTOR clusters. Same word, two different worlds.
//
// **An Obsidian vault is System 3 by design**, not by accident or by
// omission — ENRICHMENT.md's corpus matrix lists "Obsidian / watched
// folders" as `tiered` (folder variant + `vault_themes`), and records
// that the System-2 pipeline `obsidian_atlas` was REMOVED when the vault
// port moved vaults onto System 3. `lc_enrich_now` ("Make explorable")
// posts to `/internal/corpus/enrich-once` and its doc comment says it
// replaces the legacy `enrich init|build` subprocess. So System 2 is not
// what the product builds for a vault, and a beat gating on `atoms.json`
// skips forever against a fully-enriched corpus — which is exactly what
// this beat used to do.
//
// System 3 is also, per ENRICHMENT.md, "the gold standard" for
// user-facing corpora. Filming it is not a consolation prize.
//
// What it does cost, stated plainly: no entity/claim/opposition graph
// and no click-through to a source passage, because System 3 renders
// neither. A node's evidence is a COUNT on screen, not a link. So the
// provenance claim is machine-checked instead of filmed — see the
// dereference gate below.
import { beatTest, expect, demoClick, demoType } from "./beat";
import { realBootToChat } from "./demo-base";
import { convCorpusReady } from "./preflight";

// The HOSTED vault. (`obsidian-vault` — no suffix — is a bare index dir
// holding a 1,349-atom atom map from a DIFFERENT ingest, with no
// `chunks.lance` and no `_corpus_meta.json`. It is not an installed
// notebook and never appears on the shelf. Do not point this beat at it,
// and do not overlay it onto the hosted vault: the chunk ids are from
// another ingest, so every provenance claim below would silently pass
// while being false.)
const CORPUS = process.env.SOVEREIGN_DEMO_ATLAS_CORPUS ?? "obsidian-vault-959ee8a8f330";
const ANCHOR = process.env.SOVEREIGN_DEMO_ATLAS_ANCHOR ?? "Ostrom";

/** How many member chunk ids to resolve in the dereference gate. Every
 *  one costs a bridge round-trip; the point is proving the ids are live
 *  against THIS ingest, which a sample settles. Whatever is skipped is
 *  reported — a silent cap reads as "we checked everything". */
const DEREF_SAMPLE = 12;

interface Notebook {
  id: string;
  name: string;
  explorable?: boolean;
}
interface ConvSummary {
  conv_uuid: string;
  title: string;
  state: string;
  chunk_count: number;
  top_entities: string[];
  is_tiny: boolean;
}
interface ConvListPage {
  conversations: ConvSummary[];
  total_matching: number;
  next_offset?: number | null;
}
interface RaptorNode {
  node_id: string;
  level: number;
  summary: string;
  primary_entities: string[];
  direct_member_chunk_ids: number[];
  evidence_chunk_count: number;
  cluster_coherence: number;
  is_synthetic_tiny: boolean;
}
interface ConvDetailView {
  conv_uuid: string;
  title: string;
  state: string;
  chunk_count: number;
  raptor_nodes: RaptorNode[];
  max_level: number;
}

beatTest(
  {
    id: "b4-atlas-commons",
    title: "Your own notes on the commons, read back as structure",
    claim:
      "It doesn't just index your vault — it reads every note into a summary tree, " +
      "with the entities it found, on your laptop, and every cluster is built from " +
      "paragraphs that are still in the index.",
    gifPadSec: 1.2,
    gifMark: "note-detail",
  },
  async ({ page, bridge, run }) => {
    // ── Precondition 1: the tiered map exists, AS THE DESKTOP SEES IT ──
    //
    // Known trap, and the reason this reads the bridge rather than the
    // daemon: the desktop's tiered store is
    // `config.data_dir/sovereign.db`, and in ATTACH MODE `data_dir`
    // stays `default_data_dir()` (~/Library/Application Support/
    // sovereign) unless the setup flow adopted the CLI's `[data] dir` —
    // which a baked profile never walks. The daemon's tiered map lives
    // in ~/.svrnmesh/sovereign.db. None of the six `atlas_*conv*`
    // commands has an attach-mode branch (unlike `read_get_chunk`, which
    // routes to /internal/corpus/{id}/chunks/{id}), and the daemon
    // exposes no atlas routes to branch TO. So the desktop reads its own
    // store, full stop; gating on the daemon's sqlite would make this
    // beat pass and then film an empty Explore tab.
    //
    // The harness closes the gap by projecting the daemon's tiered rows
    // for this corpus into the scratch profile's store at bake time
    // (`projectHostTieredMap` in real/global-setup.ts) — real enrichment
    // output, relocated, with the operator's conversations left behind.
    // This gate is what proves the projection actually landed.
    const tiered = await convCorpusReady(bridge, CORPUS);
    run.requireOrSkip(
      tiered.corpus !== null,
      `${(tiered as { why: string }).why}. ` +
        `The desktop reads its OWN tiered store (config.data_dir/sovereign.db), which in ` +
        `attach mode is NOT the daemon's ~/.svrnmesh/sovereign.db. Global setup projects ` +
        `the daemon's rows for SOVEREIGN_DEMO_TIERED_CORPORA into it — check the setup log ` +
        `for "projected daemon tiered map"; a warning there says why it did not. If the ` +
        `daemon itself has no map for this corpus, open the notebook in Library → Explore ` +
        `and click "Make explorable" (lc_enrich_now → POST /internal/corpus/enrich-once). ` +
        `NOTE: do NOT reach for \`sovereign enrich init|build\` — that builds the ATOM map, ` +
        `a different artifact on a different surface, which this beat no longer films.`,
    );

    // ── Precondition 2: it's an explorable notebook ──
    // Separate check, separate remediation: the tiered map can exist for a
    // corpus that never made it onto the shelf, and then there is no
    // Explore tab to open.
    const notebooks = await bridge
      .invoke<Notebook[]>("notebook_list")
      .catch(() => [] as Notebook[]);
    const notebook = notebooks.find((n) => n.id === CORPUS);
    run.requireOrSkip(
      !!notebook && notebook.explorable !== false,
      `\`${CORPUS}\` has a tiered map (${tiered.readyCount} notes read) but is not an ` +
        `explorable notebook — the Library shelf has no Explore tab to open. ` +
        `Install/register it first.`,
    );

    // ── Precondition 3: the anchor note is filmable ──
    // A note the model found too short to cluster renders "too short to
    // break into topic clusters" — a true statement about an empty frame.
    // The beat needs one with a real tree, so it asks for one by name
    // before the camera rolls.
    const listed = await bridge
      .invoke<ConvListPage>("atlas_list_conversations", { corpusId: CORPUS, filter: ANCHOR })
      .catch(() => null);
    run.requireOrSkip(
      !!listed && listed.conversations.length > 0,
      `no note in \`${CORPUS}\` matches "${ANCHOR}". The Explore search filters on note ` +
        `title, so the anchor must appear in one. Set SOVEREIGN_DEMO_ATLAS_ANCHOR to a ` +
        `title fragment that exists in this vault.`,
    );
    const anchorConv = listed!.conversations[0];

    const detail = await bridge
      .invoke<ConvDetailView | null>("atlas_get_conv_detail", {
        corpusId: CORPUS,
        convUuid: anchorConv.conv_uuid,
      })
      .catch(() => null);
    const realNodes = (detail?.raptor_nodes ?? []).filter(
      (n) => !n.is_synthetic_tiny && n.summary.trim().length > 80,
    );
    run.requireOrSkip(
      realNodes.length > 0,
      `"${anchorConv.title}" has no substantive topic cluster — ` +
        `${detail?.raptor_nodes.length ?? 0} node(s), ` +
        `${(detail?.raptor_nodes ?? []).filter((n) => n.is_synthetic_tiny).length} synthetic-tiny. ` +
        `The detail view would render "too short to break into topic clusters", which is a ` +
        `true sentence over an empty frame. Point SOVEREIGN_DEMO_ATLAS_ANCHOR at a longer note.`,
    );

    // ── The dereference gate. ──
    // The old beat clicked an evidence excerpt open and asserted the
    // passage was real. This surface shows an evidence COUNT and offers
    // no click-through, so the same claim is proven off-camera instead of
    // dropped: every member chunk id on the anchor's clusters must
    // resolve, through THIS corpus, to real text. That is what catches a
    // summary tree built against a different ingest — the failure the old
    // beat's "atlas overlaid onto the wrong ingest" assertion existed for.
    const memberIds = [...new Set(realNodes.flatMap((n) => n.direct_member_chunk_ids))];
    run.requireOrSkip(
      memberIds.length > 0,
      `"${anchorConv.title}" has clusters but none carries a member chunk id, so nothing ` +
        `ties the summaries to the index. A tree with no chunk ids cannot be shown as ` +
        `"built from your notes".`,
    );
    const sample = memberIds.slice(0, DEREF_SAMPLE);
    for (const chunkId of sample) {
      const chunk = await bridge.invoke<{ content?: string } | null>("read_get_chunk", {
        corpusId: CORPUS,
        chunkId,
      });
      expect(
        chunk,
        `cluster member chunk ${chunkId} does not resolve in \`${CORPUS}\` — the summary ` +
          `tree was built against a different ingest than the one on the shelf`,
      ).toBeTruthy();
      expect(
        (chunk?.content ?? "").trim().length,
        `cluster member chunk ${chunkId} resolved to empty text`,
      ).toBeGreaterThan(0);
    }
    run.note(
      `provenance: ${sample.length} of ${memberIds.length} member chunk id(s) resolved to ` +
        `real text in \`${CORPUS}\`` +
        (memberIds.length > sample.length
          ? ` (${memberIds.length - sample.length} not checked — DEREF_SAMPLE cap)`
          : ""),
    );
    run.note(
      `tiered map: ${tiered.readyCount} note(s) read · anchor "${anchorConv.title}" has ` +
        `${realNodes.length} substantive cluster(s), ${detail!.max_level + 1} level(s)`,
    );

    // ── Film it. ──
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

    const view = page.locator(".conv-corpus-view");
    await expect(view, "the Explore tab must mount the tiered note map").toBeVisible({
      timeout: 30_000,
    });
    const rows = page.locator('[data-testid="atlas-conv-row"]');
    await expect(rows.first()).toBeVisible({ timeout: 20_000 });
    await run.caption("Every note in the vault, read.", 2800);
    await run.dwell(2400); // let the count + the entity chips read on camera
    run.mark("note-list");

    // ── Find the anchor. ──
    const search = view.locator('.search-row input[type="search"]');
    await expect(search).toBeVisible();
    await demoType(page, search, ANCHOR, { charDelayMs: 90 });
    await run.dwell(1200);

    const hit = rows.filter({ hasText: new RegExp(ANCHOR, "i") }).first();
    await expect(
      hit,
      `the note list must surface a note matching "${ANCHOR}"`,
    ).toBeVisible({ timeout: 20_000 });
    run.mark("anchor-found");
    await demoClick(page, hit.locator(".conv-button"), { settleMs: 500 });

    // ── The note detail: the summary tree. ──
    const pane = page.locator(".conv-detail");
    await expect(pane).toBeVisible({ timeout: 20_000 });
    const title = (await pane.locator("h1").first().textContent())?.trim() ?? "";
    expect(title.length, "the note detail must carry a title").toBeGreaterThan(0);

    const nodes = pane.locator(".raptor-node");
    await expect(
      nodes.first(),
      "the note detail must render at least one topic cluster",
    ).toBeVisible({ timeout: 20_000 });

    const shown = (await pane.locator(".raptor-node .summary").first().textContent())?.trim() ?? "";
    expect(
      shown.length,
      "the cluster must render the summary the enrichment pass wrote",
    ).toBeGreaterThan(80);

    // The panel is showing the corpus, not paraphrasing it. Carried over
    // from the atom version of this beat, against the tiered reader.
    const backing = realNodes.map((n) => n.summary.trim());
    expect(
      backing.some((s) => s === shown || s.startsWith(shown) || shown.startsWith(s)),
      `the summary on screen does not match any summary atlas_get_conv_detail returned for ` +
        `"${anchorConv.title}" — the panel is rendering something other than the stored tree. ` +
        `On screen: ${JSON.stringify(shown.slice(0, 120))}`,
    ).toBe(true);

    run.note(`cluster "${title}": ${shown.slice(0, 160)}`);
    run.mark("note-detail");
    await run.park();
    await run.dwell(3800); // the money frame — let the summary be read

    // ── The entities it found, if the pass named any. ──
    // `top_entities` is salience-ranked from the RAPTOR nodes'
    // primary_entities (summarize_entities), NOT from conv_skeletons —
    // so chips can be present here even when skeleton_json is null.
    // On a vault that is the ONLY entity signal: per ENRICHMENT.md
    // §"GLiNER is currently conversation-scoped", System 3's per-chunk
    // NER runs on the conversation path, not the folder variant. Do not
    // assume "tiered ⇒ GLiNER everywhere".
    const chips = page.locator(".entity-chip");
    if ((await chips.count()) > 0) {
      const names = (await chips.allTextContents()).map((s) => s.trim()).filter(Boolean);
      run.note(`entities on screen: ${names.slice(0, 8).join(" · ")}`);
    } else {
      run.note("no entity chips on this corpus — the entity row is not filmed");
    }
  },
);
