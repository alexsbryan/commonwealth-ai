// SPDX-License-Identifier: AGPL-3.0-or-later
//
// FINANCIAL_CORPORA §7.2 scenes 2 and 3, driven as a user drives them.
//
// The bar this spec exists to measure is F3: "install by ticker from the
// catalog surface with no repo script invocation". Every step below is a
// click or a keystroke on the shipped UI — the bridge is used only to
// READ state for assertions and to seal a conversation, never to perform
// the install. If this spec passes while `scripts/setup-sec-corpus.sh`
// is untouched on disk, F3 is met; there is no other way to read it.
//
// Four assertions, and the fourth is the one that makes the other three
// mean something (§7.6, as amended):
//
//   1. install by ticker completes from the catalog, no repo script;
//   2. the coverage card renders DERIVED content — every expected string
//      is read out of the installed store first, never hard-coded here,
//      so a card that rendered authored copy fails;
//   3. an answerable question returns a figure carrying period basis AND
//      an accession belonging to THIS filer;
//   4. an unanswerable one REFUSES and NAMES what is available.
//
// (4) is not optional. A corpus that answers nothing would pass 1-3.
//
// SEC is a live third party under a fair-access policy. This spec makes
// ONE install, and the User-Agent carrying a reachable contact is what
// makes it a 200 rather than a 403 (sec_edgar.rs, DEFAULT_USER_AGENT).
// Do not put this install in a loop.
import { assertTurnInvariants, sendAndAwaitTurn } from "./invariants";
import { expect, realBootToChat, test } from "./test-base-real";

const CORPUS_ID = "sec-filings-company";

/** Overridable so a rerun can pick a different filer without editing
 *  the spec; the assertions are derived from whatever installs. */
const TICKER = process.env.SOVEREIGN_SEC_TICKER ?? "AAPL";

/** The shape of an SEC accession number, e.g. `0000320193-25-000073`.
 *  The first ten digits are the FILER's CIK — which is how assertion 3
 *  can check the citation belongs to the company that was installed
 *  rather than merely looking like a citation. */
const ACCESSION = /\b(\d{10})-(\d{2})-(\d{6})\b/;

interface CoverageAsOf {
  form: string;
  accession: string;
  filed: string;
  latest_period_end: string;
}
interface AnsweredConcept {
  id: string;
  label: string;
  kind: "duration" | "instant";
  period_label: string;
  fiscal_years: number[];
}
interface CoverageCardData {
  entity: string;
  ticker: string;
  cik: string;
  answers: AnsweredConcept[];
  period_label: string;
  limits: Array<{ kind: string; statement: string }>;
  as_of: CoverageAsOf;
}
interface CorpusRow {
  id: string;
  name: string;
  status: string;
}

test.describe.configure({ mode: "serial" });

