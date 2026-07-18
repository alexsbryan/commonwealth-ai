# Product process (maintainer notes)

**Goal: keep one surface trustworthy.** The issue list is only things we intend
to touch. Discussions + upvotes + the board do the sorting; we triage in weekly
batches, never per-notification.

## Two front doors

- **Discussions** — Ideas (upvotable), Q&A, Announcements. The community sorts it.
- **Issues** — confirmed bugs + *accepted* work only. An idea becomes an issue
  when we decide to act ("Create issue from discussion").

## Weekly triage pass (Mondays, one sitting)

Walk the board's Triage column / clear `needs-triage`. For each issue:

- Real + we'll do it → `accepted` + priority, onto **Next up** (short) or **Backlog**.
- Need more from reporter → `needs-info` (the stale bot chases it, not you).
- Out of scope → close with the saved reply linking `NON-GOALS.md`.
- Duplicate → close as dup.

Then promote any upvoted Idea worth doing, and sweep `blocked` for anything freed.

**Our own findings** (robustness runs, dogfooding): file with `source: dogfood`,
skip triage, straight to `accepted`. Keeps "what users hit" separate from "what
we found" when deciding release headlines.

## Prioritizing (no scheme)

Two questions: **how badly does it hurt, and how many?** and **what's it cost to
fix?** Express it as board position, not a label zoo. Keep **Next up** short — if
everything's Next up, nothing is.

## The board (one-time setup — needs a Projects token)

Columns: **Triage → Backlog → Next up → In progress → Done.**

1. `gh auth refresh -s project` then `gh project create --owner alexsbryan --title "Roadmap"`.
2. Wire auto-add: repo secret `ADD_TO_PROJECT_PAT` (PAT, Projects read/write) +
   repo variable `TRIAGE_PROJECT_URL`. Until both exist the workflow no-ops.
3. Board Workflows → "Item closed → Done."
4. At launch: make one view public — that's the roadmap.

Discussion categories (UI, no API): **Ideas** (open), **Q&A** (question/answer),
**Announcements** (maintainer-only). Slugs `ideas` / `q-a` — the config links
assume them.

## Release comms

- **GitHub Release per tag** = canonical changelog. Headline in user terms
  ("~25% faster on long questions", not "grounding-gate rewrite"), then
  Changed / Fixed / Known issues.
- **Announcements discussion** for anything users care about; pin the current one.
- **State known issues plainly** + a tracking issue — pre-empts duplicate reports.

## Automated vs. on you

| Committed here | One-time, you |
|---|---|
| Forms apply `needs-triage`; auto-add to board (once wired); narrow stale bot; Discussions on | Create board + wire the two vars; create 3 Discussion categories; add saved replies; public board view at launch |
