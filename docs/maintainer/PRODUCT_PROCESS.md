# Running the product process (maintainer notes)

This is the internal counterpart to `CONTRIBUTING.md` and `SUPPORT.md`. Those
tell contributors how to file; this tells us how to keep the thing sane with a
very small team. The whole design goal is that the *reporter* and the *board*
absorb the sorting work, and we touch it in batches — never per-notification.

The through-line: **keep one surface trustworthy.** The issue list is a short,
honest list of things we actually intend to touch — not a 400-item graveyard.
Everything below serves that.

---

## The two front doors

| Surface | What lives there | Who sorts it |
|---|---|---|
| **Discussions** | Ideas (upvotable), Q&A, Announcements | The community, via upvotes and answers |
| **Issues** | Confirmed bugs + *accepted* work only | Us, in the weekly pass |

Raw ideas and questions land in Discussions. An item only becomes an **issue**
when we've decided to act on it. That's what keeps the issue list meaningful:
if it's open, it's real and intended. To act on a popular idea, open an issue
from it ("Create issue from discussion" on the discussion) and close the loop
back on the thread.

### Discussion categories to set up

Discussions is enabled, but the categories are configured in the UI (no API for
it). Under **Discussions → categories**, aim for exactly these — resist adding
more:

- **Ideas** — format: *Announcement*? No, leave as **Open discussion** so it
  keeps upvoting. This is the feature-request funnel.
- **Q&A** — format: **Question / Answer**, so answers can be marked. This is
  where support goes; the community fields most of it.
- **Announcements** — format: **Announcement** (only maintainers post). Release
  notes and heads-ups go here.

The issue-form `config.yml` already deep-links Ideas and Q&A, so once the
categories exist with those slugs (`ideas`, `q-a`) the "New issue" chooser
routes people correctly.

---

## The weekly triage pass

Once a week (the stale bot and this doc assume **Monday**), one sitting:

1. **Clear `needs-triage`.** Every new issue carries it. For each: is it real,
   reproducible, in scope?
   - Real and we'll do it → add **`accepted`**, remove `needs-triage`, set a
     priority (below), drop it on the board's *Backlog* or *Next up*.
   - Real but need more from the reporter → **`needs-info`**, ask the question.
     The stale bot handles the ones that go quiet — you don't chase them.
   - Out of scope → close with the **out-of-scope saved reply** linking
     `docs/NON-GOALS.md`. One line, no relitigating.
   - Duplicate → close as duplicate of #N (saved reply).
2. **Promote from Discussions.** Any Idea that's gathered upvotes and that we
   actually want → open an issue from it, `accepted`, onto the board.
3. **Sweep `blocked`.** Anything unblocked? Move it back to Next up.

That's it. Between passes, notifications can wait. State the cadence publicly
(SUPPORT.md already says "triaged weekly") so nobody feels ignored at day two.

### Our own findings (the dogfood stream)

A lot of what we file comes from robustness runs and dogfooding, not outside
reports. File those as normal issues but add **`source: dogfood`** — it keeps
"what users hit" separable from "what we found ourselves," which matters when
you're deciding what a *release* should headline. These can skip `needs-triage`
(you already triaged it by filing it) and go straight to `accepted` + priority.

---

## Prioritizing without a heavyweight scheme

No story points, no elaborate matrix. Two questions settle almost everything:

- **How badly does it hurt, and how many people?** A crash or data-loss risk or
  a broken install beats a papercut, every time. A papercut a lot of people hit
  beats a rare one.
- **What's it cost us to fix?** A cheap fix that helps a little can still jump
  the queue on a slow week.

Translate that into board position, not a label zoo: **Next up** is the
committed short list (keep it genuinely short — a handful), **Backlog** is
everything accepted-but-later. If you want an explicit severity signal for the
worst ones, use the board's priority field rather than minting `p0/p1/p2`
labels — fewer labels, same information, and the board is where you're looking
anyway.

The honest discipline: if *everything* is Next up, nothing is. The short list
is a promise; keep it small enough to mean something.

---

## The board (Projects v2)

One board is the roadmap and the workbench. Columns:

**Triage → Backlog → Next up → In progress → Done.**

### One-time setup (needs a Projects token — do this by hand)

The `repo`-scoped token I run with can't create or write user Projects, so this
part is on you (5 minutes, once):

1. **Create the board.** `gh project create --owner alexsbryan --title "Commonwealth — Roadmap"`
   (needs a token with `project` scope: `gh auth refresh -s project`), or click
   **Projects → New project → Board** on your profile. Add the five columns
   above.
2. **Auto-add new issues.** The workflow `.github/workflows/add-to-project.yml`
   is already committed and waiting. To turn it on:
   - Make a fine-grained PAT with **read/write on Projects**, save it as the
     repo secret **`ADD_TO_PROJECT_PAT`**.
   - Save the board URL as the repo variable **`TRIAGE_PROJECT_URL`**
     (`gh variable set TRIAGE_PROJECT_URL --body "https://github.com/users/alexsbryan/projects/N"`).
   - Until both exist the workflow no-ops — no red X's.
3. **Built-in workflows.** In the board's own **Workflows** settings, turn on
   "Item closed → set status Done" so closing an issue files itself.
4. **Make one view public** once the repo goes public — that view *is* the
   published roadmap, and it answers "is X coming?" without you typing a word.

### Sub-issues for big things

Break a large feature into child issues (GitHub's native sub-issues) so the
board shows real progress instead of one stalled monster issue. The parent is
the roadmap line; the children are the work.

---

## Saved replies (paste these into GitHub once)

Saved replies are per-account and can't be committed as files — the text lives
in [`saved-replies.md`](./saved-replies.md) next door. Add them under
**Settings → Saved replies** on your account. They turn the hundred repeated
answers (out-of-scope, duplicate, needs-info, PRs-welcome) into two clicks, and
consistency here is what makes batched triage fast.

---

## Release communications

Releases already build from `.github/workflows/*-release.yml`. The comms layer
on top is light:

- **GitHub Release per tag** is the canonical changelog. Keep the shape
  consistent: a one-line **headline** (what a user would notice), then
  **Changed / Fixed / Known issues**. Write it for someone who runs the thing,
  not someone reading the commit log — "answers come back ~25% faster on long
  questions" beats "surgical grounding-gate rewrite." The engineering detail can
  live below a fold for the curious.
- **Announce in Discussions → Announcements** for anything users would care
  about, linking the release. Pin the current one. This is the "what's new"
  people actually read.
- **Known issues, stated plainly.** If something shipped with a rough edge, say
  so in the release and open a tracking issue. Honesty here buys enormous
  goodwill and pre-empts a wave of duplicate reports.
- **Cadence over surprise.** A predictable "here's what shipped" rhythm beats
  sporadic big-bang posts — it sets the same expectation the weekly triage does:
  this project is tended, on a known beat.

A `RELEASE_NOTES` scaffold or a `release-drafter` action can automate the first
draft later; for now the discipline is the format and the user-facing voice, not
the tooling.

---

## What's automated vs. what's on you

| Automated (committed here) | Manual (one-time, you) |
|---|---|
| New issues get `needs-triage` (issue forms) | Create the Project board |
| New issues auto-add to board (once wired) | Wire `ADD_TO_PROJECT_PAT` + `TRIAGE_PROJECT_URL` |
| Area labels on PRs (existing labeler) | Create the 3 Discussion categories |
| `needs-info` issues stale-close narrowly | Add the saved replies to your account |
| Discussions enabled | Make a board view public at launch |
