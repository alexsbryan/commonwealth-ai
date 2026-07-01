// Reconstruct a deterministic replay bank from a prior chaos journal.
// The journal truncates the `args` field mid-conversationId, so many records
// fail JSON.parse and drop out of chaos.mjs's REPLAY_BANK builder. The
// `message` (question) is intact before the truncation point, so we recover it
// by regex and emit a clean synthetic journal the replay builder can read.
import fs from "node:fs";

const src = process.argv[2];
const dst = process.argv[3];
if (!src || !dst) {
  console.error("usage: node build-replay-bank.mjs <src-journal> <dst-bank>");
  process.exit(2);
}

const rows = fs
  .readFileSync(src, "utf8")
  .split("\n")
  .filter(Boolean)
  .map((l) => {
    try {
      return JSON.parse(l);
    } catch {
      return null;
    }
  })
  .filter(Boolean);

const chats = rows.filter((r) => r.cmd === "send_message_stream" && r.aligned && r.scopedCorpus);

// The canonical FORCE_LONG suffix (chaos.mjs). The journal truncates args at
// 200 chars, so on longer questions this suffix is cut mid-word. Because it is
// a known constant we can restore it faithfully: strip any partial-suffix tail
// and re-append the full form, recovering the exact question the app saw.
const LONG_SUFFIX = " Answer in exhaustive, comprehensive detail — at least 1500 words.";

const normalizeSuffix = (q) => {
  // If the question opens the FORCE_LONG suffix (possibly truncated), cut at
  // "Answer in exhaustive" and restore the canonical full suffix.
  const idx = q.indexOf("Answer in exhaustive");
  if (idx >= 0) return q.slice(0, idx).trimEnd() + LONG_SUFFIX;
  return q;
};

const extractMsg = (args) => {
  const s = String(args);
  // Prefer a clean parse; fall back to a relaxed regex that tolerates a
  // truncated conversationId (anchor without the trailing quote) OR a message
  // truncated at the 200-char cap (no anchor at all → take to end-of-string).
  let raw = null;
  try {
    const o = JSON.parse(s);
    if (o && o.message) raw = o.message;
  } catch {
    /* truncated — fall through */
  }
  if (raw == null) {
    const anchored = s.match(/"message":"(.*?)","conversationId/s);
    const toEnd = s.match(/"message":"(.*)$/s);
    const m = anchored ?? toEnd;
    if (!m) return null;
    raw = m[1].replace(/\\"/g, '"').replace(/\\\\/g, "\\");
  }
  return normalizeSuffix(raw);
};

const isLong = (q) => /1500 words|exhaustive, comprehensive/i.test(q);

const bank = [];
const seen = new Set();
for (const c of chats) {
  const msg = extractMsg(c.args);
  if (!msg || seen.has(msg)) continue;
  seen.add(msg);
  bank.push({ question: msg, corpus: c.scopedCorpus });
}

const out = bank
  .map((b) => JSON.stringify({ cmd: "send_message_stream", scopedCorpus: b.corpus, args: JSON.stringify({ message: b.question }) }))
  .join("\n");
fs.writeFileSync(dst, out + "\n");

const longN = bank.filter((b) => isLong(b.question)).length;
console.log(`reconstructed bank: ${bank.length} entries, ${longN} long-prompt (1500-word)`);
console.log(`distinct corpora: ${[...new Set(bank.map((b) => b.corpus))].length}`);
console.log(`wrote ${dst}`);
