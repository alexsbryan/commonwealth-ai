// SPDX-License-Identifier: AGPL-3.0-or-later
// B1 — "Is free will compatible with determinism?"
//
// The hero beat: a hard question, the glassbox open while it works, an
// answer, and the click back to the sentence in the Stanford
// Encyclopedia of Philosophy it came from.
//
// Deliberately asked UNSCOPED, against the operator's whole shelf. The
// scoped version (mute 32 chips, then ask) is a worse demo and a weaker
// claim; asking everything and landing on `sep` proves retrieval picked
// the right shelf out of thirty-odd on its own. If it doesn't land
// there, that is a real regression in routing, and this beat should go
// red rather than be quietly narrowed until it passes.
import { beatTest, expect, demoClick } from "./beat";
import { realBootToChat } from "./demo-base";
import { hasCorpus } from "./preflight";

const QUESTION = "Is free will compatible with determinism?";

beatTest(
  {
    id: "b1-determinism",
    title: "Ask a hard question, watch it work, follow the answer home",
    claim:
      "You can ask a hard question and see exactly what it read, what it weighed, " +
      "and which sentence the answer came from — on your laptop.",
    gifPadSec: 1.0,
    gifMark: "citation-click",
  },
  async ({ page, run }) => {
    run.requireOrSkip(
      await hasCorpus("sep"),
      "the `sep` corpus is not hosted by the daemon on :9741 — ingest it before capturing B1",
    );

    await realBootToChat(page);
    await run.dwell(1200); // let the surface settle before anything moves
    run.mark("open");

    // The scope bar states, in plain language, what this question will
    // reach. Filming it is the honest framing for the unscoped ask.
    const scopeBar = page.getByTestId("ask-scope-bar");
    if (await scopeBar.isVisible().catch(() => false)) {
      await expect(scopeBar).toContainText(/\S/);
      run.note(`scope bar reads: "${(await scopeBar.textContent())?.trim()}"`);
    } else {
      run.note("ask-scope-bar not rendered on this build — filming without it");
    }

    // Watch for the mid-turn glassbox transients CONCURRENTLY: both are
    // cleared the instant the phase they belong to ends, so they cannot
    // be observed after the turn resolves.
    // Boxed rather than a bare `let`: the assignment happens inside a
    // closure, and TS narrows the outer binding to its initializer, so a
    // `let x: string | null = null` reads as `never` at the check below.
    const observed: { heartbeat?: string } = {};
    const heartbeat = page.locator('[data-testid="synthesis-heartbeat"]');
    const heartbeatWatch = heartbeat
      .waitFor({ state: "visible", timeout: 120_000 })
      .then(async () => {
        observed.heartbeat = (await heartbeat.textContent())?.trim() ?? "";
        run.mark("synthesis-heartbeat");
      })
      .catch(() => {
        /* synthesis window shorter than a frame — best effort */
      });

    const narration = page.getByTestId("narration-stack");
    const narrationWatch = narration
      .waitFor({ state: "visible", timeout: 120_000 })
      .then(() => run.mark("narration"))
      .catch(() => {
        /* retrieval too fast to surface a chip — best effort */
      });

    await run.caption("Asked locally. Nothing left the machine.", 3200);
    const facts = await run.turn(QUESTION, { requireCitations: true });

    await heartbeatWatch;
    await narrationWatch;

    // The gate holds the prose and shows a COUNT. A heartbeat that
    // leaked the held text would be a correctness failure, not a
    // cosmetic one — assert the shape, not just the presence.
    if (observed.heartbeat !== undefined) {
      expect(
        observed.heartbeat,
        "the synthesis heartbeat must show a running token COUNT, never held content",
      ).toMatch(/\d[\d,]*\s+tokens?/);
      run.note(`synthesis heartbeat during the gated hold: "${observed.heartbeat}"`);
    } else {
      run.note("synthesis heartbeat not observed (synthesis window under one frame)");
    }

    // ── The claim under the beat: this answer is grounded in SEP. ──
    const corpora = [...new Set(facts.citations.map((c) => c.corpus_id))];
    run.note(`retrieval drew from: ${corpora.join(", ")}`);
    expect(
      corpora,
      "unscoped ask over the whole shelf must land on `sep` for a philosophy question",
    ).toContain("sep");

    // Let the finished answer sit on screen long enough to be read.
    await run.park();
    await run.dwell(2400);
    run.mark("answer-settled");

    const footer = page.getByTestId("epistemic-footer").last();
    if (await footer.isVisible().catch(() => false)) {
      run.note(`epistemic footer: "${(await footer.textContent())?.trim().slice(0, 160)}"`);
      await run.dwell(1600);
    }

    // ── The click-through. ──
    // Best-effort by construction: the only UI path from a message to
    // the reading surface is an inline `[Source: …]` marker, and whether
    // the model emits one is model behaviour, not app behaviour. The
    // data-layer citation contract is already proven above by
    // assertTurnInvariants(requireCitations) — every retrieved chunk
    // resolved through read_get_chunk to real text. So a missing marker
    // costs us the shot, not the claim.
    const lastMsg = page.locator(".sv-ai-msg").last();
    const citations = lastMsg.locator(".source-citation");
    if ((await citations.count()) > 0) {
      await run.caption("Every claim dereferences to its source.", 3000);
      run.mark("citation-click");
      await demoClick(page, citations.first(), { settleMs: 600 });
      const surface = page.locator(".reading-surface");
      await expect(
        surface,
        "clicking an inline citation must open the reading surface",
      ).toBeVisible({ timeout: 20_000 });
      await expect(
        surface.locator(".content"),
        "the reading surface must render the cited passage",
      ).toContainText(/\S/);
      run.mark("source-open");
      await run.park();
      await run.dwell(3200); // hold on the source — this is the money frame
      run.note("reading surface opened on the cited SEP passage");
    } else {
      run.note(
        "model emitted no inline [Source:] marker — click-through not filmed. " +
          "Citation resolution still proven at the data layer.",
      );
    }
  },
);
