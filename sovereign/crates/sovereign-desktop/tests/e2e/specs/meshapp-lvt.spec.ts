import { test, expect, type Page } from "@playwright/test";

// Agent-drivable verification of the first-party SF land-value-tax mesh
// app bundle. We load the static bundle (vite serves `public/` at root)
// and MOCK the host bridge `window.meshApp` with representative
// deterministic figures — the exact shape `meshapp_parcel_analytics`
// returns — so the bundle's RENDERING + slider logic are verified
// headlessly, without the real Tauri runtime. The Rust bridge + its
// fail-closed authorization are unit-tested separately (`meshapp.rs`).
//
// Assertions favour role/label/text locators ("read the structure, not
// the pixels"), so they double as a screen-reader contract for the app.

const ANALYTICS = {
  corpus_id: "sf-assessor-roll",
  parcel_count: 208666,
  land_value_total: 174097946887,
  improvement_value_total: 170058850999,
  business_tax_target: 1400000000,
  neutral_rate: 1400000000 / 174097946887, // 0.0080414… (business-tax-only)
  property_tax_rate: 0.0118,
  property_tax_revenue_est: (174097946887 + 170058850999) * 0.0118, // ≈ $4.06B
  property_tax_swap_rate: ((174097946887 + 170058850999) * 0.0118) / 174097946887, // ≈ 2.33%
  high_land_share_count: 89175,
  underused_count: 2856,
  derivation: [
    "land_value_total = Σ assessed_land_value over 208,666 parcel atoms (sf-assessor-roll) = $174,097,946,887.00",
    "neutral_rate = business_tax_target ÷ land_value_total = $1,400,000,000.00 ÷ $174,097,946,887.00 = 0.80%",
    "property_tax_revenue_est = (Σland + Σimprovement) × property_tax_rate = $344,156,797,886.00 × 1.18% = $4,061,050,215.05",
    "property_tax_swap_rate = property_tax_revenue_est ÷ land_value_total = $4,061,050,215.05 ÷ $174,097,946,887.00 = 2.33%",
  ],
};

// Representative parcel atoms (the shape of a `read_corpus` row), from a
// real export sample — two on the same street so the address search can
// return both (the pick-list path) or one (exact parcel-number path).
const PARCEL = {
  atom_id: "entity-c187b57689b19f18",
  parcel_number: "1301001",
  source_chunk: "1301001",
  attributes: {
    assessed_land_value: 3033872,
    assessed_improvement_value: 1069136,
    property_location: "0000 0004 25TH AV",
    analysis_neighborhood: "Seacliff",
    use_definition: "Single Family Residential",
  },
};
const PARCEL2 = {
  atom_id: "entity-2nd",
  parcel_number: "1301002",
  source_chunk: "1301002",
  attributes: {
    assessed_land_value: 5000000,
    assessed_improvement_value: 2000000,
    property_location: "0000 0006 25TH AV",
    analysis_neighborhood: "Seacliff",
    use_definition: "Single Family Residential",
  },
};

type Parcel = typeof PARCEL;

// Inject the host bridge mock BEFORE the bundle's app.js runs. searchParcels
// mirrors the Rust op: exact parcel-number OR substring on property_location
// (case-folded).
async function installBridge(page: Page) {
  await page.addInitScript(
    (data) => {
      const { analytics, parcels } = data as { analytics: unknown; parcels: Parcel[] };
      const match = (p: Parcel, q: string) =>
        p.parcel_number.toUpperCase() === q ||
        String(p.attributes.property_location ?? "").toUpperCase().includes(q);
      (window as unknown as { meshApp: unknown }).meshApp = {
        capabilities: async () => ({
          mesh_store_read: true,
          mesh_store_write: false,
          inference_access: false,
          knowledge_access: false,
        }),
        readCorpus: async (_corpusId: string, ids: string[]) =>
          parcels.filter((p) => ids.includes(p.parcel_number) || ids.includes(p.atom_id)),
        searchParcels: async (_corpusId: string, query: string) =>
          parcels.filter((p) => match(p, String(query).toUpperCase())),
        parcelAnalytics: async () => analytics,
      };
    },
    { analytics: ANALYTICS, parcels: [PARCEL, PARCEL2] },
  );
}

