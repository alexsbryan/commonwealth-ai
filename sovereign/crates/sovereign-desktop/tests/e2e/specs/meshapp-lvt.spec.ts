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
  neutral_rate: 1400000000 / 174097946887, // 0.0080414…
  high_land_share_count: 89175,
  underused_count: 2856,
  derivation: [
    "land_value_total = Σ assessed_land_value over 208,666 parcel atoms (sf-assessor-roll) = $174,097,946,887.00",
    "neutral_rate = business_tax_target ÷ land_value_total = $1,400,000,000.00 ÷ $174,097,946,887.00 = 0.80%",
  ],
};

// Inject the host bridge shim BEFORE the bundle's app.js runs.
async function installBridge(page: Page) {
  await page.addInitScript((a) => {
    (window as unknown as { meshApp: unknown }).meshApp = {
      capabilities: async () => ({
        mesh_store_read: true,
        mesh_store_write: false,
        inference_access: false,
        knowledge_access: false,
      }),
      readCorpus: async () => [],
      parcelAnalytics: async () => a,
    };
  }, ANALYTICS);
}

test.describe("SF-LVT mesh app bundle", () => {
  test("renders cited figures + verbatim derivation from the bridge", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/lvt/index.html");

    // Headline land base (compact, cited) + the provenance chip naming
    // the input-set size.
    await expect(page.getByText("$174.10B")).toBeVisible();
    await expect(page.getByText(/Σ over 208,666 parcel atoms/)).toBeVisible();
    await expect(page.locator("#neutral-rate")).toHaveText("0.80%");

    // Flag counts.
    await expect(page.locator("#high")).toHaveText("89,175");
    await expect(page.locator("#under")).toHaveText("2,856");

    // The verbatim derivation block: rendered by the system, carrying the
    // exact (un-rounded) figure — the show-your-work guarantee.
    await expect(page.locator("#derivation li")).toHaveCount(2);
    await expect(page.locator("#derivation")).toContainText("$174,097,946,887.00");

    // Loading replaced by the app; no error.
    await expect(page.locator("#loading")).toBeHidden();
    await expect(page.locator("#error")).toBeHidden();
  });

  test("rate slider drives revenue deterministically (cited base × rate)", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/lvt/index.html");

    // Role-based locator: the accessible slider.
    const slider = page.getByRole("slider", { name: /flat land-only rate/i });
    await expect(slider).toBeVisible();

    // Default sits at the neutral rate → ≈ the $1.4B target. (Range
    // inputs canonicalize the value, so "0.80" reads back as "0.8".)
    await expect(slider).toHaveValue("0.8");
    await expect(page.locator("#revenue")).toHaveText("$1.39B");
    await expect(page.locator("#rate-meta")).toContainText("revenue-neutral");

    // Drive it to 1.00% → 1.00% × $174.10B base = $1.74B (a surplus).
    // (Playwright's fill() rejects range inputs; set value + fire input.)
    await slider.evaluate((el: HTMLInputElement) => {
      el.value = "1";
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await expect(page.locator("#revenue")).toHaveText("$1.74B");
    await expect(page.locator("#rate-meta")).toContainText("surplus");
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
