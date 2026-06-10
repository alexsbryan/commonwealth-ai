// SPDX-License-Identifier: AGPL-3.0-or-later
// Wrapped — a story-card show over the user's own conversation archive.
//
// The whole show reads ONE precomputed artifact (`wrappedArtifact`): every
// number was folded deterministically by the host at build time, every quote
// is a verbatim cited span the host's audit verified before serving. This
// file is pure composition over the SDK — it renders cards, it never computes
// a figure. Unknown card types in the artifact are SKIPPED, so future
// enriched cards (questions, reversals, the arc) ship by appearing in the
// artifact, not by editing every bundle.

import {
  $, connect, hasBridge, emsg, fmtInt, el, clear,
  forceGraph, citationExpander, storyShow, heatGrid,
} from "../_sdk/meshapp.js";

const CORPUS = "conversations-anthropic";
/** Anna Karenina ≈ 350k words — the scale card's one comparator. */
const ANNA_KARENINA_WORDS = 350000;

let bridge;

async function main() {
  if (!hasBridge()) return fail("window.meshApp is not available — the host bridge shim did not load.");
  bridge = connect(CORPUS);

  let artifact;
  try {
    artifact = await bridge.wrappedArtifact();
  } catch (e) {
    return fail(
      "Couldn't read your archive — " + emsg(e) +
      "  (make sure the conversations corpus is installed and this app is allowed to read it.)"
    );
  }

  const deck = deckFromArtifact(artifact);
  if (!deck.length) return fail("The artifact carried no cards this build could show.");

  $("loading").hidden = true;
  storyShow($("show"), deck, { label: "your conversations, wrapped" });
}

function fail(msg) {
  $("loading").hidden = true;
  const err = $("error");
  err.hidden = false;
  err.textContent = msg;
}

/** Artifact cards → renderable deck. Unknown types are skipped by design. */
function deckFromArtifact(artifact) {
  const renderers = {
    scale: renderScale,
    rhythm: renderRhythm,
    obsessions: renderObsessions,
    cast: renderCast,
    door: renderDoor,
  };
  const deck = [];
  for (const card of artifact.cards || []) {
    const render = renderers[card.type];
    if (!render) continue; // forward-compat: future card types no-op here
    deck.push({
      id: card.type,
      type: card.type,
      skippable: card.type !== "door",
      render: (slide, ctx) => render(slide, card, ctx),
    });
  }
  return deck;
}

// ─── Card renderers — one number or one quote per card ───────────────

function renderScale(slide, card) {
  const novels = card.words_total / ANNA_KARENINA_WORDS;
  slide.appendChild(el("div", { class: "story-kicker", text: "Your archive" }));
  slide.appendChild(el("div", { class: "story-big", text: fmtInt(card.conversations) }));
  slide.appendChild(el("div", {
    class: "story-line",
    text: "conversations across " + fmtInt(card.months_active) + " months — " +
      fmtInt(card.words_total) + " words between you (" + fmtInt(card.words_user) +
      ") and the machine (" + fmtInt(card.words_assistant) + ").",
  }));
  if (novels >= 1) {
    slide.appendChild(el("div", {
      class: "story-sub",
      text: "That's Anna Karenina, " + (novels >= 2 ? Math.floor(novels) + " times over" : "once") +
        " — from " + card.first_date + " to " + card.last_date + ".",
    }));
  } else {
    slide.appendChild(el("div", { class: "story-sub", text: "From " + card.first_date + " to " + card.last_date + "." }));
  }
  slide.appendChild(el("div", {
    class: "story-sub",
    text: "Every figure on every card was computed on this machine — tap any quote to read the conversation it came from.",
  }));
}

