// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, type Page } from "../fixtures/test-base";

// Onboarding (Welcome → Setup → Consent → Chat). These specs drive
// the first-launch experience that 100% of users see before reaching
// the product. We exercise the happy path, every documented failure
// mode, and a handful of cross-cutting concerns (theme continuity,
// font availability, keyboard a11y, prefers-reduced-motion).
//
// Defining the desired behaviour up front — even when it currently
// fails — is deliberate: the specs are how we drive the UX/UI fixes,
// not how we ratify the current state. Each failing assertion below
// either represents a real bug (theme flip, missing font) or a
// missing UX affordance (no retry on hard failure, no live region).
// They land green only once the surface is fixed.

// ── Phase frames the backend will emit ──────────────────────────
// Same shape as setup_flow.rs::SetupProgress / SetupPhase.
type Frame = {
  phase: { kind: string; [k: string]: unknown };
  message: string;
  fraction?: number | null;
  eta_seconds?: number | null;
  indeterminate?: boolean;
};

const HAPPY_PATH_FRAMES: Frame[] = [
  {
    phase: { kind: "detecting_hardware" },
    message: "Reading what this machine can do.",
    indeterminate: true,
  },
  {
    phase: { kind: "preparing_data_dir" },
    message: "Preparing your storage.",
    indeterminate: true,
  },
  {
    phase: { kind: "downloading_primary", mb_total: 4500 },
    message: "Downloading the main responder.",
    fraction: 0.25,
    eta_seconds: 120,
    indeterminate: false,
  },
  {
    phase: { kind: "downloading_primary", mb_total: 4500 },
    message: "Downloading the main responder.",
    fraction: 0.85,
    eta_seconds: 18,
    indeterminate: false,
  },
  {
    phase: { kind: "downloading_fast" },
    message: "Downloading the quick responder.",
    fraction: 0.6,
    eta_seconds: 30,
    indeterminate: false,
  },
  {
    phase: { kind: "downloading_embed" },
    message: "Downloading the knowledge embedder.",
    fraction: 0.4,
    eta_seconds: 45,
    indeterminate: false,
  },
  {
    phase: { kind: "opening_database" },
    message: "Opening your library.",
    indeterminate: true,
  },
  {
    phase: { kind: "loading_model" },
    message: "Bringing the model online.",
    indeterminate: true,
  },
  {
    phase: { kind: "smoke_testing" },
    message: "Testing the connection.",
    indeterminate: true,
  },
  {
    phase: { kind: "ready" },
    message: "Ready.",
    fraction: 1.0,
    indeterminate: false,
  },
];

// Install a scripted `complete_setup_auto` handler that emits each
// frame on a fake clock (one frame per call to `advance`) and
// resolves only after the final frame is emitted. Returns control
// helpers the test can drive in-page.
async function installScriptedSetup(
  page: Page,
  opts: {
    frames: Frame[];
    /** If set, the corresponding frame index will reject the
     *  `complete_setup_auto` promise after emit. The frame should
     *  carry `phase.kind === "failed"`. */
    rejectAtIndex?: number;
    /** Force the promise to reject with no `failed` frame at all —
     *  exercises the pathological "backend died silently" path. */
    silentReject?: boolean;
  },
) {
  await page.evaluate(({ frames, rejectAtIndex, silentReject }) => {
    // Pre-emit one frame per `advance()` call. The promise the UI is
    // awaiting resolves (or rejects) AFTER the last frame so the
    // narration sticks around until we want it to clear.
    let resolveOuter: (() => void) | null = null;
    let rejectOuter: ((e: unknown) => void) | null = null;
    let idx = 0;
    const queue: Array<() => void> = [];

    function emitNext() {
      if (idx >= frames.length) return false;
      const frame = frames[idx];
      window.__sovereign_test__.emit("setup-progress", frame);
      idx += 1;
      return true;
    }

    window.__sovereign_test__.setHandler("complete_setup_auto", () => {
      return new Promise<void>((resolve, reject) => {
        resolveOuter = resolve;
        rejectOuter = reject;
      });
    });

    (window as unknown as { __setupCtl: unknown }).__setupCtl = {
      /** Emit one queued frame. Returns false if nothing left. */
      advance: () => emitNext(),
      /** Emit all remaining frames in a burst. */
      flush: () => {
        while (emitNext()) {
          /* spin */
        }
      },
      /** Resolve or reject the `complete_setup_auto` promise. */
      finish: () => resolveOuter?.(),
      fail: (msg: string) => rejectOuter?.(new Error(msg)),
      rejectAtIndex: rejectAtIndex ?? null,
      silentReject: !!silentReject,
    };
  }, opts);
}

