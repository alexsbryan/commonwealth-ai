// SPDX-License-Identifier: AGPL-3.0-or-later
// Wrapped — a story-card show over the user's own conversation archive.
//
// The whole show reads ONE precomputed artifact (`wrappedArtifact`): every
// number was folded deterministically by the host at build time, every quote
// is a verbatim cited span the host's audit verified before serving. This
// file is pure composition over the SDK — it renders cards, it never computes
// a figure. Unknown card types in the artifact are SKIPPED, so future
// enriched cards ship by appearing in the artifact, not by editing every
// bundle.
//
// Every card that makes a CLAIM (rather than reporting a count) carries a
// `derivation` from the host, rendered behind "why this?". A claim the reader
// can interrogate is the difference between a system that looks clever and
// one that is trustworthy.

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
    recurring: renderRecurring,
    turn: renderTurn,
    obsessions: renderObsessions,
    night_shift: renderNightShift,
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

// ─── Shared bits ─────────────────────────────────────────────────────

/**
 * The host's derivation trace, collapsed. Present on every card that
 * asserts something about the reader rather than merely counting.
 */
function whyThis(derivation) {
  const wrap = el("div", { "data-no-advance": "" });
  if (!derivation || !derivation.length) return wrap;
  const body = el("ul", { class: "meta" });
  body.hidden = true;
  for (const line of derivation) body.appendChild(el("li", { text: line }));
  const toggle = el("button", {
    type: "button",
    class: "link",
    text: "▸ why this?",
    onClick: (e) => {
      e.stopPropagation();
      body.hidden = !body.hidden;
      toggle.textContent = (body.hidden ? "▸" : "▾") + " why this?";
    },
  });
  wrap.appendChild(el("div", { class: "prov" }, toggle));
  wrap.appendChild(body);
  return wrap;
}

/**
 * A span, rendered the way a person would say it. Stays specific — the
 * whole reveal of the recurring card is the DISTANCE, so "13 months"
 * earns its place where a vaguer "over a year" would throw it away.
 */
function saySpan(days) {
  if (days >= 730) return Math.round(days / 365) + " years";
  if (days >= 60) return Math.round(days / 30.4) + " months";
  return days + (days === 1 ? " day" : " days");
}