function renderRhythm(slide, card) {
  slide.appendChild(el("div", { class: "story-kicker", text: "When you think" }));
  slide.appendChild(el("div", {
    class: "story-line",
    text: fmtInt(card.total_turns) + " turns, hour by hour — by the archive's clock (UTC).",
  }));
  heatGrid(slide, card.heatmap);

  const s = card.longest_session;
  if (!s) return;
  const hours = Math.floor(s.duration_minutes / 60);
  const mins = s.duration_minutes % 60;
  const parts = [];
  if (hours) parts.push(hours + (hours === 1 ? " hour" : " hours"));
  if (mins || !hours) parts.push(mins + " minutes");
  slide.appendChild(el("div", {
    class: "story-line",
    text: "Your longest rabbit hole: " + parts.join(" ") + " and " +
      s.turns + " turns on " + s.date + (s.title ? " — “" + s.title + "”" : "") + ".",
  }));
  if (s.excerpt) {
    slide.appendChild(el("div", { class: "story-sub", text: "It started with your words:" }));
    slide.appendChild(el("pre", { class: "excerpt", text: s.excerpt.text }));
  }
  const drill = el("div", { "data-no-advance": "" });
  for (const id of (s.chunk_ids || []).slice(0, 3)) {
    drill.appendChild(citationExpander(bridge, id, { label: "where it went" }));
  }
  slide.appendChild(drill);
}

function renderObsessions(slide, card) {
  slide.appendChild(el("div", { class: "story-kicker", text: "Your obsessions, by the quarter" }));
  slide.appendChild(el("div", {
    class: "story-line",
    text: "What kept showing up — counted across distinct conversations, each with a citation.",
  }));
  const wrap = el("div", { class: "quarters", "data-no-advance": "" });
  for (const q of card.quarters) {
    const box = el("div", { class: "card" });
    box.appendChild(el("div", { class: "label", text: q.quarter }));
    for (const t of q.topics) {
      const row = el("div", { class: "edge" });
      row.appendChild(el("div", { class: "edge-head" },
        el("span", { class: "rel", text: t.text }),
        " · " + fmtInt(t.conversations) + (t.conversations === 1 ? " conversation" : " conversations")));
      row.appendChild(citationExpander(bridge, t.sample.chunk_id, { label: "one of them" }));
      box.appendChild(row);
    }
    wrap.appendChild(box);
  }
  // Scrollable region inside the slide so long histories stay one card.
  wrap.style.maxHeight = "46vh";
  wrap.style.overflow = "auto";
  slide.appendChild(wrap);
}

function renderCast(slide, card) {
  slide.appendChild(el("div", { class: "story-kicker", text: "The cast" }));
  slide.appendChild(el("div", {
    class: "story-line",
    text: "The people, projects and works that recur — linked when they share your conversations.",
  }));
  const map = el("div", { class: "map", "data-no-advance": "" });
  const detail = el("div", { "data-no-advance": "" });
  slide.appendChild(map);
  slide.appendChild(detail);
  forceGraph(map, { nodes: card.nodes, edges: card.edges }, {
    onNodeClick: (id) => {
      const n = card.nodes.find((x) => x.id === id);
      if (!n) return;
      clear(detail);
      detail.appendChild(el("div", {
        class: "story-sub",
        text: n.canonical_name + " — " + fmtInt(n.conversations) +
          (n.conversations === 1 ? " conversation" : " conversations") + ".",
      }));
      detail.appendChild(citationExpander(bridge, n.sample.chunk_id, { label: "where they appear" }));
    },
  });
}

function renderDoor(slide, card, ctx) {
  slide.appendChild(el("div", { class: "story-kicker", text: "The door" }));
  slide.appendChild(el("div", { class: "story-line", text: "Your archive is now your memory." }));
  slide.appendChild(el("div", {
    class: "story-sub",
    text: "The same corpus that just told you who you've been is standing by to answer " +
      "“what did I decide about that, again?” — cited, honest about what it can't find, " +
      "on hardware you own.",
  }));
  // The funnel: open Outer Work on a fresh conversation scoped to this
  // corpus. Outside the desktop (dev server) the op errors — fall back
  // to copy instead of a dead button.
  const note = el("div", { class: "story-sub" });
  const cta = el("button", {
    type: "button",
    class: "story-cta",
    text: "Ask your past self →",
    onClick: async (e) => {
      e.stopPropagation();
      try {
        await bridge.openOuterWork();
      } catch (err) {
        note.textContent = "Open this in the desktop app to ask — " + emsg(err);
      }
    },
  });
  slide.appendChild(cta);
  slide.appendChild(note);
  slide.appendChild(el("div", {
    class: "story-sub",
    text: "Computed on this machine. Uploaded nowhere.",
  }));
}

main();
