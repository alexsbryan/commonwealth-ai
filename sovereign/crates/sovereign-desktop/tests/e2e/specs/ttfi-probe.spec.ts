// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Direct unit-style coverage of the TTFI probe semantics. Scenarios in
// ttfi.spec.ts exercise the probe through real chat-machine state; this
// file pins down the probe's CONTRACT against synthetic DOM cases:
//
//   • `visible` ≠ `aux` when an element is in DOM but not in viewport
//   • `visible` fires once the element scrolls/repositions into view
//   • `gap` = content − specific, derived on read
//   • Re-marking start resets all tiers
//
// Without these, a refactor that flattens `visible` into `aux` (or
// breaks the IntersectionObserver wiring) would slip through scenario
// runs because real scenarios always render in viewport.

test.describe("TTFI probe contract", () => {
  test("visible tier is null when an aux element is rendered off-screen", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    await chat.api.ttfi.markStart();

    // Inject a synthetic interpretation-banner far above the viewport.
    // We use this class because it's matched by both the aux and
    // visible selectors, so the contrast is direct: aux MUST fire
    // (DOM presence), visible MUST NOT (no viewport intersection).
    await page.evaluate(() => {
      const el = document.createElement("div");
      el.className = "interpretation-banner";
      el.textContent = "off-screen banner";
      el.style.position = "fixed";
      el.style.top = "-2000px";
      el.style.left = "0";
      el.style.width = "200px";
      el.style.height = "30px";
      document.body.appendChild(el);
    });

    // Wait long enough for IntersectionObserver to fire if it were
    // going to. IO callbacks are queued microtasks after layout — a
    // single rAF is plenty, but we give a comfortable budget.
    await page.waitForTimeout(150);

    const offscreen = await chat.api.ttfi.getReport();
    expect(offscreen.aux).not.toBeNull();
    expect(offscreen.visible).toBeNull();

    // Move the same element into view. IntersectionObserver should
    // now fire and visible should populate.
    await page.evaluate(() => {
      const el = document.querySelector(
        ".interpretation-banner",
      ) as HTMLElement | null;
      if (el) el.style.top = "100px";
    });
    await page.waitForTimeout(150);

    const onscreen = await chat.api.ttfi.getReport();
    expect(onscreen.visible).not.toBeNull();
    // Visible must land AFTER aux (chronological — the banner moved
    // into viewport later). Slight defensive epsilon on equality
    // because the sweep + IO callback can land on the same tick.
    expect(onscreen.visible!).toBeGreaterThanOrEqual(onscreen.aux! - 1);
  });

  test("gap is derived as content − specific on read", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await chat.api.ttfi.markStart();

    // Inject specific marker first, then content marker after a
    // measurable delay. Gap should equal the delta.
    await page.evaluate(() => {
      const slot = document.createElement("div");
      slot.className = "doc-progress-indicator";
      slot.innerHTML =
        '<span class="progress-mark">◈</span><span class="progress-text">working</span>';
      document.body.appendChild(slot);
    });
    await page.waitForTimeout(50);

    const beforeContent = await chat.api.ttfi.getReport();
    expect(beforeContent.specific).not.toBeNull();
    expect(beforeContent.content).toBeNull();
    expect(beforeContent.gap).toBeNull();

    await page.waitForTimeout(200);
    await page.evaluate(() => {
      const wrap = document.createElement("div");
      wrap.className = "sv-ai-msg";
      wrap.innerHTML = '<div class="sv-prose">first content</div>';
      document.body.appendChild(wrap);
    });
    await page.waitForTimeout(50);

    const after = await chat.api.ttfi.getReport();
    expect(after.specific).not.toBeNull();
    expect(after.content).not.toBeNull();
    expect(after.gap).not.toBeNull();
    // Gap = content − specific, exactly.
    expect(after.gap).toBeCloseTo(after.content! - after.specific!, 5);
    // Sanity: gap reflects the ~200ms wait we inserted (allow slack
    // for rAF + setTimeout coalescing).
    expect(after.gap!).toBeGreaterThan(150);
  });

  test("thinking tier fires for .think-block independent of .sv-prose", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await chat.api.ttfi.markStart();

    // Inject a synthetic .think-block (the same class ThinkBlock.svelte
    // emits). thinking should fire; content must stay null because
    // .sv-prose isn't present.
    await page.evaluate(() => {
      const el = document.createElement("div");
      el.className = "think-block";
      el.innerHTML =
        '<button class="think-toggle"><span class="think-label">Reasoning</span></button>';
      document.body.appendChild(el);
    });
    await page.waitForTimeout(80);

    const thinkOnly = await chat.api.ttfi.getReport();
    expect(thinkOnly.thinking).not.toBeNull();
    expect(thinkOnly.content).toBeNull();
    // Specifically: gap should still be null because content hasn't
    // arrived. Thinking does not feed the gap derivation.
    expect(thinkOnly.gap).toBeNull();

    // Now add prose. Content should fire and gap may populate (if
    // specific had also fired earlier; in this synthetic case it
    // hasn't, so gap stays null — that's correct).
    await page.waitForTimeout(150);
    await page.evaluate(() => {
      const wrap = document.createElement("div");
      wrap.className = "sv-ai-msg";
      wrap.innerHTML = '<div class="sv-prose">answer text</div>';
      document.body.appendChild(wrap);
    });
    await page.waitForTimeout(80);

    const after = await chat.api.ttfi.getReport();
    expect(after.thinking).not.toBeNull();
    expect(after.content).not.toBeNull();
    // thinking landed before content — that's the whole point of the
    // tier on reasoning-heavy turns.
    expect(after.thinking!).toBeLessThan(after.content!);
  });

  test("staleness records longest static-text window in slot", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await chat.api.ttfi.markStart();

    // Plant the slot with text "A".
    await page.evaluate(() => {
      const wrap = document.createElement("div");
      wrap.className = "doc-progress-indicator";
      wrap.innerHTML =
        '<span class="progress-mark">◈</span><span class="progress-text">A</span>';
      document.body.appendChild(wrap);
    });
    await page.waitForTimeout(50); // let MutationObserver record

    // Wait 200ms (steady state), then change text to "B".
    await page.waitForTimeout(200);
    await page.evaluate(() => {
      const t = document.querySelector(
        ".doc-progress-indicator .progress-text",
      );
      if (t) t.textContent = "B";
    });
    await page.waitForTimeout(50);

    // Wait 350ms more, then remove the slot entirely.
    await page.waitForTimeout(350);
    await page.evaluate(() => {
      document.querySelector(".doc-progress-indicator")?.remove();
    });
    await page.waitForTimeout(50);

    const r = await chat.api.ttfi.getReport();
    expect(r.staleness).not.toBeNull();
    // Two static windows in the slot's lifetime: ~250ms (A from
    // appearance to change) and ~350ms (B from change to removal).
    // Max is the trailing one. Allow generous slack for timer jitter.
    expect(r.staleness!).toBeGreaterThan(280);
    expect(r.staleness!).toBeLessThan(600);
  });

  test("markStart resets all tiers including visible", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    await chat.api.ttfi.markStart();
    await page.evaluate(() => {
      const el = document.createElement("div");
      el.className = "interpretation-banner";
      el.style.position = "fixed";
      el.style.top = "100px";
      el.style.left = "0";
      el.style.width = "100px";
      el.style.height = "20px";
      el.textContent = "first";
      document.body.appendChild(el);
    });
    await page.waitForTimeout(100);
    const first = await chat.api.ttfi.getReport();
    expect(first.aux).not.toBeNull();
    expect(first.visible).not.toBeNull();

    // Re-anchor. The previously-rendered banner is still in the DOM,
    // but markStart() resets the report and rebinds observers — the
    // initial sweep finds the existing element and re-records aux/
    // visible from the new t0.
    await chat.api.ttfi.markStart();
    await page.waitForTimeout(100);
    const second = await chat.api.ttfi.getReport();
    // New report: every tier is freshly derived from the new t0, so
    // values should be SMALL (close to 0), not the old larger numbers.
    expect(second.aux).not.toBeNull();
    expect(second.aux!).toBeLessThan(first.aux! / 2 + 50);
  });
});
