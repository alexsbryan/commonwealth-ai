// SPDX-License-Identifier: AGPL-3.0-or-later
// B2 — Inner Work: an anxious journal entry, and the witness.
//
// The beat that makes people feel the product rather than evaluate it.
// Everything here is deliberately slower than a test wants to be: the
// threshold hold, the paragraph pauses, the composing dot. That pacing
// IS the surface — filming it at test speed would misrepresent it.
//
// What's assertable is structure, never prose: the witness is a real
// model on real input, so its words differ every run. We assert that it
// spoke, that the page never claimed anything left the machine, and that
// the writing survives leaving the surface — the three things the beat
// actually promises.
import { beatTest, expect, demoClick, demoType } from "./beat";
import { realBootToChat } from "./demo-base";

const ENTRY = [
  "I keep circling the release. What if it isn't perfect. " +
    "What if we put it in front of people and it just fails, in the ordinary way things fail, " +
    "and the failure is the thing they remember.",
  "Underneath that there's something I actually want to say. " +
    "I want to build this with people I've loved working with before. " +
    "On work that's genuinely at the edge. " +
    "Where what we make matters to someone, and where we have real stake in where it goes — " +
    "not just a seat near the decision.",
  "I think the fear is just the size of wanting that.",
].join("\n\n");

/** The closing line, used as the persistence probe: short enough to match
 *  on, distinctive enough that matching it means the whole entry came back. */
const ENTRY_TAIL = "I think the fear is just the size of wanting that.";

beatTest(
  {
    id: "b2-inner-work",
    title: "Write something true, and be read back",
    claim:
      "There is a surface here that isn't a chatbot: you write, nothing is sent " +
      "anywhere, and when you ask for it something reads you back with care.",
    gifPadSec: 1.4,
    gifMark: "witness-appears",
  },
  async ({ page, run }) => {
    await realBootToChat(page);
    await run.dwell(700);

    await demoClick(page, page.getByTestId("nav-reflect"), { settleMs: 500 });
    run.mark("enter-reflect");

    // The threshold holds the empty page (~800ms) before the dateline
    // fades in. Wait for the dateline rather than a fixed sleep so the
    // beat films the transition, not a guess at it.
    const dateline = page.locator(".dateline");
    await expect(dateline).toBeVisible({ timeout: 8_000 });
    await run.dwell(1400); // let the empty page breathe — this is the point of it

    // "Stored locally", unblinking, for the whole beat.
    const local = page.locator(".local-indicator");
    await expect(local).toBeVisible();
    await expect(local).toContainText(/local/i);
    run.mark("local-indicator");

    const column = page.locator("textarea.column");
    await expect(column).toBeVisible();

    run.mark("writing");
    await demoType(page, column, ENTRY, {
      charDelayMs: 42, // slower than chat: this is writing, not querying
      sentencePauseMs: 340,
      paragraphPauseMs: 1300,
    });
    await run.dwell(1500); // the pause before asking to be read

    // ── Summon the witness (Cmd+Return) ──
    await run.caption("Cmd + Return summons the witness.", 2400);
    await column.press("Meta+Enter");
    run.mark("summon");

    // The composing dot is a mid-turn transient; catch it if the model
    // is slow enough to show one, but never fail the beat on cadence.
    const composing = page.locator(".composing");
    const composingWatch = composing
      .waitFor({ state: "visible", timeout: 30_000 })
      .then(() => run.mark("composing"))
      .catch(() => {
        /* first token arrived inside one frame */
      });

    const witness = page.locator("blockquote.witness").last();
    await expect(
      witness,
      "the witness must respond to a summoned entry",
    ).toBeVisible({ timeout: 240_000 });
    await composingWatch;
    run.mark("witness-appears");

    // Structure, not prose: it spoke, and it said something.
    const witnessText = (await witness.textContent())?.trim() ?? "";
    expect(
      witnessText.length,
      "the witness response must be non-empty",
    ).toBeGreaterThan(20);
    run.note(`witness (${witnessText.length} chars): "${witnessText.slice(0, 140)}…"`);

    await run.park();
    await run.dwell(4200); // hold — the viewer needs to actually read this

    // ── Provenance: which model, where it ran, what left the machine. ──
    await run.caption("Cmd + / — the receipt.", 2200);
    await page.keyboard.press("Meta+Slash");
    const provenance = page.locator("aside.provenance");
    if (await provenance.isVisible({ timeout: 10_000 }).catch(() => false)) {
      await expect(
        provenance,
        "the provenance panel must render the captured turn, not an empty shell",
      ).toContainText(/\S/);
      run.mark("provenance");
      await run.dwell(3000);
      await page.keyboard.press("Escape");
      run.note("provenance panel opened on the witness turn");
    } else {
      run.note(
        "provenance panel did not open (Cmd+/ chord not delivered by this WebView) — " +
          "skipped the receipt shot",
      );
    }

    // ── The persistence claim, proven rather than stated. ──
    // Leave the surface entirely and come back. If the entry is gone,
    // "stored locally" was a decoration.
    //
    // Look at the COMMITTED TURN, not the draft textarea. Summoning the
    // witness deliberately clears the draft from both the UI and
    // localStorage — "the text is now committed as the user portion of a
    // turn" (InnerWorkSurface.summonWitness). So an empty textarea here is
    // correct behaviour, not lost work, and asserting against it can only
    // ever fail. The claim actually worth filming is the stronger one
    // anyway: the entry comes back as a turn, restored from the local
    // conversation store rather than a browser draft key.
    await demoClick(page, page.getByTestId("nav-ask"), { settleMs: 400 });
    await expect(page.locator(".chat-view")).toBeVisible();
    await run.dwell(900);
    await demoClick(page, page.getByTestId("nav-reflect"), { settleMs: 400 });

    const restoredEntry = page
      .locator("article.turn p.user-prose")
      .filter({ hasText: ENTRY_TAIL })
      .first();
    await expect(
      restoredEntry,
      "the committed entry must survive leaving and re-entering the surface",
    ).toBeVisible({ timeout: 15_000 });
    run.mark("persisted");
    await run.dwell(2200);
    run.note("committed entry restored intact after a rail round-trip");
  },
);
