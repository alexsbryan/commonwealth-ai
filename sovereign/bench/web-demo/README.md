# web-demo — SOLVE × Playwright holdout material

Minimal vite + Playwright app for exercising the solver's browser
path (`docs/specs/SOLVE_PLAYWRIGHT.md`). Two planted gaps:

1. **Failing e2e (the fix path).** `src/main.ts` sets `toast.hidden =
   true` after Save — `tests/e2e/save.spec.ts` fails. `solve <dir>
   "make the save toast appear"` must drive it green.
2. **Unpinned behavior (the pin path).** The Clear button exists in
   the markup but has no handler. With the suite green, `solve <dir>
   "the Clear button empties the note field"` must write a failing
   `tests/e2e/pin.spec.ts`, then implement the handler.

Setup for a live run (copy OUT of the monorepo first — solve wants
its own git root):

    cp -R sovereign/bench/web-demo ~/scratch-web-demo
    cd ~/scratch-web-demo && npm install && npx playwright install chromium
    git init && git add -A && git commit -m demo
