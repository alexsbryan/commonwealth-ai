// SPDX-License-Identifier: AGPL-3.0-or-later
// MeshApp SDK — story-card shell + hour-of-week heat grid.
//
// `storyShow` is the lean-back full-screen card sequence (the Wrapped form):
// one reveal per card, progress dots, keyboard/click/swipe navigation, and a
// per-card Skip affordance (the sensitivity escape hatch — skipping marks the
// dot, it never blocks the deck). Cards render lazily on first show, so a
// deck over a precomputed artifact paints instantly and works offline.
//
// CSP-safe like every SDK view: DOM by construction via `el()`, no canvas,
// no innerHTML.

import { el, clear } from "./dom.js";

/**
 * Full-screen story deck rendered into `container`.
 *
 * `cards`: `[{ id, type, skippable = true, render(slide, ctx) }]` — `render`
 * fills the provided slide element (called once, on first show; may be async).
 * `ctx` is `{ next, prev, goTo, index }` so a card can advance the show (e.g.
 * a Door card's call-to-action).
 *
 * Nav: → / ␣ / click right third = next; ← / click left third = back;
 * Esc calls `opts.onClose`. Pointer swipe works on touch. Returns
 * `{ next, prev, goTo, index, destroy }`.
 */
export function storyShow(container, cards, opts = {}) {
  clear(container);
  const root = el("div", { class: "story-show", tabindex: "0", role: "region", "aria-label": opts.label || "story" });
  const stage = el("div", { class: "story-stage" });
  const dots = el("div", { class: "story-dots", role: "tablist" });
  root.appendChild(stage);
  root.appendChild(dots);
  container.appendChild(root);

  let current = -1;
  const slides = [];
  const dotEls = [];
  const rendered = new Set();

  const ctx = {
    next: () => goTo(current + 1),
    prev: () => goTo(current - 1),
    goTo: (i) => goTo(i),
    get index() {
      return current;
    },
  };

  for (let i = 0; i < cards.length; i++) {
    const card = cards[i];
    const slide = el("div", { class: "story-slide", "data-card": card.type || card.id || String(i) });
    slides.push(slide);
    stage.appendChild(slide);
    const dot = el("button", {
      type: "button",
      class: "story-dot",
      role: "tab",
      "aria-label": "card " + (i + 1),
      onClick: () => goTo(i),
    });
    dotEls.push(dot);
    dots.appendChild(dot);
  }

  function goTo(i) {
    if (i < 0 || i >= cards.length || i === current) return;
    current = i;
    for (let k = 0; k < slides.length; k++) {
      slides[k].classList.toggle("active", k === i);
      if (k === i) dotEls[k].setAttribute("aria-current", "true");
      else dotEls[k].removeAttribute("aria-current");
    }
    if (!rendered.has(i)) {
      rendered.add(i);
      const card = cards[i];
      const body = el("div", { class: "story-body" });
      slides[i].appendChild(body);
      if (card.skippable !== false) {
        slides[i].appendChild(
          el("button", {
            type: "button",
            class: "story-skip",
            text: "Skip",
            onClick: (e) => {
              e.stopPropagation();
              dotEls[i].classList.add("skipped");
              if (opts.onSkip) opts.onSkip(card, i);
              ctx.next();
            },
          })
        );
      }
      Promise.resolve(card.render(body, ctx)).catch((err) => {
        clear(body);
        body.appendChild(el("div", { class: "story-error", text: "this card failed to render: " + (err && err.message ? err.message : String(err)) }));
      });
    }
    if (opts.onShow) opts.onShow(cards[i], i);
  }

  const onKey = (e) => {
    if (e.key === "ArrowRight" || e.key === " ") {
      e.preventDefault();
      ctx.next();
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      ctx.prev();
    } else if (e.key === "Escape" && opts.onClose) {
      opts.onClose();
    }
  };
  root.addEventListener("keydown", onKey);

  // Click left/right thirds — but never when the click landed on an
  // interactive element inside the card (links, expanders, skip).
  stage.addEventListener("click", (e) => {
    if (e.target.closest("button, a, input, textarea, [data-no-advance]")) return;
    const r = stage.getBoundingClientRect();
    const x = (e.clientX - r.left) / r.width;
    if (x < 1 / 3) ctx.prev();
    else if (x > 2 / 3) ctx.next();
  });

  // Pointer swipe.
  let downX = null;
  stage.addEventListener("pointerdown", (e) => {
    downX = e.clientX;
  });
  stage.addEventListener("pointerup", (e) => {
    if (downX == null) return;
    const dx = e.clientX - downX;
    downX = null;
    if (Math.abs(dx) < 48) return;
    if (dx < 0) ctx.next();
    else ctx.prev();
  });

  goTo(0);
  root.focus();

  return {
    next: ctx.next,
    prev: ctx.prev,
    goTo,
    get index() {
      return current;
    },
    destroy: () => {
      root.removeEventListener("keydown", onKey);
      clear(container);
    },
  };
}

const DAY_LABELS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/**
 * Hour-of-week heat grid (sister of `timelineChart`). `grid`: 7 rows
 * (Mon..Sun) × 24 columns of counts. Each cell is a button with a
 * `data-i="0..4"` intensity bucket; `opts.onCell(day, hour, count)` makes it
 * drillable, `opts.title(day, hour, count)` overrides the hover text.
 */
export function heatGrid(container, grid, opts = {}) {
  clear(container);
  const max = Math.max(1, ...grid.flat());
  const wrap = el("div", { class: "heat-grid", role: "img", "aria-label": opts.label || "activity by hour of week" });
  for (let d = 0; d < 7; d++) {
    const row = el("div", { class: "heat-row" });
    row.appendChild(el("span", { class: "heat-day", text: (opts.rowLabels || DAY_LABELS)[d] }));
    for (let h = 0; h < 24; h++) {
      const count = (grid[d] && grid[d][h]) || 0;
      const bucket = count === 0 ? 0 : Math.min(4, 1 + Math.floor((count / max) * 3.999));
      const title = opts.title
        ? opts.title(d, h, count)
        : `${(opts.rowLabels || DAY_LABELS)[d]} ${String(h).padStart(2, "0")}:00 — ${count}`;
      const cell = el("button", {
        type: "button",
        class: "heat-cell",
        "data-i": String(bucket),
        "data-day": String(d),
        "data-hour": String(h),
        title,
        "aria-label": title,
        onClick: (e) => {
          e.stopPropagation();
          if (opts.onCell) opts.onCell(d, h, count);
        },
      });
      row.appendChild(cell);
    }
    wrap.appendChild(row);
  }
  container.appendChild(wrap);
}
