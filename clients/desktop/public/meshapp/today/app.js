// Today — Wikipedia's Current Events portal as a local, dated feed.
//
// One SDK read (`documentFeed`) drives the app. Each source document is
// a portal day (source_doc_id = YYYY-MM-DD, newest first); its text is
// parsed back into the portal's real structure — section headings (a
// fixed vocabulary), topic breadcrumbs (short link-chain lines), and
// event sentences (which carry their press attributions in trailing
// parens) — so the feed reads as news, not as a wall of text.
import { $, clear, el, emsg, hasBridge, connect } from "../_sdk/meshapp.js";

const CORPUS = "wikipedia-newsworthy";
const DAYS = 14;

// The portal's canonical section headings (plus observed variants).
const SECTIONS = new Set([
  "Armed conflicts and attacks",
  "Arts and culture",
  "Business and economy",
  "Business and economics",
  "Disasters and accidents",
  "Health and environment",
  "Health and medicine",
  "International relations",
  "Law and crime",
  "Politics and elections",
  "Science and technology",
  "Sports",
]);

/** Display-side bandage over the logged ingest double-decode bug:
 *  "â€“"/"Ã³" are UTF-8 bytes mis-read as Latin-1. The
 *  escape→decodeURIComponent round-trip inverts exactly that class;
 *  clean text passes through untouched (guard regex), and anything the
 *  round-trip can't represent throws and falls back to the original. */
function demojibake(s) {
  if (!/[ÃÂâ]/.test(s)) return s;
  try {
    return decodeURIComponent(escape(s));
  } catch {
    return s;
  }
}

/** Pull trailing "(Source)" groups off an event sentence. */
function splitSources(line) {
  const sources = [];
  let text = line.trim();
  for (;;) {
    const m = text.match(/\(([^()]{2,60})\)\s*$/);
    if (!m) break;
    sources.unshift(m[1]);
    text = text.slice(0, m.index).trim();
  }
  return { text, sources };
}

/** Heuristic: an event sentence is long or ends a sentence; breadcrumb
 *  topic lines are short, unpunctuated wikilink chains. */
function isEventSentence(line) {
  return /[.!?]\)?\s*$/.test(line) || line.length > 110;
}

/** Parse one day-document's flattened text back into
 *  [{section, items: [{kickers, text, sources}]}]. */
function parseDay(content) {
  const out = [];
  let section = null;
  let kickers = [];
  const push = (item) => {
    if (!out.length || out[out.length - 1].section !== section) {
      out.push({ section: section ?? "Events", items: [] });
    }
    out[out.length - 1].items.push(item);
  };
  for (const raw of content.split("\n")) {
    const line = demojibake(raw.trim());
    if (!line) continue;
    if (SECTIONS.has(line)) {
      section = line;
      kickers = [];
      continue;
    }
    if (isEventSentence(line)) {
      const { text, sources } = splitSources(line);
      push({ kickers, text, sources });
      kickers = [];
    } else {
      kickers.push(line);
      if (kickers.length > 4) kickers.shift();
    }
  }
  return out;
}

function humanDate(iso) {
  const d = new Date(`${iso}T12:00:00Z`);
  return isNaN(d)
    ? iso
    : d.toLocaleDateString(undefined, {
        weekday: "long",
        year: "numeric",
        month: "long",
        day: "numeric",
      });
}

function wikiUrl(title) {
  return `https://en.wikipedia.org/wiki/${encodeURIComponent(title.replaceAll(" ", "_"))}`;
}

async function openPanel(bridge, title) {
  const panel = $("panel");
  panel.hidden = false;
  $("p-title").textContent = title;
  $("p-body").textContent = "Looking in the atlas…";
  const actions = $("p-actions");
  clear(actions);
  actions.append(
    el("button", { text: "Ask about this (grounded chat)", onclick: () => bridge.openOuterWork?.() }),
    el(
      "a",
      { href: wikiUrl(title), target: "_blank", rel: "noreferrer" },
      el("button", { text: "Read on Wikipedia ↗" }),
    ),
  );
  panel.scrollIntoView({ behavior: "smooth", block: "nearest" });
  try {
    const hits = await bridge.search(title, null, 3);
    const hit = (hits || []).find((h) => h.display_name === title) || (hits || [])[0];
    if (!hit) {
      $("p-body").textContent =
        "Not in the atlas yet — enrichment tracks entities as days accumulate. The links above still work.";
      return;
    }
    const node = await bridge.node(hit.atom_id ?? hit.id);
    $("p-body").textContent =
      node?.description || node?.summary || "In the atlas — no description yet.";
  } catch (e) {
    $("p-body").textContent = `Atlas lookup unavailable: ${emsg(e)}`;
  }
}

function renderDay(bridge, doc) {
  const day = el("section", { class: "day" }, el("h2", { text: humanDate(doc.source_doc_id) }));
  const linkSet = new Set();
  for (const chunk of doc.chunks) {
    for (const group of parseDay(chunk.content)) {
      day.append(el("h3", { class: "sect", text: group.section }));
      for (const item of group.items) {
        day.append(
          el(
            "article",
            { class: "evt" },
            item.kickers.length
              ? el("div", { class: "kicker", text: item.kickers.join("  ·  ") })
              : null,
            el("p", { text: item.text }),
            item.sources.length
              ? el("div", { class: "srcs", text: item.sources.join(" · ") })
              : null,
          ),
        );
      }
    }
    for (const t of chunk.outbound_links) linkSet.add(demojibake(t));
  }
  const links = [...linkSet].slice(0, 16);
  if (links.length) {
    day.append(
      el(
        "div",
        { class: "dig" },
        el("div", { class: "dig-label", text: "Dig deeper" }),
        el(
          "div",
          { class: "chips" },
          ...links.map((title) =>
            el("button", { text: title, onclick: () => openPanel(bridge, title) }),
          ),
        ),
      ),
    );
  }
  return day;
}

function renderFeed(bridge, feed) {
  const root = $("feed");
  clear(root);
  if (!feed.docs.length) {
    root.append(
      el("div", {
        class: "card",
        text: "No days ingested yet. The newsworthy watcher fills this corpus on its daily tick.",
      }),
    );
    return;
  }
  $("freshness").textContent = `updated through ${feed.docs[0].source_doc_id}`;
  for (const doc of feed.docs) root.append(renderDay(bridge, doc));
}

async function main() {
  const loading = $("loading");
  const errBox = $("error");
  if (!hasBridge()) {
    errBox.textContent =
      "No meshApp bridge — run inside the desktop host or `sovereign meshapp dev today`.";
    errBox.hidden = false;
    loading.hidden = true;
    return;
  }
  const bridge = connect(CORPUS);
  try {
    const feed = await bridge.documentFeed(DAYS);
    renderFeed(bridge, feed);
    $("app").hidden = false;
  } catch (e) {
    errBox.textContent = `Could not read the feed: ${emsg(e)}`;
    errBox.hidden = false;
  } finally {
    loading.hidden = true;
  }
}

main();