test("a ticker typed into the catalog installs a corpus that answers a figure with its basis and refuses what it cannot know", async ({
  sovereignPage: page,
  bridge,
}) => {
  // Live SEC fetch + full ingest + two model turns. The default 180s
  // budget is for a turn, not for an install.
  test.setTimeout(25 * 60_000);

  // ── 0. make the run repeatable ────────────────────────────────────
  // The catalog offers Install only for a corpus that is NOT installed,
  // so a profile carrying it from a previous run would have no button
  // and assertion 1 would fail for a reason that has nothing to do with
  // the journey. This matters because the real suite can be run with
  // SOVEREIGN_REAL_KEEP_PROFILE=1 (see the run manifest), which is the
  // only way past the harness's fixture-ingest livelock today.
  // Removal is bridge-side setup, not part of what is being proven.
  const before = await bridge.invoke<CorpusRow[]>("list_corpora");
  if (before.some((r) => r.id === CORPUS_ID && r.status !== "not_installed")) {
    await bridge.invoke("remove_corpus", { corpusId: CORPUS_ID });
  }

  // ── 1. install by ticker, from the catalog, with no repo script ───
  await realBootToChat(page);
  await page.getByTestId("nav-library").click();
  await page.getByTestId("library-add").click();
  await page.getByTestId("add-section-catalog").click();

  const installBtn = page.locator(
    `[data-testid="corpus-install"][data-corpus-id="${CORPUS_ID}"]`,
  );
  await expect(
    installBtn,
    `${CORPUS_ID} has no Install button in the catalog. A recipe at ` +
      `catalog_status = "preview" renders as a non-interactive "Coming soon" ` +
      `card instead (KnowledgeStatus.svelte), which is F3 unmet by construction.`,
  ).toBeVisible({ timeout: 30_000 });
  await installBtn.click();

  // The form is rendered FROM the recipe's [parameters] block. `contact`
  // must be visible and pre-filled: it is the address SEC is told to
  // reach the user at, and the recipe declared it a parameter rather
  // than a hidden constant precisely so it is not sent unseen.
  const form = page.getByTestId("param-form");
  await expect(form).toBeVisible({ timeout: 30_000 });
  const contact = page.getByTestId("param-contact");
  await expect(contact).toBeVisible();
  await expect(contact).toBeEditable();
  expect(
    ((await contact.inputValue()) ?? "").trim().length,
    "the declared contact default must reach the field, or SEC returns 403",
  ).toBeGreaterThan(0);

  // Required, undefaulted: Install is unavailable until the user types.
  await expect(page.getByTestId("param-form-install")).toBeDisabled();
  await page.getByTestId("param-ticker").fill(TICKER);
  await expect(page.getByTestId("param-form-install")).toBeEnabled();
  await page.getByTestId("param-form-install").click();

  // Installed means the daemon says installed, not that the UI flipped.
  const deadline = Date.now() + 20 * 60_000;
  let installedRow: CorpusRow | undefined;
  let lastStatus = "(never listed)";
  while (Date.now() < deadline) {
    const rows = await bridge.invoke<CorpusRow[]>("list_corpora");
    const row = rows.find((r) => r.id === CORPUS_ID);
    if (row) lastStatus = row.status;
    if (row && row.status === "installed") {
      installedRow = row;
      break;
    }
    // A failed install has nowhere to go but the failure card; break on
    // it rather than burning the whole budget waiting for a status that
    // will never arrive.
    if ((await page.getByTestId("install-failed").count()) > 0) {
      lastStatus = `failed: ${await page.getByTestId("install-failed").first().innerText()}`;
      break;
    }
    await page.waitForTimeout(5_000);
  }
  expect(
    installedRow,
    `${CORPUS_ID} never reached "installed" (last status: ${lastStatus}). ` +
      `Read real-daemon.log for target=sec_edgar: it names ticker -> CIK, the ` +
      `selected 10-K and every skip. If that trace is ABSENT rather than ` +
      `unhappy, the run had no RUST_LOG and the daemon's allowlist ` +
      `(DAEMON_TRACING_FILTER) dropped it — diagnose the instrument first. ` +
      `A 403 means the User-Agent carried no contact; an empty window means ` +
      `no 10-K was in range; no "Starting embed+index pipeline ` +
      `corpus=${CORPUS_ID}" means it never got past acquire.`,
  ).toBeTruthy();

  // ── 2. the coverage card renders DERIVED content ──────────────────
  // Read the store FIRST, then require the DOM to agree with it. Every
  // expected string below comes from this object; none is typed into
  // the spec, so a card rendering authored copy cannot pass.
  const card = await bridge.invoke<CoverageCardData | null>("corpus_coverage_card", {
    corpusId: CORPUS_ID,
  });
  expect(
    card,
    "no typed fact store for the installed corpus — the acquirer did not " +
      "place one, or install_fact_sidecar did not move it into the index dir",
  ).toBeTruthy();
  const store = card as CoverageCardData;
  expect(store.answers.length, "an installed SEC corpus must answer something").toBeGreaterThan(0);
  expect(store.cik.replace(/^0+/, "").length).toBeGreaterThan(0);

  await page.getByTestId("add-sheet-close").click();
  // By corpus id, not by display text: `notebook_list` names the card
  // from the CATALOG entry, not from the filer, so matching on the
  // entity would never hit and matching on "SEC Filings" would pin the
  // test to catalog copy. `data-notebook-id` already carries the id
  // (LibraryView.svelte:152) — identity from essence (ARCH §7.5).
  await page
    .locator(`[data-testid="notebook-card"][data-notebook-id="${CORPUS_ID}"] .card-open`)
    .click();
  // Sources lives behind the overflow menu — `notebook-tab-sources` is
  // inside `{#if moreMenuOpen}` (NotebookDetail.svelte:486-494), so it
  // does not exist in the DOM until the trigger is clicked.
  await page.getByTestId("notebook-more").click();
  await page.getByTestId("notebook-tab-sources").click();

  const coverage = page.getByTestId("coverage-card");
  await expect(coverage).toBeVisible({ timeout: 30_000 });
  // The as-of block must carry the accession the store recorded.
  await expect(page.getByTestId("coverage-as-of")).toContainText(store.as_of.accession);
  // And the answers block must name a concept the store actually holds.
  await expect(page.getByTestId("coverage-answers")).toContainText(store.answers[0].label);

  // ── 3. an answerable question, with basis and accession ───────────
  // The question is BUILT from the store, so it is answerable by
  // construction rather than by a guess about what this filer reports.
  const answerable =
    store.answers.find((a) => a.kind === "duration" && a.fiscal_years.length > 0) ??
    store.answers[0];
  const askYear = Math.max(...answerable.fiscal_years);

  const conv = await bridge.invoke<{ id: string }>("create_conversation");
  await bridge.invoke("rename_conversation", {
    conversationId: conv.id,
    title: "sec-figures-by-ticker",
  });
  await bridge.invoke("set_conversation_enabled_corpora", {
    conversationId: conv.id,
    enabledCorpora: [CORPUS_ID],
  });
  // Re-boot rather than clicking `nav-ask`: the sidebar's conversation
  // list is loaded at boot, and this conversation was created after
  // this page booted, so it is not in that list yet. Every real spec
  // that seals a conversation seals it BEFORE `realBootToChat`
  // (native-grounding-p1.real.spec.ts:121-135); the install had to come
  // first here, so the boot moves instead.
  await realBootToChat(page);
  await page
    .locator(".convo-title", { hasText: "sec-figures-by-ticker" })
    .first()
    .click();

  const figureId = await sendAndAwaitTurn(
    page,
    `What was ${store.entity}'s ${answerable.label} in FY${askYear}?`,
  );
  const figure = await assertTurnInvariants(page, bridge, figureId);
  const figureText = figure.complete.full_text;

  const cited = ACCESSION.exec(figureText);
  expect(
    cited,
    `the figure carried no accession. A typed answer cites its filing:\n${figureText}`,
  ).toBeTruthy();
  expect(
    (cited as RegExpExecArray)[1],
    `the cited accession belongs to a different filer than the installed one ` +
      `(store CIK ${store.cik}):\n${figureText}`,
  ).toBe(store.cik);
  // Period basis: the fiscal period the figure covers, as dates. The
  // XBRL frame label is never the basis (sec_facts_render module docs),
  // so an answer that says only "FY2025" has not carried one.
  expect(
    (figureText.match(/\d{4}-\d{2}-\d{2}/g) ?? []).length,
    `the figure carried no fiscal-period basis (no period dates):\n${figureText}`,
  ).toBeGreaterThan(0);

  // ── 4. the refusal, which names what IS available ─────────────────
  // A period ending after the corpus's as-of filing CANNOT be known
  // here — SecRefusal::BeyondAsOf. The refusal must still say what the
  // corpus does hold; "we cannot know that yet" without "here is what
  // we do know" is the abstention §7.7 forbids.
  const impossibleYear = askYear + 3;
  const refusalId = await sendAndAwaitTurn(
    page,
    `What was ${store.entity}'s ${answerable.label} in FY${impossibleYear}?`,
  );
  const refusal = await assertTurnInvariants(page, bridge, refusalId);
  const refusalText = refusal.complete.full_text;

  // It must NAME something available. The latest period end is in the
  // store; so is the as-of accession. Either naming is "what IS
  // available"; naming neither is a bare abstention.
  const namesAvailable =
    refusalText.includes(store.as_of.latest_period_end) ||
    refusalText.includes(store.as_of.accession) ||
    store.answers.some((a) => a.fiscal_years.some((fy) => refusalText.includes(String(fy))));
  expect(
    namesAvailable,
    `the refusal named nothing that IS available (expected the latest period ` +
      `end ${store.as_of.latest_period_end}, the as-of accession ` +
      `${store.as_of.accession}, or a fiscal year the store holds):\n${refusalText}`,
  ).toBe(true);

  // And it must not have invented a figure FOR the impossible period:
  // no accession may be presented as covering FY<impossibleYear>.
  const fabricated = new RegExp(
    `FY${impossibleYear}[^.]{0,160}?\\b\\d{10}-\\d{2}-\\d{6}\\b`,
  ).test(refusalText);
  expect(
    fabricated,
    `a figure was cited for FY${impossibleYear}, which ends after this ` +
      `corpus's as-of filing (${store.as_of.accession}, latest period end ` +
      `${store.as_of.latest_period_end}):\n${refusalText}`,
  ).toBe(false);
});
