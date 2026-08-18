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
//      an accession THIS CORPUS ACTUALLY HOLDS (membership in the store's
//      own set — NOT a CIK-prefix check; see the `ACCESSION` note);
//   4. an unanswerable one REFUSES and NAMES what is available.
//
// (4) is not optional. A corpus that answers nothing would pass 1-3.
//
// SEC is a live third party under a fair-access policy. This spec makes
// ONE install, and the User-Agent carrying a reachable contact is what
// makes it a 200 rather than a 403 (sec_edgar.rs, DEFAULT_USER_AGENT).
// Do not put this install in a loop.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assertTurnInvariants, sendAndAwaitTurn } from "./invariants";
import { expect, realBootToChat, test } from "./test-base-real";

const CORPUS_ID = "sec-filings-company";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");

/** The typed fact store the install placed, on disk. Same idiom as
 *  `real-workflow-author.spec.ts`, which reads the hermetic profile
 *  directly; the profile dir name mirrors `global-setup.ts:44`. */
function sidecarPath(): string {
  const profile = process.env.SOVEREIGN_REAL_PROFILE_DIR ?? "real-profile";
  return path.join(
    CRATE_ROOT,
    "test-artifacts",
    profile,
    "home/.sovereign/indexes",
    CORPUS_ID,
    "sec_facts.json",
  );
}

/** Every accession the installed store actually holds.
 *
 *  Read from the sidecar rather than from the coverage card because the
 *  card carries only `as_of.accession` — one of the six — and widening
 *  it would be a response-shape change, which this order treats as a
 *  seam to escalate rather than to edit. The sidecar is the same file
 *  the `sec_facts` tool answers from, so this is the store's own truth,
 *  not a second copy of it.
 *
 *  THROWS rather than returning an empty set: an unreadable store must
 *  fail the assertion as could-not-judge, never pass it by vacuously
 *  containing nothing (ARCH §18.3 — absence is reported, never
 *  defaulted). */
function storeAccessions(): Set<string> {
  const p = sidecarPath();
  const raw = fs.readFileSync(p, "utf8");
  const store = JSON.parse(raw) as {
    concepts: Record<string, { facts: Array<{ accession: string }> }>;
  };
  const out = new Set<string>();
  for (const concept of Object.values(store.concepts ?? {})) {
    for (const f of concept.facts ?? []) if (f.accession) out.add(f.accession);
  }
  if (out.size === 0) {
    throw new Error(
      `the typed fact store at ${p} carries no accessions — the instrument ` +
        `cannot judge assertion 3, which is not the same as the answer being wrong`,
    );
  }
  return out;
}

/** Whitespace-normalized text, for comparing a quoted span against the
 *  filing it claims to come from. The extractor collapses the 10-K's
 *  layout into running text and the model re-wraps what it quotes, so a
 *  byte-for-byte compare would fail on formatting alone and prove
 *  nothing about fidelity. Case is PRESERVED — "verbatim" that tolerates
 *  case is not verbatim. */
function flat(s: string): string {
  return s.replace(/\s+/g, " ").trim();
}

/** The ingested 10-K prose, as one normalized string, plus the accessions
 *  its page files are named for.
 *
 *  FOUND, NOT ASSUMED. The install writes prose under a downloads dir
 *  whose name comes from the recipe, and hard-coding that path is exactly
 *  the class of guess that has cost this initiative a run at a time. So
 *  this walks the hermetic profile for a `prose` directory whose page
 *  files are named for an accession the STORE actually holds, and takes
 *  that one. If the layout moves, this keeps working; if no such
 *  directory exists, it THROWS — the risk assertion is then
 *  could-not-judge, which is not the same as the answer being wrong
 *  (ARCH §18.2, §18.3).
 *
 *  Page files are named `<accession-digits>-<page>.txt`, e.g.
 *  `000032019325000079-005.txt` for accession `0000320193-25-000079`. */