interface SetupCtl {
  advance(): Promise<boolean>;
  flush(): Promise<void>;
  finish(): Promise<void>;
  fail(msg: string): Promise<void>;
}

function setupCtl(page: Page): SetupCtl {
  const drive = async <R>(
    method: "advance" | "flush" | "finish" | "fail",
    arg?: unknown,
  ): Promise<R> =>
    page.evaluate(
      ({ method, arg }) => {
        const ctl = (window as unknown as { __setupCtl: Record<string, (a?: unknown) => unknown> })
          .__setupCtl;
        return ctl[method](arg) as unknown;
      },
      { method, arg },
    ) as Promise<R>;

  return {
    advance: () => drive<boolean>("advance"),
    flush: () => drive<void>("flush"),
    finish: () => drive<void>("finish"),
    fail: (msg) => drive<void>("fail", msg),
  };
}

// Drive isSetupComplete=false (the only path that lands on welcome).
// Must be set via addInitScript so the override is in place before
// App.svelte's onMount fires the very first `is_setup_complete` call —
// page.evaluate after goto loses that race.
async function bootToWelcome(page: Page) {
  await page.addInitScript(() => {
    const poll = setInterval(() => {
      if (!window.__sovereign_test__) return;
      clearInterval(poll);
      window.__sovereign_test__.setHandler("is_setup_complete", () => false);
    }, 1);
  });
  await page.goto("/");
  await page.locator(".threshold").waitFor();
}

// Advance welcome → setup_plan → setup-flow. The consent-first rework
// inserted the SetupPlan ("Set up Sovereign") consent-before-mutation
// screen between the welcome threshold and the (mutating) SetupFlow, so
// every test that used to click Begin and land on setup now passes
// through the plan. waitFor(".plan") also asserts the plan rendered.
async function beginSetup(page: Page) {
  await page.locator(".begin-btn").click();
  await page.locator(".plan").waitFor();
  await page.locator(".btn-go").click();
}

// ── 1. Happy path ──────────────────────────────────────────────