/** A scrollable region that stays inside one card. */
function scrollBox(maxHeight = "46vh") {
  const box = el("div", { "data-no-advance": "" });
  box.style.maxHeight = maxHeight;
  box.style.overflow = "auto";
  return box;
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
  slide.appendChild(whyThis(card.derivation));
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

/**
 * The question you keep asking. The reveal is the DISTANCE — same
 * question, different words, months apart — so the span leads and the
 * verbatim askings carry it.
 */
function renderRecurring(slide, card) {
  const top = card.threads[0];
  slide.appendChild(el("div", { class: "story-kicker", text: "The question you keep asking" }));
  slide.appendChild(el("div", {
    class: "story-line",
    text: "You came back to this " + top.conversations + " times over " +
      saySpan(top.span_days) + " — in different words each time.",
  }));

  const box = scrollBox("52vh");
  card.threads.forEach((thread, i) => {
    const group = el("div", { class: "card" });
    group.appendChild(el("div", { class: "label" },
      el("span", { class: "rel", text: thread.conversations + " conversations" }),
      el("span", { class: "chip", text: saySpan(thread.span_days) })));
    for (const asking of thread.askings) {
      const row = el("div", { class: "edge" });
      row.appendChild(el("div", { class: "meta", text: asking.date }));
      row.appendChild(el("pre", { class: "excerpt", text: asking.excerpt.text }));
      row.appendChild(citationExpander(bridge, asking.excerpt.chunk_id, { label: "that conversation" }));
      group.appendChild(row);
    }
    if (i > 0) group.style.opacity = "0.85";
    box.appendChild(group);
  });
  slide.appendChild(box);
  slide.appendChild(whyThis(card.derivation));
}

/**
 * The turn. One conversation, one seam, both sides quoted — the card
 * that has to feel like the system read you, so it shows the single
 * strongest pivot big and files the rest underneath.
 */
function renderTurn(slide, card) {
  const p = card.pivots[0];
  slide.appendChild(el("div", { class: "story-kicker", text: "The turn" }));
  slide.appendChild(el("div", {
    class: "story-line",
    text: "On " + p.date + ", " + (p.seam_index > 1 ? p.seam_index + " stretches" : "one stretch") +
      " into " + (p.title ? "“" + p.title + "”" : "a conversation") +
      ", it stopped being about the same thing.",
  }));

  const pivotBlock = (pivot, lead) => {
    const wrap = el("div", { class: "card", "data-no-advance": "" });
    if (lead !== undefined) {
      wrap.appendChild(el("div", { class: "label" },
        el("span", { class: "rel", text: pivot.date }),
        pivot.title ? "“" + pivot.title + "”" : ""));
    }
    if (pivot.before) {
      wrap.appendChild(el("div", { class: "meta", text: "before" }));
      wrap.appendChild(el("pre", { class: "excerpt", text: pivot.before.text }));
    }
    if (pivot.after) {
      wrap.appendChild(el("div", { class: "meta", text: "after" }));
      wrap.appendChild(el("pre", { class: "excerpt", text: pivot.after.text }));
    }
    const cite = pivot.after || pivot.before;
    if (cite) wrap.appendChild(citationExpander(bridge, cite.chunk_id, { label: "where it went" }));
    return wrap;
  };

  slide.appendChild(pivotBlock(p));

  if (card.pivots.length > 1) {
    slide.appendChild(el("div", { class: "story-sub", text: "It happened elsewhere too:" }));
    const box = scrollBox("28vh");
    for (const other of card.pivots.slice(1)) box.appendChild(pivotBlock(other, true));
    slide.appendChild(box);
  }
  slide.appendChild(whyThis(card.derivation));
}

function topicRow(topic, unit) {
  const row = el("div", { class: "edge" });
  row.appendChild(el("div", { class: "edge-head" },
    el("span", { class: "rel", text: topic.text }),
    " · " + fmtInt(topic.conversations) + " " + unit +
      (topic.conversations === 1 ? "" : "s")));
  row.appendChild(citationExpander(bridge, topic.sample.chunk_id, { label: "one of them" }));
  return row;
}

function renderObsessions(slide, card) {
  slide.appendChild(el("div", { class: "story-kicker", text: "Your obsessions, by the quarter" }));
  slide.appendChild(el("div", {
    class: "story-line",
    text: "Not what you talked about most — what each quarter was unusually about, " +
      "measured against everything else in the archive.",
  }));
  const wrap = scrollBox();
  wrap.classList.add("quarters");
  for (const q of card.quarters) {
    const box = el("div", { class: "card" });
    box.appendChild(el("div", { class: "label", text: q.quarter }));
    for (const t of q.topics) box.appendChild(topicRow(t, "conversation"));
    wrap.appendChild(box);
  }
  slide.appendChild(wrap);
  slide.appendChild(whyThis(card.derivation));
}

/**
 * The night shift. The claim ("after midnight you are a different
 * person") is only true in the reader's OWN time, so the offset the host
 * inferred is stated on the card, not buried.
 */
function renderNightShift(slide, card) {
  const sign = card.utc_offset_hours >= 0 ? "+" : "";
  slide.appendChild(el("div", { class: "story-kicker", text: "The night shift" }));
  slide.appendChild(el("div", {
    class: "story-line",
    text: "You are not the same thinker at 2am as at 2pm — and the archive can prove it.",
  }));
  const box = scrollBox("48vh");
  for (const band of card.bands) {
    const group = el("div", { class: "card" });
    group.appendChild(el("div", { class: "label" },
      el("span", { class: "rel", text: band.name }),
      el("span", { class: "chip", text: String(band.start_hour).padStart(2, "0") + ":00–" +
        String(band.end_hour).padStart(2, "0") + ":59" })));
    for (const t of band.topics) group.appendChild(topicRow(t, "mention"));
    box.appendChild(group);
  }
  slide.appendChild(box);
  slide.appendChild(el("div", {
    class: "story-sub",
    text: "Hours are your local clock (UTC" + sign + card.utc_offset_hours +
      "); the archive itself stamps UTC.",
  }));
  slide.appendChild(whyThis(card.derivation));
}

function renderCast(slide, card) {
  slide.appendChild(el("div", { class: "story-kicker", text: "The cast" }));
  slide.appendChild(el("div", {
    class: "story-line",
    text: "The people, places and ideas that recur — linked only where they turn up " +
      "together more than chance would put them.",
  }));
  const map = el("div", { class: "map", "data-no-advance": "" });
  const detail = el("div", { "data-no-advance": "" });
  slide.appendChild(map);
  slide.appendChild(detail);

  const byId = new Map(card.nodes.map((n) => [n.id, n]));
  forceGraph(map, { nodes: card.nodes, edges: card.edges }, {
    // Size by bridging, not frequency: the story of an archive is who
    // connects otherwise-separate concerns.
    size: (n) => n.bridging || 0,
    describe: (n) => n.first_date
      ? n.conversations + " conversations, " + n.first_date + " → " + n.last_date
      : n.conversations + " conversations",
    onNodeClick: (id) => {
      const n = byId.get(id);
      if (!n) return;
      clear(detail);
      detail.appendChild(el("div", {
        class: "story-sub",
        text: n.canonical_name + " — " + fmtInt(n.conversations) +
          (n.conversations === 1 ? " conversation" : " conversations") +
          (n.first_date ? ", " + n.first_date + " to " + n.last_date : "") + ".",
      }));
      const links = card.edges
        .filter((e) => e.source === id || e.target === id)
        .sort((a, b) => b.pmi - a.pmi)
        .slice(0, 4);
      for (const e of links) {
        const other = byId.get(e.source === id ? e.target : e.source);
        if (!other) continue;
        detail.appendChild(el("div", { class: "meta",
          text: "with " + other.canonical_name + " — " + e.co_conversations +
            (e.co_conversations === 1 ? " shared conversation" : " shared conversations") +
            ", " + e.first_date + " → " + e.last_date }));
      }
      detail.appendChild(citationExpander(bridge, n.sample.chunk_id, { label: "where they appear" }));
    },
  });
  slide.appendChild(whyThis(card.derivation));
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