function ingestedProse(held: Set<string>): { text: string; accessions: Set<string> } {
  const profile = process.env.SOVEREIGN_REAL_PROFILE_DIR ?? "real-profile";
  const root = path.join(CRATE_ROOT, "test-artifacts", profile, "home");
  const digitsOf = (accession: string) => accession.replace(/-/g, "");
  const heldDigits = new Map([...held].map((a) => [digitsOf(a), a]));

  const candidates: string[] = [];
  const walk = (dir: string, depth: number) => {
    if (depth > 8) return;
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      if (!e.isDirectory()) continue;
      const full = path.join(dir, e.name);
      if (e.name === "prose") candidates.push(full);
      else walk(full, depth + 1);
    }
  };
  walk(root, 0);

  for (const dir of candidates) {
    const files = fs.readdirSync(dir).filter((f) => f.endsWith(".txt"));
    const accessions = new Set<string>();
    for (const f of files) {
      const m = /^(\d{18})-\d+\.txt$/.exec(f);
      const hit = m && heldDigits.get(m[1]);
      if (hit) accessions.add(hit);
    }
    if (accessions.size === 0) continue;
    const text = flat(
      files.map((f) => fs.readFileSync(path.join(dir, f), "utf8")).join("\n"),
    );
    return { text, accessions };
  }

  throw new Error(
    `no ingested 10-K prose found under ${root} whose page files are named ` +
      `for one of the ${held.size} accessions this store holds ` +
      `(${[...held].sort().join(", ")}). Searched ${candidates.length} ` +
      `directory(ies) named "prose". Without the filing's own text there is ` +
      `no independent oracle for "verbatim", so the risk assertion cannot be ` +
      `judged — which is NOT the same as the answer being wrong.`,
  );
}

/** The filers this run proves, in order. TWO by default, because one is
 *  not the feature: every figure measured before this spec was Apple's,
 *  and "it works" for a single hard-coded company is not an installable
 *  product. The assertions are all derived from whatever installs, so the
 *  list is data and the journey is the same code for every entry.
 *
 *  WHY THESE TWO. The order asks for one company that SELF-files and one
 *  that files THROUGH AN AGENT, because the "accession prefix is the
 *  subject's CIK" assumption cost this initiative five attempts (see the
 *  `ACCESSION` note). Checked against EDGAR's own submissions index
 *  before this list was written, rather than assumed:
 *    - AAPL (CIK 0000320193) — latest 10-K `0000320193-25-000079`,
 *      prefix == CIK, SELF-FILED.
 *    - KO   (CIK 0000021344) — latest 10-K `0001628280-26-010047`,
 *      prefix != CIK, AGENT-FILED.
 *  So the pair exercises both shapes on the filing the acquirer actually
 *  selects, not merely somewhere in a back catalogue. `filerShapes` below
 *  records what was OBSERVED per install and the final check asserts the
 *  run covered both — a permanent regression test for the prefix bug at
 *  the cost of one extra install.
 *
 *  Overridable: `SOVEREIGN_SEC_TICKERS` (comma-separated) for the list,
 *  `SOVEREIGN_SEC_TICKER` for the legacy single-filer form. */
const TICKERS = (
  process.env.SOVEREIGN_SEC_TICKERS ??
  process.env.SOVEREIGN_SEC_TICKER ??
  "AAPL,KO"
)
  .split(",")
  .map((s) => s.trim().toUpperCase())
  .filter(Boolean);

/** What each install turned out to be, filled in as the run goes. Read by
 *  the final check. `self` and `agent` are counted per ACCESSION, not per
 *  company: one filer's store routinely carries both (Apple's does), and
 *  the property that matters is that an accession whose prefix is NOT the
 *  subject's CIK was accepted rather than rejected. */
const filerShapes: Array<{
  ticker: string;
  cik: string;
  self: number;
  agent: number;
}> = [];