test.describe("onboarding · happy path", () => {
  test("welcome → setup → consent → chat", async ({ sovereignPage: page }) => {
    await bootToWelcome(page);

    // Welcome screen renders the three-line script + Begin button.
    await expect(page.locator(".line-primary")).toContainText(
      "This is svrnmesh.",
    );
    await expect(page.locator(".begin-btn")).toBeVisible();

    // Wire scripted setup BEFORE clicking Begin so the handler is
    // installed by the time SetupFlow mounts.
    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });

    await beginSetup(page);
    await page.locator(".setup-flow").waitFor();

    const ctl = setupCtl(page);
    const sentence = page.locator(".setup-flow .sentence");

    // Walk through each scripted frame; assert the sentence updates.
    for (const frame of HAPPY_PATH_FRAMES) {
      await ctl.advance();
      await expect(sentence).toHaveText(frame.message);
    }

    // The promise resolves only after the user has seen the "Ready."
    // frame; until then, the setup-flow remains mounted.
    await ctl.finish();

    // Consent gate is the next stop (default shim consent is null).
    await page.locator(".gate").waitFor({ timeout: 5_000 });
    await expect(page.locator(".line-primary")).toContainText(
      "A mesh is a network of friends",
    );

    await page.locator(".choice-primary").click();
    // After consent, we land on the chat (Ask) surface.
    await page.locator(".chat-view").waitFor({ timeout: 5_000 });

    // Sanity: consent was recorded with shareGpu=true.
    const consent = await page.evaluate(() =>
      window.__sovereign_test__.lastConsent(),
    );
    expect(consent).toEqual({ shareGpu: true });
  });

  test("consent · 'keep all compute local' records shareGpu=false", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });

    await beginSetup(page);
    const ctl = setupCtl(page);
    await ctl.flush();
    await ctl.finish();
    await page.locator(".gate").waitFor();

    await page.locator(".choice-secondary").click();
    await page.locator(".chat-view").waitFor();

    const consent = await page.evaluate(() =>
      window.__sovereign_test__.lastConsent(),
    );
    expect(consent).toEqual({ shareGpu: false });
  });

  test("setup-already-complete bypasses welcome and lands on chat", async ({
    sovereignPage: page,
    chat,
  }) => {
    // is_setup_complete default is true, so onMount won't switch to
    // welcome. backend-ready then advances loading → chat.
    await page.goto("/");
    await page
      .locator(".loading-screen, .chat-view, .app-layout")
      .first()
      .waitFor();
    await expect
      .poll(
        async () => {
          await chat.api.signalBackendReady();
          return page.locator(".chat-view").count();
        },
        { timeout: 8_000, intervals: [50, 100, 200] },
      )
      .toBeGreaterThan(0);
  });
});

// ── 2. Determinate vs indeterminate progress ──────────────────

test.describe("onboarding · progress narration", () => {
  test("download phases render percent counter and ETA", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await installScriptedSetup(page, {
      frames: [
        {
          phase: { kind: "detecting_hardware" },
          message: "Reading what this machine can do.",
          indeterminate: true,
        },
        {
          phase: { kind: "downloading_primary", mb_total: 4500 },
          message: "Downloading the main responder.",
          fraction: 0.42,
          eta_seconds: 120,
          indeterminate: false,
        },
      ],
    });

    await beginSetup(page);
    const ctl = setupCtl(page);

    await ctl.advance();
    // Indeterminate phase: ProgressRule shows sweep, no counter.
    await expect(page.locator(".setup-flow .rule-sweep")).toBeVisible();
    await expect(page.locator(".setup-flow .rule-counter")).toHaveCount(0);
    await expect(page.locator(".setup-flow .eta")).toHaveCount(0);

    await ctl.advance();
    // Determinate download phase: counter (42%) + ETA visible.
    await expect(page.locator(".setup-flow .rule-fill")).toBeVisible();
    await expect(page.locator(".setup-flow .rule-counter")).toHaveText("42%");
    await expect(page.locator(".setup-flow .eta")).toHaveText(
      /~\d+\s*(s|min)\s*remaining/,
    );
  });

  test("eta formats: seconds for <60s, minutes otherwise", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await installScriptedSetup(page, {
      frames: [
        {
          phase: { kind: "downloading_primary", mb_total: 100 },
          message: "Downloading the main responder.",
          fraction: 0.5,
          eta_seconds: 45,
          indeterminate: false,
        },
        {
          phase: { kind: "downloading_primary", mb_total: 100 },
          message: "Downloading the main responder.",
          fraction: 0.6,
          eta_seconds: 240,
          indeterminate: false,
        },
      ],
    });

    await beginSetup(page);
    const ctl = setupCtl(page);
    const eta = page.locator(".setup-flow .eta");

    await ctl.advance();
    await expect(eta).toHaveText("~45s remaining");

    await ctl.advance();
    await expect(eta).toHaveText("~4 min remaining");
  });
});

// ── 3. Failure modes ──────────────────────────────────────────

