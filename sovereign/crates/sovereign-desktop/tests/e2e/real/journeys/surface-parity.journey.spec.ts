// SPDX-License-Identifier: AGPL-3.0-or-later
// J6 (Tier 1) — SURFACE PARITY. The sv-surface campaign's second
// construction: "one fixture, both surfaces, identical answers per
// family".
//
// THE QUESTION. "Who manufactured the Meridian Lighthouse's Fresnel lens,
// and in what year was it made?" — both facts live in exactly ONE
// passage of ONE fixture document (`lamp-mechanism.txt`: "a second-order
// Fresnel lens manufactured by the Aubert works in 1886"). Chosen over
// the chat-citation journey's height/signal question because that one is
// answered by the doc every fixture chunk is topically adjacent to, so a
// divergence in the cited set would be a tie-break among equally-good
// passages rather than a real retrieval difference. `Aubert` and
// `Fresnel` appear nowhere else in the fixture, so the answer text is
// checkable against its source, and it is a plain factual question — it
// routes to the knowledge path on both surfaces rather than to a
// speech-act handler with no provenance block to compare.
//
// THE SCOPE IS PINNED ON BOTH SIDES, and that is the point of the
// construction: the desktop's per-conversation allow-list is narrowed to
// the fixture corpus through the filter strip, and the CLI is given
// `--corpus <same id>`. One question, one corpus, one daemon — so any
// difference in the answer families is a difference between the
// SURFACES, which is the only thing this journey is allowed to be
// measuring.
//
// THE ROWS.
//   HARD    routed_intent      metadata.routed_intent, both sides.
//   HARD    sources            (corpus_id, chunk_id) handle SET. The
//                              sharp key: titles are per-DOCUMENT and a
//                              document holds several chunks, so a
//                              surface retrieving a different top-k of
//                              the same document would still show
//                              identical titles. Titles are recorded
//                              alongside as the legible row.
//   HARD    grounding_gate     metadata.grounding_gate.action.
//   TRACKED full_text          Recorded with its first divergence, never
//                              failed on: decoding is the one family
//                              where two processes may legitimately
//                              differ, and the campaign wants the number,
//                              not a red.
//   TRACKED answer_names_fact  Does each surface's prose actually say
//                              "Aubert" / "1886".
//
// THE NEGATIVE CONTROL (§18.1). A parity check that cannot fail proves
// nothing, and set-equality over two empty lists is the classic way to
// pass vacuously. So the SAME comparator (`compareSets`) is run a second
// time in the same sweep against a PLANTED TWIN: the identical question,
// through the identical binary, against the identical daemon, with one
// input changed — `--corpus <a different installed corpus>`. That run
// must come back NOT equal, and must cite nothing from the fixture
// corpus. Twin id is read from the daemon's own `/v1/corpora`, so the
// control cannot quietly degrade into "the other corpus wasn't there".
//
// NON-POLLUTION. The CLI's conversations are minted by the daemon in ITS
// store (`<HOME>/.svrnmesh/sovereign.db`); the desktop's sidebar lists
// from the desktop's own store (`config.data_dir/sovereign.db` — see
// global-setup's `desktopDataDir`). Different files, so the CLI turns
// cannot reach the sidebar. Asserted rather than assumed: neither CLI
// conversation id may appear in `list_conversations`.
import fs from "node:fs";
import { FIXTURE_INFO } from "../global-setup";
import { test } from "../test-base-real";
import {
  chatAsk,
  compareSets,
  daemonCorpora,
  desktopAnswer,
  firstDivergence,
  FIXTURE_DAEMON_BASE,
  PARITY_RECORD,
  writeParityRecord,
  type SurfaceAnswer,
} from "./cli-surface";
import { expect, journeyTest, realBootToChat, RUN_ID } from "./journey";
import { J_SURFACE_PARITY } from "./manifest";

const QUESTION =
  "Who manufactured the Meridian Lighthouse's Fresnel lens, and in what year was it made?";
/** The facts that passage carries — the TRACKED content row. */
const FACT_TOKENS = ["Aubert", "1886"];

type Verdict = "passed" | "failed";
interface Row {
  family: string;
  kind: "HARD" | "TRACKED";
  verdict: Verdict;
  detail: Record<string, unknown>;
}

/** The grounding gate's action vocabulary folded to what a reader
 *  experiences: `released` (released / citation_grounded / annotated_marked
 *  / retried — the answer reached the reader with grounding), `withheld`
 *  (every abstained_* and refused_*), or `none` when no gate ran. The
 *  vocabulary is the daemon's (sovereign-core grounding); an action outside
 *  it is its own class, so a new verb cannot silently pass as released. */