/** The shape of an SEC accession number, e.g. `0000320193-25-000073`.
 *
 *  ITS LEADING TEN DIGITS ARE NOT THE SUBJECT COMPANY'S CIK, and this
 *  spec asserted for five attempts that they were. They identify the
 *  entity that TRANSMITTED the filing to EDGAR — the filing agent —
 *  which is the subject company only when it self-files.
 *
 *  Measured on the store this spec actually installs (attempt 5,
 *  `sec_facts.json`, Apple CIK 0000320193): six distinct accessions,
 *  THREE prefixed `0000320193` (FY2023-25, self-filed) and THREE
 *  prefixed `0001193125` (FY2013-15, filed through an agent). So a
 *  prefix check is false for half of Apple's own filings, and no
 *  product change can make it true. It rejected a correct answer that
 *  named "Apple Inc. (AAPL, CIK 0000320193)" and cited a real Apple
 *  10-K.
 *
 *  Assertion 3 therefore checks MEMBERSHIP in the accession set the
 *  store actually holds (see `storeAccessions`), which is strictly
 *  stronger: it also catches a citation to a real filing this corpus
 *  never ingested — something a prefix check passes. */
const ACCESSION = /\b\d{10}-\d{2}-\d{6}\b/;

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

// SERIAL IS LOAD-BEARING, not tidiness. Install is SINGLE-INSTANCE under
// the recipe's id: a second company REPLACES the first (and the acquirer
// says so). Minting per-filer corpus ids was priced and deliberately
// dropped as a real seam, so the two-filer proof SEQUENCES — each test
// removes whatever is installed before installing its own filer, at step
// 0 below. Run these in parallel and they fight over one corpus id.
test.describe.configure({ mode: "serial" });