test.describe("onboarding · failure recovery", () => {
  test("recoverable failure shows retry; retry succeeds", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);

    // First call fails after one frame; subsequent call (after Retry)
    // succeeds with the full happy path.
    await page.evaluate(() => {
      let attempt = 0;
      window.__sovereign_test__.setHandler("complete_setup_auto", () => {
        attempt += 1;
        if (attempt === 1) {
          // Emit a Failed frame + reject.
          window.__sovereign_test__.emit("setup-progress", {
            phase: { kind: "downloading_primary", mb_total: 4500 },
            message: "Downloading the main responder.",
            fraction: 0.15,
            indeterminate: false,
          });
          window.__sovereign_test__.emit("setup-progress", {
            phase: { kind: "failed", recoverable: true },
            message: "Network unreachable — check your connection.",
            indeterminate: false,
          });
          return Promise.reject(new Error("net err"));
        }
        // Second attempt: emit Ready + resolve.
        window.__sovereign_test__.emit("setup-progress", {
          phase: { kind: "ready" },
          message: "Ready.",
          fraction: 1.0,
          indeterminate: false,
        });
        return Promise.resolve();
      });
    });

    await beginSetup(page);

    // Failure sentence + retry button appear; the progress rule is
    // gone (the UI hides the rule when failed).
    await expect(page.locator(".setup-flow .sentence")).toContainText(
      "Network unreachable",
    );
    await expect(page.locator(".setup-flow .retry")).toBeVisible();
    await expect(page.locator(".setup-flow .rule")).toHaveCount(0);

    await page.locator(".setup-flow .retry").click();

    // After retry the consent gate (or chat, if already consented) is reached.
    await page.locator(".gate, .chat-view").first().waitFor();
  });

  test("non-recoverable failure shows an escape (re-run or contact)", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await page.evaluate(() => {
      window.__sovereign_test__.setHandler("complete_setup_auto", () => {
        window.__sovereign_test__.emit("setup-progress", {
          phase: { kind: "failed", recoverable: false },
          message: "Hardware not supported on this machine.",
          indeterminate: false,
        });
        return Promise.reject(new Error("hardware unsupported"));
      });
    });

    await beginSetup(page);
    await expect(page.locator(".setup-flow .sentence")).toContainText(
      "Hardware not supported",
    );
    // Even an unrecoverable failure must give the user *some* exit
    // — re-run, file a report, or back-to-welcome. Stuck with no
    // action is the worst possible outcome.
    const escapeAffordance = page
      .locator(".setup-flow")
      .locator("button, a[href]");
    await expect(escapeAffordance.first()).toBeVisible();
  });

  test("backend rejects without emitting Failed — UI synthesises an error", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await page.evaluate(() => {
      window.__sovereign_test__.setHandler("complete_setup_auto", () => {
        return Promise.reject(new Error("daemon crashed during setup"));
      });
    });

    await beginSetup(page);
    // SetupFlow.svelte's catch block sets a `failed` state with
    // `recoverable: false` when the backend didn't emit one itself.
    await expect(page.locator(".setup-flow .sentence")).toContainText(
      /daemon crashed/i,
    );
  });
});

// ── 4. Consent gate edge cases ────────────────────────────────