test.describe("SF-LVT mesh app bundle", () => {
  test("renders cited figures + verbatim derivation from the bridge", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/lvt/index.html");

    // Headline land base (compact, cited) + the provenance chip naming
    // the input-set size.
    await expect(page.getByText("$174.10B")).toBeVisible();
    await expect(page.getByText(/across 208,666 parcels/)).toBeVisible();
    // Primary reform = the revenue-neutral property-tax swap rate.
    await expect(page.locator("#swap-rate")).toHaveText("2.33%");
    // The business-tax "stable base" insight carries the computed figures.
    await expect(page.locator("#biz-rate")).toHaveText("0.80%");
    await expect(page.locator("#biz-target")).toHaveText("$1.40B");

    // Flag counts.
    await expect(page.locator("#high")).toHaveText("89,175");
    await expect(page.locator("#under")).toHaveText("2,856");

    // The verbatim derivation block: rendered by the system, carrying the
    // exact (un-rounded) figures — the show-your-work guarantee.
    await expect(page.locator("#derivation li")).toHaveCount(4);
    await expect(page.locator("#derivation")).toContainText("$174,097,946,887.00");
    await expect(page.locator("#derivation")).toContainText("property_tax_swap_rate");

    // Loading replaced by the app; no error.
    await expect(page.locator("#loading")).toBeHidden();
    await expect(page.locator("#error")).toBeHidden();
  });

  test("rate slider drives revenue, framed against the property tax it replaces", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/lvt/index.html");

    // Role-based locator: the accessible slider.
    const slider = page.getByRole("slider", { name: /flat land-only rate/i });
    await expect(slider).toBeVisible();

    // Default sits at the revenue-neutral SWAP rate → ≈ the $4.06B property tax.
    await expect(slider).toHaveValue("2.33");
    await expect(page.locator("#revenue")).toHaveText("$4.06B");
    await expect(page.locator("#rate-meta")).toContainText("revenue-neutral");

    // Drive it DOWN to 0.80% (the business-tax-only rate) → $1.39B: a
    // shortfall vs the property tax. The honesty mechanism — a low rate reads
    // as a city-wide CUT, not a free lunch. (Playwright's fill() rejects range
    // inputs; set value + fire input.)
    await slider.evaluate((el: HTMLInputElement) => {
      el.value = "0.8";
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await expect(page.locator("#revenue")).toHaveText("$1.39B");
    await expect(page.locator("#rate-meta")).toContainText("shortfall");
  });

  test("per-parcel: an exact search computes the revenue-neutral swap outcome", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/lvt/index.html");

    await page.getByRole("textbox", { name: /address or parcel number/i }).fill("1301001");
    await page.getByRole("button", { name: /^search$/i }).click();

    await expect(page.locator("#parcel-result")).toBeVisible();
    await expect(page.locator("#p-land")).toHaveText("$3.03M"); // cited from the atom
    await expect(page.locator("#p-impr")).toHaveText("$1.07M");
    // The comparison is the revenue-neutral SWAP, anchored to the swap rate
    // (2.33%), NOT the macro slider: 0.0233 × $3.03M land ≈ $70.8K, vs $48.4K
    // property tax today on land+building → a LOSER (land-rich Seacliff).
    await expect(page.locator("#p-rate")).toHaveText("2.33");
    await expect(page.locator("#p-lvt")).toContainText("$70");
    await expect(page.locator("#p-cur")).toContainText("$48");
    await expect(page.locator("#p-delta")).toContainText("Loser");
    // Plain-English (property-tax swap framing) + the Prop-13 honesty caveat.
    await expect(page.locator("#p-plain")).toContainText("property tax");
    await expect(page.getByText(/Prop-13-frozen/)).toBeVisible();
    await expect(page.locator("#p-chip")).toContainText("entity-c187b57689b19f18");
  });

  test("per-parcel: a street search shows a pick-list; choosing one loads it", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/lvt/index.html");

    // Both sample parcels are on 25TH AV → two matches → pick-list.
    await page.getByRole("textbox", { name: /address or parcel number/i }).fill("25TH");
    await page.getByRole("button", { name: /^search$/i }).click();

    const matches = page.locator("#parcel-matches .match-row");
    await expect(matches).toHaveCount(2);
    // Choosing the second loads its (distinct) cited figures.
    await matches.nth(1).click();
    await expect(page.locator("#parcel-result")).toBeVisible();
    await expect(page.locator("#p-land")).toHaveText("$5.00M");
    await expect(page.locator("#p-chip")).toContainText("entity-2nd");
  });

  test("a bridge denial fails closed — no figures, a clear message", async ({ page }) => {
    // Mirror the host's fail-closed authorize error (ungranted permission).
    await page.addInitScript(() => {
      (window as unknown as { meshApp: unknown }).meshApp = {
        parcelAnalytics: async () => {
          throw new Error("denied: app `lvt` was not granted MeshStoreRead");
        },
      };
    });
    await page.goto("/meshapp/lvt/index.html");

    await expect(page.locator("#error")).toBeVisible();
    await expect(page.locator("#error")).toContainText("denied");
    // The figures section never renders — no partial/uncited UI.
    await expect(page.locator("#app")).toBeHidden();
  });

  test("the real host shim wires window.meshApp to the IPC primitive", async ({ page }) => {
    // The path the mocked-`meshApp` tests above SKIP: inject the exact
    // shim the Rust host embeds (`meshapp_shim.js`) over a mock IPC
    // primitive, and prove the bundle reaches the bridge through it. This
    // is the regression guard for the `withGlobalTauri`-off bug — the shim
    // used `window.__TAURI__` (undefined) instead of `__TAURI_INTERNALS__`,
    // which the meshApp-mocking tests could never have caught.
    await page.addInitScript((a) => {
      const w = window as unknown as {
        __meshInvokeCalls: { cmd: string; args: unknown }[];
        __TAURI_INTERNALS__: { invoke: (cmd: string, args: unknown) => Promise<unknown> };
      };
      w.__meshInvokeCalls = [];
      w.__TAURI_INTERNALS__ = {
        invoke: async (cmd, args) => {
          w.__meshInvokeCalls.push({ cmd, args });
          if (cmd === "meshapp_parcel_analytics") return a;
          if (cmd === "meshapp_capabilities") return { mesh_store_read: true };
          return null;
        },
      };
    }, ANALYTICS);
    // Inject the EXACT shim the host embeds (single source of truth).
    await page.addInitScript({ path: "src-tauri/src/meshapp_shim.js" });

    await page.goto("/meshapp/lvt/index.html");

    // Rendered → the shim resolved `window.meshApp` → IPC primitive → data.
    await expect(page.getByText("$174.10B")).toBeVisible();

    // …and it forwarded camelCase args (the casing contract the Rust side
    // relies on: corpusId → corpus_id).
    const calls = await page.evaluate(
      () =>
        (window as unknown as {
          __meshInvokeCalls: { cmd: string; args: Record<string, unknown> }[];
        }).__meshInvokeCalls,
    );
    const pa = calls.find((c) => c.cmd === "meshapp_parcel_analytics");
    expect(pa, "shim must call meshapp_parcel_analytics").toBeTruthy();
    expect(pa!.args).toMatchObject({ corpusId: "sf-assessor-roll" });
  });
});