for (const TICKER of TICKERS) {
  test(`${TICKER}: a ticker typed into the catalog installs a corpus that answers a figure with its basis, reports the filing's own risk language, and refuses what it cannot know`, async ({
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
  // OBSERVED, then candidates — never one asserted cause. This message
  // used to read "the acquirer did not place one, or install_fact_sidecar
  // did not move it into the index dir". On attempt 4 it fired with the
  // sidecar sitting correctly on disk (25,130 bytes, 20 concepts) and
  // BOTH named causes innocent: the real one was that `authoritative_store`
  // resolved the recipe only from `recipes_dir`, which a CATALOG install
  // never writes. The message sent the next reader to two correct files
  // and cost a full run plus a live SEC fetch. `corpus_coverage_card`
  // returns null for several upstream reasons and can distinguish none of
  // them, so it states what it saw and lists what to check.
  expect(
    card,
    `corpus_coverage_card returned null for ${CORPUS_ID}. That is the ` +
      `OBSERVATION; the cause is one of several and this assertion cannot ` +
      `tell them apart. Check, in the order they fail silently:\n` +
      `  1. does the recipe resolve with [authority] tool = "sec_facts"? ` +
      `A catalog install writes NO recipe to ~/.svrnmesh/recipes, so the ` +
      `declaration must come from the bundled copy (discovery.rs).\n` +
      `  2. is the sidecar at <indexes>/${CORPUS_ID}/sec_facts.json?\n` +
      `  3. did the acquirer render one at all — grep real-daemon.log for ` +
      `"rendered typed figures".\n` +
      `real-app.log at target=sec_facts debug names which of these it was.`,
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
  //
  // PICK THE MOST CURRENT CONCEPT, not the first one. Until attempt 5
  // this was `store.answers.find(a => a.kind === "duration")`, i.e. the
  // first duration concept in the card's BTreeMap (alphabetical) order.
  // On the real Apple store that is deterministically
  // `advertising_expense` — the ONE concept of twenty whose facts stop
  // at FY2015 (Apple stopped disclosing it) and the ONE whose facts all
  // come from agent-filed accessions. The spec was asking about a
  // discontinued line item from a decade ago and calling it the
  // representative case.
  //
  // That mattered twice over: it maximised the odds of assertion 3
  // tripping on a citation quirk, and it broke assertion 4's premise
  // outright — see the `impossibleYear` note below.
  const durations = store.answers.filter(
    (a) => a.kind === "duration" && a.fiscal_years.length > 0,
  );
  const answerable =
    durations.length > 0
      ? durations.reduce((best, a) =>
          Math.max(...a.fiscal_years) > Math.max(...best.fiscal_years) ||
          (Math.max(...a.fiscal_years) === Math.max(...best.fiscal_years) && a.id < best.id)
            ? a
            : best,
        )
      : store.answers[0];
  const askYear = Math.max(...answerable.fiscal_years);

  // Title carries the TICKER: the profile is reused across the filers in
  // TICKERS, and the sidebar lookup below matches on title text and takes
  // `.first()`. A shared title would silently seal the PREVIOUS filer's
  // conversation — enabled on a corpus that has since been replaced —
  // and the figure question would be asked against nothing.
  const convTitle = `sec-figures-${TICKER}`;
  const conv = await bridge.invoke<{ id: string }>("create_conversation");
  await bridge.invoke("rename_conversation", {
    conversationId: conv.id,
    title: convTitle,
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
  await page.locator(".convo-title", { hasText: convTitle }).first().click();

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
  // MEMBERSHIP in the store's own accession set — see the `ACCESSION`
  // note for why the leading ten digits are NOT the subject's CIK.
  // Deliberately NOT also asserting that the answer states `CIK
  // <store.cik>`: that string appears in the one answer observed so far,
  // but "the grounding block always prints the CIK" is a rule this spec
  // has not verified, and asserting an unverified rule is the exact
  // mistake being corrected here. Membership already covers the honesty
  // failure that matters — a citation to a filing this corpus does not
  // hold, whoever transmitted it.
  const held = storeAccessions();
  expect(
    held.has((cited as RegExpExecArray)[0]),
    `the answer cited accession ${(cited as RegExpExecArray)[0]}, which is not ` +
      `one of the ${held.size} this corpus holds (${[...held].sort().join(", ")}). ` +
      `A figure must cite a filing the store actually ingested:\n${figureText}`,
  ).toBe(true);
  // Period basis: the fiscal period the figure covers, as dates. The
  // XBRL frame label is never the basis (sec_facts_render module docs),
  // so an answer that says only "FY2025" has not carried one.
  expect(
    (figureText.match(/\d{4}-\d{2}-\d{2}/g) ?? []).length,
    `the figure carried no fiscal-period basis (no period dates):\n${figureText}`,
  ).toBeGreaterThan(0);

  // WHAT THIS FILER TURNED OUT TO BE — an OBSERVATION, recorded for the
  // cross-filer check at the end of the file. Never a claim about who
  // filed: an accession whose prefix differs from the subject's CIK was
  // transmitted by someone else, and that is all this counts.
  const bare = store.cik.replace(/^0+/, "");
  const shape = { ticker: TICKER, cik: store.cik, self: 0, agent: 0 };
  for (const a of held) {
    if (a.split("-")[0].replace(/^0+/, "") === bare) shape.self += 1;
    else shape.agent += 1;
  }
  filerShapes.push(shape);

  // ── 4. the refusal, which names what IS available ─────────────────
  // A period ending after the corpus's as-of filing CANNOT be known
  // here — SecRefusal::BeyondAsOf. The refusal must still say what the
  // corpus does hold; "we cannot know that yet" without "here is what
  // we do know" is the abstention §7.7 forbids.
  // DERIVED FROM THE AS-OF, not from the concept's own latest year.
  // `askYear + 3` was only "beyond as-of" when the picked concept
  // happened to be current. Under the old selection it was not: the
  // spec picked `advertising_expense` (latest FY2015) and asked about
  // FY2018 — a period comfortably BEHIND an as-of of 2025-09-27. That
  // is a different refusal class (the concept has no fact for that
  // period) than the one this assertion names in its own comment
  // (`SecRefusal::BeyondAsOf`), and it would very likely have passed
  // anyway by naming FY2013-15 as available. A one-sided bar that
  // passes while testing nothing is what §7.6 was amended to close, so
  // the impossible period is now impossible BY CONSTRUCTION.
  const asOfYear = Number(store.as_of.latest_period_end.slice(0, 4));
  expect(
    Number.isFinite(asOfYear),
    `the store's as-of carries no parseable year (${store.as_of.latest_period_end}), ` +
      `so no period can be shown to be beyond it`,
  ).toBe(true);
  const impossibleYear = asOfYear + 3;
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

  // ── 5. THE RISK QUESTION — the journey this corpus exists for ──────
  // Operator, verbatim, on what "quantify" means here: "we just want to
  // be able to report what the report actually cites -- like a lawsuit
  // coming up, etc". That is NARROWER and safer than deriving exposure
  // numbers, and the three checks below are exactly its three parts.
  //
  // The question deliberately asks something the source CANNOT fully
  // answer. A 10-K carries no forward figures, so "in the next year" has
  // no answer here; the correct behaviour is the filing's own risk
  // language, figures only where they trace, and a refusal to project
  // that still NAMES the trend that exists.
  const riskId = await sendAndAwaitTurn(
    page,
    `What are the material risks facing ${store.entity} in the next year ` +
      `and how would you quantify them?`,
  );
  const risk = await assertTurnInvariants(page, bridge, riskId);
  const riskText = risk.complete.full_text;

  // Preserve the answer whatever the verdict: this is the one turn in the
  // initiative nobody has read, and a failing assertion that also loses
  // the evidence costs a live SEC fetch to see again.
  const artifactDir = path.join(CRATE_ROOT, "test-artifacts", "sec-risk-answers");
  fs.mkdirSync(artifactDir, { recursive: true });
  fs.writeFileSync(path.join(artifactDir, `${TICKER}-risk.txt`), riskText);

  // 5a. THE FILING'S OWN WORDS, checked against the filing.
  // The oracle is the ingested prose on disk, not the product's own
  // claim that it quoted something — a spec that asked the answerer
  // whether it was faithful would be the §18.1 echo smell.
  const prose = ingestedProse(held);
  const MIN_QUOTE = 60;
  const quoted = [...riskText.matchAll(/["“]([^"“”]{20,})["”]/g)].map((m) => flat(m[1]));
  const verbatim = quoted.filter(
    (q) => q.length >= MIN_QUOTE && prose.text.includes(q),
  );
  expect(
    verbatim.length,
    `no quoted span of ${MIN_QUOTE}+ characters in the risk answer appears ` +
      `verbatim in the ${prose.accessions.size} ingested filing(s) ` +
      `(${[...prose.accessions].join(", ")}). ` +
      `${quoted.length} span(s) were quoted; the longest was ` +
      `${Math.max(0, ...quoted.map((q) => q.length))} chars. A risk reported ` +
      `as the company's own language must BE the company's own language — ` +
      `a paraphrase in quotation marks is the failure this checks for.` +
      `\n\nquoted spans:\n${quoted.map((q) => `  - ${q.slice(0, 180)}`).join("\n")}` +
      `\n\nanswer:\n${riskText}`,
  ).toBeGreaterThan(0);

  // ...cited to a filing THIS CORPUS HOLDS. Same membership rule as
  // assertion 3, and for the same reason: a prefix check is false for
  // half of Apple's own filings and passes a citation to a real filing
  // this corpus never ingested.
  const riskCited = [...riskText.matchAll(/\b\d{10}-\d{2}-\d{6}\b/g)].map((m) => m[0]);
  expect(
    riskCited.length,
    `the risk answer cited no accession. Risk language reported without ` +
      `its filing is unsourced prose:\n${riskText}`,
  ).toBeGreaterThan(0);
  const foreign = riskCited.filter((a) => !held.has(a));
  expect(
    foreign,
    `the risk answer cited accession(s) this corpus does not hold. ` +
      `Held (${held.size}): ${[...held].sort().join(", ")}:\n${riskText}`,
  ).toEqual([]);

  // 5b. QUANTIFY ONLY WHAT TRACES — no figure may be attached to a period
  // the store cannot cover. This is the fabrication class observed on the
  // shipped tree before the guard merged: an answer that projected
  // "through FY2026" off a store whose as-of is FY2025.
  //
  // Checked as PROXIMITY in both directions, because "$X in FY2027" and
  // "in FY2027, $X" are the same failure and a one-directional regex
  // catches only one of them.
  const FIGURE = /\$\s?[\d,]+(?:\.\d+)?|\b\d[\d,]*\.?\d*\s?(?:billion|million|thousand)\b/gi;
  const figureAt = [...riskText.matchAll(FIGURE)].map((m) => m.index ?? 0);
  const beyond: string[] = [];
  for (let y = asOfYear + 1; y <= asOfYear + 5; y += 1) {
    for (const m of riskText.matchAll(new RegExp(`\\bFY?\\s?${y}\\b`, "g"))) {
      const at = m.index ?? 0;
      const near = figureAt.find((f) => Math.abs(f - at) <= 120);
      if (near !== undefined) {
        beyond.push(`${y} @${at} with a figure @${near}: ` +
          `"${riskText.slice(Math.min(at, near) - 40, Math.max(at, near) + 60)}"`);
      }
    }
  }
  expect(
    beyond,
    `the risk answer attached a figure to a period beyond this corpus's ` +
      `as-of (${store.as_of.latest_period_end}, accession ` +
      `${store.as_of.accession}). A 10-K carries no forward figures, so any ` +
      `number for FY${asOfYear + 1}+ was invented:\n${beyond.join("\n")}` +
      `\n\nanswer:\n${riskText}`,
  ).toEqual([]);

  // 5c. THE REFUSAL NAMES WHAT IT DOES HAVE. "We cannot know that yet"
  // without "here is what we do know" is the bare abstention §7.7
  // forbids. The store carries multi-year trends; the answer must name
  // at least one fiscal year it actually holds.
  const heldYears = [...new Set(store.answers.flatMap((a) => a.fiscal_years))].sort();
  const namedYears = heldYears.filter((y) => riskText.includes(String(y)));
  expect(
    namedYears.length,
    `the risk answer named none of the ${heldYears.length} fiscal years this ` +
      `corpus holds (${heldYears.join(", ")}). Declining to project forward ` +
      `is correct; declining without naming the trend that DOES exist is a ` +
      `bare abstention:\n${riskText}`,
  ).toBeGreaterThan(0);
  });
}

// ── the cross-filer check ───────────────────────────────────────────────
// Two companies is the feature, not a nicety: everything measured before
// this spec was Apple. And the assumption that an accession's leading ten
// digits are the SUBJECT's CIK cost five attempts, so this pins the fix
// as a permanent regression test — the run must have accepted at least
// one accession transmitted by someone OTHER than the subject.
//
// This states what was OBSERVED across the installs above. It asserts no
// cause and names no filing agent.
test("the run proved more than one filer, and accepted an agent-filed accession", () => {
  expect(
    filerShapes.map((f) => f.ticker),
    `only ${filerShapes.length} of ${TICKERS.length} filer(s) in ` +
      `[${TICKERS.join(", ")}] completed the journey, so the second-company ` +
      `claim is unproven. A single-filer pass is not this feature.`,
  ).toHaveLength(TICKERS.length);
  expect(
    new Set(filerShapes.map((f) => f.cik)).size,
    `the installs resolved to ${new Set(filerShapes.map((f) => f.cik)).size} ` +
      `distinct CIK(s): ${filerShapes.map((f) => `${f.ticker}=${f.cik}`).join(", ")}. ` +
      `Two tickers that resolve to one company prove one company.`,
  ).toBe(TICKERS.length);
  const agent = filerShapes.reduce((n, f) => n + f.agent, 0);
  const self = filerShapes.reduce((n, f) => n + f.self, 0);
  expect(
    self,
    `no SELF-filed accession (prefix == subject CIK) was seen across ` +
      `${JSON.stringify(filerShapes)}`,
  ).toBeGreaterThan(0);
  expect(
    agent,
    `no AGENT-filed accession (prefix != subject CIK) was seen across ` +
      `${JSON.stringify(filerShapes)}. The prefix-is-the-filer bug is then ` +
      `untested by this run: every accession it accepted would also have ` +
      `passed the broken prefix check.`,
  ).toBeGreaterThan(0);
});