test.describe("onboarding · consent", () => {
  test("record_first_mesh_consent throws — error shown, choices re-enabled", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });
    await beginSetup(page);
    const ctl = setupCtl(page);
    await ctl.flush();
    await ctl.finish();
    await page.locator(".gate").waitFor();

    // Force the first attempt to reject; second succeeds.
    await page.evaluate(() => {
      let attempt = 0;
      window.__sovereign_test__.setHandler(
        "record_first_mesh_consent",
        (args) => {
          attempt += 1;
          if (attempt === 1) {
            throw new Error("daemon unavailable");
          }
          const shareGpu = !!(args as { shareGpu: boolean }).shareGpu;
          (window.__sovereign_test__ as unknown as {
            _lastConsent: { shareGpu: boolean };
          })._lastConsent = { shareGpu };
          return {
            share_gpu: shareGpu,
            ceiling: 0.5,
            recorded_at_unix: 0,
          };
        },
      );
    });

    await page.locator(".choice-primary").click();
    // Error appears in a live region (role=alert).
    await expect(page.locator(".gate [role='alert']")).toContainText(
      "daemon unavailable",
    );
    // Choices are clickable again (busy state cleared).
    await expect(page.locator(".choice-primary")).toBeEnabled();
    await expect(page.locator(".choice-secondary")).toBeEnabled();

    // Retry: second click should advance through.
    await page.locator(".choice-secondary").click();
    await page.locator(".chat-view").waitFor();
    const consent = await page.evaluate(() =>
      window.__sovereign_test__.lastConsent(),
    );
    expect(consent).toEqual({ shareGpu: false });
  });

  test("consent already recorded — skip directly to chat after setup", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await page.evaluate(() => {
      window.__sovereign_test__.setHandler("get_first_mesh_consent", () => ({
        share_gpu: true,
        ceiling: 0.5,
        recorded_at_unix: 1700_000_000,
      }));
    });
    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });

    await beginSetup(page);
    const ctl = setupCtl(page);
    await ctl.flush();
    await ctl.finish();

    // Consent gate must NOT appear.
    await page.locator(".chat-view").waitFor();
    expect(await page.locator(".gate").count()).toBe(0);
  });

  test("rapid double-click on consent choice records once, not twice", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });
    await beginSetup(page);
    const ctl = setupCtl(page);
    await ctl.flush();
    await ctl.finish();
    await page.locator(".gate").waitFor();

    // Slow the consent handler so the busy state is observable.
    await page.evaluate(() => {
      let calls = 0;
      (window as unknown as { __consentCalls: number }).__consentCalls = 0;
      window.__sovereign_test__.setHandler(
        "record_first_mesh_consent",
        async (args) => {
          calls += 1;
          (window as unknown as { __consentCalls: number }).__consentCalls = calls;
          await new Promise((r) => setTimeout(r, 200));
          const shareGpu = !!(args as { shareGpu: boolean }).shareGpu;
          return {
            share_gpu: shareGpu,
            ceiling: 0.5,
            recorded_at_unix: 0,
          };
        },
      );
    });

    const primary = page.locator(".choice-primary");
    // Two rapid clicks. The component's `busy` guard must suppress
    // the second invocation.
    await primary.click();
    await primary.click({ force: true }).catch(() => {});
    await page.locator(".chat-view").waitFor();

    const calls = await page.evaluate(
      () => (window as unknown as { __consentCalls: number }).__consentCalls,
    );
    expect(calls).toBe(1);
  });
});

// ── 5. Visual consistency (the inconsistencies that drove this) ──