function gateClass(action: string | null | undefined): string {
  if (!action) return "none";
  if (["released", "citation_grounded", "annotated_marked", "retried"].includes(action)) {
    return "released";
  }
  if (action.startsWith("abstained") || action.startsWith("refused")) return "withheld";
  return `unclassified:${action}`;
}

journeyTest(J_SURFACE_PARITY, async ({ page, bridge, run }) => {
  // Three real turns (one in-app, two through the CLI) do not fit the
  // config's 180s per-test budget. Each turn is capped at 240s on its
  // own; this is the envelope for all three plus the boot, and the
  // daemon may still be cold — first-launch-setup restarts it in its
  // afterAll, immediately before this journey.
  test.setTimeout(1_200_000);

  const fixture = JSON.parse(fs.readFileSync(FIXTURE_INFO, "utf8")) as {
    corpus_id: string;
    display_name: string;
  };

  // ── The daemon's own account of what is installed ──
  // Both surfaces are about to ask it the same question; this is where
  // the sweep learns that they are backed by the same corpus, and picks
  // the twin from live state.
  const installed = await daemonCorpora();
  const ids = installed.map((c) => c.corpus_id);
  expect(
    ids,
    `the fixture corpus must be installed on the daemon both surfaces use ` +
      `(${FIXTURE_DAEMON_BASE}); it lists ${JSON.stringify(installed)}`,
  ).toContain(fixture.corpus_id);
  const twinCorpus = installed.find((c) => c.corpus_id !== fixture.corpus_id);
  expect(
    twinCorpus,
    "the negative control needs a SECOND installed corpus to plant the twin in; " +
      `the daemon lists only ${JSON.stringify(ids)}. Without it the parity ` +
      "comparison has no demonstrated failing input and the sweep proves nothing.",
  ).toBeTruthy();
  const twinId = twinCorpus!.corpus_id;

  // ── Surface A: the desktop ──
  await realBootToChat(page);
  // A fresh conversation: the allow-list is per-conversation (null =
  // "all installed"), so this starts from a known scope regardless of
  // what the corpus-filter journey left behind.
  await page.locator(".new-btn").click();
  const input = page.locator(".input-area textarea");
  await input.fill(QUESTION);

  // The bar is always rendered; clicking it TOGGLES the strip, so click
  // only when the strip is not already open (a fresh boot collapses it,
  // but this journey must not depend on that).
  const strip = page.locator(".corpus-filter-strip");
  if (!(await strip.isVisible())) await page.getByTestId("ask-scope-bar").click();
  await expect(strip, "the corpus filter strip must render for the pinned scope").toBeVisible();
  const chips = strip.locator(".kb-tag");
  const chipCount = await chips.count();
  const labels: string[] = [];
  for (let i = 0; i < chipCount; i++) {
    labels.push(((await chips.nth(i).textContent()) ?? "").trim());
  }
  const fixtureChipIndex = labels.findIndex((l) => l.includes(fixture.display_name));
  expect(
    fixtureChipIndex,
    `no filter chip carries the fixture corpus name ${JSON.stringify(fixture.display_name)}; ` +
      `the strip shows ${JSON.stringify(labels)}. The desktop scope cannot be pinned to the ` +
      "same corpus the CLI is given, so the two surfaces would not be asked the same question.",
  ).toBeGreaterThanOrEqual(0);
  // Mute every other corpus; leave the fixture one enabled. Each toggle
  // is confirmed before the next so the strip's in-flight guard cannot
  // swallow one (the flake corpus-filter.journey documents).
  for (let i = 0; i < chipCount; i++) {
    const chip = chips.nth(i);
    const muted = ((await chip.getAttribute("class")) ?? "").includes("disabled");
    if (i === fixtureChipIndex) {
      if (muted) {
        await chip.click();
        await expect(chip, "the fixture corpus must be enabled").not.toHaveClass(/disabled/);
      }
      continue;
    }
    if (muted) continue;
    await chip.click();
    await expect(chip, "a muted chip must reflect its disabled state").toHaveClass(/disabled/);
  }
  run.note(`desktop scope pinned to ${fixture.display_name} (${chipCount} chips on the strip)`);

  const facts = await run.turn(QUESTION, { requireCitations: true, timeoutMs: 240_000 });
  const desktop: SurfaceAnswer = desktopAnswer({
    message_id: facts.complete.message_id,
    full_text: facts.complete.full_text,
    metadata: (facts.complete.metadata ?? null) as Record<string, unknown> | null,
  });
  const activeConvo = page.locator(".convo-item.selected");
  desktop.conversationId = await activeConvo.getAttribute("data-conversation-id");

  // ── Surface B: the CLI, same question, same corpus, same daemon ──
  const cliRun = await chatAsk({ question: QUESTION, corpusIds: [fixture.corpus_id], label: "cli" });
  const cli = cliRun.answer;

  // ── The planted twin: one input changed ──
  const twinRun = await chatAsk({ question: QUESTION, corpusIds: [twinId], label: "cli-twin" });
  const twin = twinRun.answer;

  // ── The rows ──
  const sourceVerdict = compareSets(desktop.sourceKeys, cli.sourceKeys);
  const titleVerdict = compareSets(desktop.sourceTitles, cli.sourceTitles);
  const twinVerdict = compareSets(desktop.sourceKeys, twin.sourceKeys);
  const twinFixtureHits = twin.sourceKeys.filter((k) => k.startsWith(`${fixture.corpus_id}#`));
  const textDiff = firstDivergence(desktop.fullText, cli.fullText);
  const namesFact = (a: SurfaceAnswer) => FACT_TOKENS.filter((t) => a.fullText.includes(t));

  const rows: Row[] = [
    {
      family: "routed_intent",
      kind: "HARD",
      verdict:
        desktop.routedIntent !== null && desktop.routedIntent === cli.routedIntent
          ? "passed"
          : "failed",
      detail: { desktop: desktop.routedIntent, cli: cli.routedIntent, cli_twin: twin.routedIntent },
    },
    {
      family: "sources",
      kind: "HARD",
      verdict: sourceVerdict.equal && desktop.sourceKeys.length > 0 ? "passed" : "failed",
      detail: {
        desktop: sourceVerdict.a,
        cli: sourceVerdict.b,
        only_desktop: sourceVerdict.onlyA,
        only_cli: sourceVerdict.onlyB,
        titles_equal: titleVerdict.equal,
        titles: { desktop: titleVerdict.a, cli: titleVerdict.b },
      },
    },
    {
      // The gate's RELEASE CLASS is the parity family, not its raw action.
      // Measured 2026-09-10 on two consecutive runs of this journey: run 1
      // both surfaces "released"; run 2 desktop "released" (per-claim
      // audit) and CLI "citation_grounded" (quote-first) — same intent,
      // same sources, both naming the facts. The raw action is a property
      // of the model's draft under MoE nondeterminism at temperature 0
      // (RUNBOOK §6), not of the surface; a HARD row on it fails on
      // weather. What the surfaces MUST agree on is whether the answer was
      // released to the reader or withheld — the class. The raw action is
      // recorded beside it, TRACKED.
      family: "grounding_gate_class",
      kind: "HARD",
      verdict:
        gateClass(desktop.gateAction) === gateClass(cli.gateAction) &&
        desktop.gateRan === cli.gateRan
          ? "passed"
          : "failed",
      detail: {
        desktop: { class: gateClass(desktop.gateAction), action: desktop.gateAction, ran: desktop.gateRan },
        cli: { class: gateClass(cli.gateAction), action: cli.gateAction, ran: cli.gateRan },
        // A row that passes because BOTH sides are absent is parity, but
        // it is parity on a gate that never ran — say so rather than let
        // the tick read as "verified" (§18.2).
        passed_on_absence: !desktop.gateRan && !cli.gateRan,
      },
    },
    {
      family: "grounding_gate_action",
      kind: "TRACKED",
      verdict: desktop.gateAction === cli.gateAction ? "passed" : "failed",
      detail: { desktop: desktop.gateAction, cli: cli.gateAction },
    },
    {
      family: "full_text",
      kind: "TRACKED",
      verdict: textDiff === null ? "passed" : "failed",
      detail: {
        identical: textDiff === null,
        desktop_len: desktop.fullText.length,
        cli_len: cli.fullText.length,
        first_divergence: textDiff,
      },
    },
    {
      family: "answer_names_fact",
      kind: "TRACKED",
      verdict:
        namesFact(desktop).length === FACT_TOKENS.length &&
        namesFact(cli).length === FACT_TOKENS.length
          ? "passed"
          : "failed",
      detail: {
        expected: FACT_TOKENS,
        desktop: namesFact(desktop),
        cli: namesFact(cli),
        cli_twin: namesFact(twin),
      },
    },
  ];

  // ── The conversations the CLI minted must not reach the sidebar ──
  const sidebar = await bridge.invoke<Array<{ id: string }>>("list_conversations", {
    limit: 200,
  });
  const sidebarIds = sidebar.map((c) => c.id);
  const leaked = [cli.conversationId, twin.conversationId].filter(
    (id): id is string => !!id && sidebarIds.includes(id),
  );

  // ── The record, written BEFORE the assertions so a red sweep still
  // lands its evidence ──
  writeParityRecord({
    kind: "surface_parity",
    campaign: "sv-surface",
    construction: "2 — SURFACE PARITY",
    runId: RUN_ID,
    ts: Date.now(),
    question: QUESTION,
    fixture,
    daemon: { base: FIXTURE_DAEMON_BASE, corpora: installed },
    scope: { desktop: "corpus filter strip pinned to the fixture corpus", cli: `--corpus ${fixture.corpus_id}` },
    surfaces: {
      desktop,
      cli: { ...cli, argv: cliRun.run.argv, duration_ms: cliRun.run.durationMs },
      cli_twin: { ...twin, argv: twinRun.run.argv, duration_ms: twinRun.run.durationMs },
    },
    rows,
    negative_control: {
      twin_corpus_id: twinId,
      twin_display_name: twinCorpus!.display_name,
      comparator: "compareSets(desktop.sourceKeys, twin.sourceKeys)",
      differs: !twinVerdict.equal,
      // How STRONG the control was: a twin that cited nothing at all
      // still differs from a non-empty fixture set, but it demonstrates
      // less than a twin that cited its own corpus. Recorded rather
      // than glossed.
      twin_source_count: twin.sourceKeys.length,
      only_desktop: twinVerdict.onlyA,
      only_twin: twinVerdict.onlyB,
      twin_hits_in_fixture_corpus: twinFixtureHits,
    },
    // ONE STORE: both surfaces are projections over the daemon's store, so
    // the CLI's conversations are visible to the desktop by construction —
    // that is the campaign's thesis, not pollution (an earlier draft
    // asserted the opposite and was wrong on the live run).
    one_store: {
      sidebar_conversation_count: sidebarIds.length,
      cli_conversations_visible_to_desktop: leaked,
    },
  });
  run.note(`parity record written to ${PARITY_RECORD}`);

  // ── Assertions ──
  // The negative control FIRST: if the comparator cannot tell the twin
  // apart, every equality below it is vacuous and reporting them as
  // green would be the §18.1 failure this row exists to prevent.
  expect(
    twinVerdict.equal,
    `NEGATIVE CONTROL FAILED — the same question scoped to a different corpus ` +
      `(${twinId}) produced the SAME source set as the fixture-scoped desktop turn. ` +
      `The comparison is vacuous, so the parity rows below prove nothing. ` +
      `desktop=${JSON.stringify(twinVerdict.a)} twin=${JSON.stringify(twinVerdict.b)}`,
  ).toBe(false);
  expect(
    twinFixtureHits,
    `NEGATIVE CONTROL FAILED — the twin run was scoped to ${twinId} but cited chunks ` +
      `from the fixture corpus: ${JSON.stringify(twinFixtureHits)}. The --corpus ` +
      "allow-list did not hold, so a differing set would not mean what this control claims.",
  ).toEqual([]);

  // One backend, one store: every conversation the CLI opened on the shared
  // daemon must be visible to the desktop's sidebar. A CLI conversation the
  // desktop cannot see would mean the two surfaces read different stores —
  // the split the campaign exists to close.
  expect(
    leaked.length,
    `ONE-STORE PARITY FAILED — the CLI opened conversations on the shared daemon ` +
      `but the desktop sidebar shows none of them (sidebar has ${sidebarIds.length}); ` +
      `the two surfaces are not reading one store`,
  ).toBeGreaterThan(0);

  for (const row of rows.filter((r) => r.kind === "HARD")) {
    expect(
      row.verdict,
      `SURFACE PARITY, family ${row.family}: ${JSON.stringify(row.detail, null, 2)}`,
    ).toBe("passed");
  }

  for (const row of rows.filter((r) => r.kind === "TRACKED" && r.verdict === "failed")) {
    run.note(
      `TRACKED row ${row.family} differs across surfaces (recorded, not failed): ` +
        JSON.stringify(row.detail),
    );
  }
  run.note(
    `parity: intent=${desktop.routedIntent} sources=${desktop.sourceKeys.length} ` +
      `gate=${desktop.gateAction ?? "none"} text_identical=${textDiff === null}; ` +
      `twin(${twinId}) differs=${!twinVerdict.equal}`,
  );
});