test.describe("onboarding · visual consistency", () => {
  // Every onboarding screen should share the brand substrate. A
  // jarring dark→light→dark flip across welcome→setup→consent
  // breaks the perceived continuity of the application.
  test("background remains a single substrate across welcome → setup → consent", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    const welcomeBg = await page.evaluate(() => {
      const el = document.querySelector(".threshold");
      return el ? getComputedStyle(el).backgroundColor : null;
    });

    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });
    await beginSetup(page);
    await page.locator(".setup-flow").waitFor();
    const setupBg = await page.evaluate(() => {
      const el = document.querySelector(".setup-flow");
      return el ? getComputedStyle(el).backgroundColor : null;
    });

    const ctl = setupCtl(page);
    await ctl.flush();
    await ctl.finish();
    await page.locator(".gate").waitFor();
    const consentBg = await page.evaluate(() => {
      const el = document.querySelector(".gate");
      return el ? getComputedStyle(el).backgroundColor : null;
    });

    // The three substrates must read as the same surface. We pipe
    // each color through a canvas to normalize oklch / hsl / rgb /
    // hex into a single sRGB triplet, then compare relative
    // luminance with a tolerance that allows tint variants but
    // fails on dark↔light flips.
    const lums = await page.evaluate(
      ({ welcomeBg, setupBg, consentBg }) => {
        const canvas = document.createElement("canvas");
        canvas.width = 1;
        canvas.height = 1;
        const ctx = canvas.getContext("2d");
        if (!ctx) return null;
        const toLum = (input: string | null): number | null => {
          if (!input) return null;
          ctx.fillStyle = "#000";
          ctx.fillStyle = input;
          // If the input was unparseable, fillStyle stays "#000000".
          ctx.fillRect(0, 0, 1, 1);
          const { data } = ctx.getImageData(0, 0, 1, 1);
          const [r, g, b] = [data[0], data[1], data[2]].map((v) => {
            const c = v / 255;
            return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
          });
          return 0.2126 * r + 0.7152 * g + 0.0722 * b;
        };
        return [toLum(welcomeBg), toLum(setupBg), toLum(consentBg)];
      },
      { welcomeBg, setupBg, consentBg },
    );
    expect(
      lums && lums.every((l) => l !== null),
      `could not parse backgrounds: ${[welcomeBg, setupBg, consentBg].join(", ")}`,
    ).toBe(true);
    const arr = lums as number[];
    const min = Math.min(...arr);
    const max = Math.max(...arr);
    // 0.15 spans roughly a 35-point L difference in sRGB; anything
    // bigger is unmistakably a theme flip.
    expect(
      max - min,
      `backgrounds disagree: welcome=${welcomeBg} setup=${setupBg} consent=${consentBg}`,
    ).toBeLessThan(0.15);
  });

  // The primary type face on every onboarding screen should resolve
  // to a bundled font. The current implementation asks for "Outfit"
  // first which silently falls through to system-ui — that drift is
  // invisible without this test. We check the FIRST family in the
  // declared stack: if it's loaded, great; if it's not, the screen
  // is not actually rendering in the family the author intended.
  test("primary font on each screen actually loads", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    // Wait for fontsource @font-face declarations to settle.
    await page.evaluate(() => document.fonts.ready);

    const firstFamilyLoaded = async (selector: string) =>
      page.evaluate((s) => {
        const el = document.querySelector(s);
        if (!el) return { family: null, loaded: false };
        const stack = getComputedStyle(el).fontFamily;
        const first = stack.split(",")[0].trim().replace(/^['"]|['"]$/g, "");
        // System / generic fallbacks don't have @font-face records;
        // treat them as "loaded" so we don't flag custom-free pages.
        const isGeneric = /^(system-ui|sans-serif|serif|monospace|-apple-system|ui-monospace)$/i.test(
          first,
        );
        const loaded = isGeneric || document.fonts.check(`1rem "${first}"`);
        return { family: first, loaded };
      }, selector);

    const welcomeFont = await firstFamilyLoaded(".threshold .line");
    expect(
      welcomeFont.loaded,
      `welcome screen first-choice font "${welcomeFont.family}" is not bundled`,
    ).toBe(true);

    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });
    await beginSetup(page);
    await page.locator(".setup-flow").waitFor();
    const setupFont = await firstFamilyLoaded(".setup-flow .sentence");
    expect(
      setupFont.loaded,
      `setup screen first-choice font "${setupFont.family}" is not bundled`,
    ).toBe(true);

    const ctl = setupCtl(page);
    await ctl.flush();
    await ctl.finish();
    await page.locator(".gate").waitFor();
    const consentFont = await firstFamilyLoaded(".gate .line");
    expect(
      consentFont.loaded,
      `consent screen first-choice font "${consentFont.family}" is not bundled`,
    ).toBe(true);
  });

  // The brand mark (◈ InkStamp + the three rings on loading) should
  // appear in the setup phase too so the user sees brand continuity
  // from loading into setup.
  test("brand mark renders on the setup screen", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });
    await beginSetup(page);
    await page.locator(".setup-flow").waitFor();
    await expect(page.locator(".setup-flow .mark")).toBeVisible();
  });
});

// ── 6. Keyboard a11y ─────────────────────────────────────────

test.describe("onboarding · keyboard", () => {
  test("Begin button is keyboard-reachable and activates with Enter", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });

    // Tab to focus the only interactive element on welcome.
    await page.keyboard.press("Tab");
    await expect(page.locator(".begin-btn")).toBeFocused();
    await page.keyboard.press("Enter");
    // Activating Begin via keyboard advances to the setup plan.
    await page.locator(".plan").waitFor();
  });

  test("Consent choices are keyboard-reachable and announce errors via role=alert", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });
    await beginSetup(page);
    const ctl = setupCtl(page);
    await ctl.flush();
    await ctl.finish();
    await page.locator(".gate").waitFor();

    // First Tab focuses the first choice; Shift+Tab from there goes
    // back to nothing (this is the entry screen, no top-level chrome).
    await page.keyboard.press("Tab");
    await expect(page.locator(".choice-primary")).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(page.locator(".choice-secondary")).toBeFocused();

    // Force an error and assert role=alert wires up correctly.
    await page.evaluate(() => {
      window.__sovereign_test__.setHandler(
        "record_first_mesh_consent",
        () => {
          throw new Error("daemon unavailable");
        },
      );
    });
    await page.keyboard.press("Enter");
    const alert = page.locator(".gate [role='alert']");
    await expect(alert).toBeVisible();
    await expect(alert).toContainText("daemon unavailable");
  });

  test("Retry button (recoverable failure) is keyboard-reachable", async ({
    sovereignPage: page,
  }) => {
    await bootToWelcome(page);
    await page.evaluate(() => {
      window.__sovereign_test__.setHandler("complete_setup_auto", () => {
        window.__sovereign_test__.emit("setup-progress", {
          phase: { kind: "failed", recoverable: true },
          message: "Temporary network blip.",
          indeterminate: false,
        });
        return Promise.reject(new Error("blip"));
      });
    });

    await beginSetup(page);
    const retry = page.locator(".setup-flow .retry");
    await expect(retry).toBeVisible();
    await retry.focus();
    await expect(retry).toBeFocused();
  });
});

// ── 7. Motion & resilience ───────────────────────────────────

test.describe("onboarding · motion + resilience", () => {
  test("prefers-reduced-motion suppresses the breathing animation", async ({
    sovereignPage: page,
  }) => {
    // Playwright's emulateMedia survives navigation, so we can keep
    // using the fixture's already-shimmed page.
    await page.emulateMedia({ reducedMotion: "reduce" });
    await bootToWelcome(page);
    await installScriptedSetup(page, { frames: HAPPY_PATH_FRAMES });

    await beginSetup(page);
    await page.locator(".setup-flow").waitFor();

    // The breathing animation is suppressed under reduced-motion;
    // assert the breathing BrandMark has animation-name: none.
    const anim = await page.evaluate(() => {
      const el = document.querySelector(".setup-flow .mark");
      return el ? getComputedStyle(el).animationName : null;
    });
    expect(anim).toBe("none");
  });

  test("setup-required event during loading routes to welcome", async ({
    sovereignPage: page,
  }) => {
    // is_setup_complete returns true (default), so onMount keeps the
    // app in loading. The backend then emits setup-required (e.g., the
    // user wiped their data dir while the desktop was running). The
    // app must respond by switching to welcome.
    await page.goto("/");
    await page.locator(".loading-screen").waitFor();
    await page.evaluate(() => {
      window.__sovereign_test__.emit("setup-required", {});
    });
    await page.locator(".threshold").waitFor({ timeout: 3_000 });
  });

  test("isSetupComplete throws — UI stays on loading, doesn't crash", async ({
    sovereignPage: page,
  }) => {
    await page.addInitScript(() => {
      const poll = setInterval(() => {
        if (!window.__sovereign_test__) return;
        clearInterval(poll);
        window.__sovereign_test__.setHandler("is_setup_complete", () => {
          throw new Error("daemon not ready");
        });
      }, 1);
    });
    await page.goto("/");
    await page.locator(".loading-screen").waitFor();
    // Should stay on loading; should not crash the page (pageerror
    // would fail the test via the fixture's collector).
    await page.waitForTimeout(300);
    await expect(page.locator(".loading-screen")).toBeVisible();
  });
});
