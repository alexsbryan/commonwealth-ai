#!/usr/bin/env node
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// demo-export — turn a demo capture run into the shipping encode ladder.
//
//   npm run demo:export
//   npm run demo:export -- --beat b1-determinism
//   npm run demo:export -- --gif-max-mb 4 --width 1280
//
// Reads test-artifacts/demo/ledger.jsonl (written by tests/e2e/demo/beat.ts),
// takes the LATEST run only, and for every beat that PASSED emits:
//
//   out/<beat>.mp4          H.264 CRF 26, yuv420p, +faststart, no audio
//   out/<beat>.webm         VP9 CRF 40, no audio
//   out/<beat>-poster.webp  first frame (png when ffmpeg lacks libwebp)
//   out/<beat>.gif          short-form loop cut on the beat's gifMark
//
// Two rules it will not bend:
//
//   1. A beat that FAILED or SKIPPED produces no clip, and there is no
//      flag to override that. The ledger's `status` is the gate. A demo
//      we can't verify is a demo we don't ship — and a stale clip from a
//      previous run is exactly how a reel starts lying.
//   2. Whatever it could not produce is printed AND written to
//      MANIFEST.md with the reason. "The peer was down that day" should
//      be something you read off the run, not something you discover in
//      the edit.
//
// Hand-recorded beats (B7, the Pi) go through the SAME ladder: drop a
// .mov/.mp4 at test-artifacts/demo/raw/<beat-id>.<ext> and it is encoded
// with identical settings so it cuts into the reel without looking
// pasted in.
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const DEMO_DIR = path.join(CRATE_ROOT, "test-artifacts/demo");
const LEDGER = path.join(DEMO_DIR, "ledger.jsonl");
const RAW_DIR = path.join(DEMO_DIR, "raw");
const OUT_DIR = path.join(DEMO_DIR, "out");
const MANIFEST = path.join(DEMO_DIR, "MANIFEST.md");

// ── args ─────────────────────────────────────────────────────────────
const argv = process.argv.slice(2);
const flag = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : dflt;
};
const has = (name) => argv.includes(`--${name}`);

const OPTS = {
  beat: flag("beat", null),
  width: Number(flag("width", 1280)),
  gifWidth: Number(flag("gif-width", 800)),
  gifFps: Number(flag("gif-fps", 15)),
  gifMaxSec: Number(flag("gif-max-sec", 9)),
  gifMaxMb: Number(flag("gif-max-mb", 5)),
  leadInSec: Number(flag("lead-in", 0.6)),
  noGif: has("no-gif"),
};

// ── tool discovery ───────────────────────────────────────────────────
function have(bin) {
  return spawnSync("which", [bin], { encoding: "utf8" }).status === 0;
}
const HAVE_FFMPEG = have("ffmpeg");
const HAVE_FFPROBE = have("ffprobe");
// gifski quantizes far better than ffmpeg's default palette. Optional:
// we fall back to ffmpeg palettegen/paletteuse rather than refusing, and
// say so, so a missing brew package costs quality and not the run.
const HAVE_GIFSKI = have("gifski");
// Not every ffmpeg build ships libwebp — Homebrew's plain `ffmpeg` formula
// does not, while `ffmpeg-full` does, and installing an unrelated package
// can quietly swap one for the other underneath you. A poster is a poster;
// degrade the container rather than failing an otherwise-good export.
const POSTER_EXT = spawnSync("ffmpeg", ["-hide_banner", "-encoders"], { encoding: "utf8" })
  .stdout?.includes("webp")
  ? "webp"
  : "png";

if (!HAVE_FFMPEG || !HAVE_FFPROBE) {
  console.error(
    "demo-export needs ffmpeg + ffprobe on PATH.\n" +
      "  brew install ffmpeg\n" +
      "  brew install gifski   # optional, much better GIF quantization",
  );
  process.exit(1);
}

const sh = (bin, args) => execFileSync(bin, args, { stdio: ["ignore", "pipe", "pipe"] });

function durationSec(file) {
  const out = sh("ffprobe", [
    "-v", "error",
    "-show_entries", "format=duration",
    "-of", "default=noprint_wrappers=1:nokey=1",
    file,
  ])
    .toString()
    .trim();
  const d = Number(out);
  return Number.isFinite(d) ? d : 0;
}

const mb = (file) => fs.statSync(file).size / (1024 * 1024);

// ── ledger ───────────────────────────────────────────────────────────
function readLedger() {
  if (!fs.existsSync(LEDGER)) {
    console.error(
      `no ledger at ${LEDGER}\nRun a capture first:  npm run demo`,
    );
    process.exit(1);
  }
  const rows = fs
    .readFileSync(LEDGER, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l));
  const beats = rows.filter((r) => r.kind === "beat");
  if (beats.length === 0) return [];
  // Latest run only — never Frankenstein clips across takes.
  const latest = Math.max(...beats.map((b) => b.runId ?? 0));
  const selected = beats.filter((b) => (b.runId ?? 0) === latest);
  // Say what was selected and what was left behind. Silence here once hid a
  // ledger split (a failing beat restarted the worker and minted a second
  // runId mid-run), so an export of 1 beat looked identical to a run of 1.
  const older = beats.length - selected.length;
  console.log(
    `ledger: run ${latest} — ${selected.length} beat(s)` +
      (older > 0 ? `; ignoring ${older} beat(s) from earlier run(s)` : ""),
  );
  return selected;
}

/** Where the beat's video actually landed.
 *
 * The ledger records `page.video().path()` at the moment the beat ends,
 * and at that moment the file is still Playwright's IN-FLIGHT artifact
 * (`video/.playwright-artifacts-N/page@<hash>.webm`). Playwright only
 * relocates it to `<outputDir>/video.webm` when the context closes —
 * AFTER the ledger line is written — and deletes the temp dir. So the
 * recorded path is essentially never the resting path, and trusting it
 * alone made every passing beat look like "video capture was off".
 *
 * Prefer the recorded path when it does exist (a future beat.ts that
 * writes post-close would land there), then fall back to the recorded
 * outputDir, which Playwright owns per test and per retry.
 */
function resolveVideo(b) {
  if (b.video && fs.existsSync(b.video)) return b.video;
  if (!b.outputDir || !fs.existsSync(b.outputDir)) return null;
  const webms = fs
    .readdirSync(b.outputDir)
    .filter((f) => f.endsWith(".webm"))
    .map((f) => path.join(b.outputDir, f));
  if (webms.length === 0) return null;
  // >1 only if a retry left an older take behind; newest is the one the
  // ledger line describes.
  webms.sort((x, y) => fs.statSync(y).mtimeMs - fs.statSync(x).mtimeMs);
  return webms[0];
}

// ── encode ladder ────────────────────────────────────────────────────
function encodeLadder(src, stem, startSec, endSec) {
  const produced = [];
  const trim = ["-ss", String(Math.max(0, startSec))];
  if (Number.isFinite(endSec) && endSec > startSec) trim.push("-to", String(endSec));
  const scale = `scale=${OPTS.width}:-2:flags=lanczos`;

  const mp4 = path.join(OUT_DIR, `${stem}.mp4`);
  sh("ffmpeg", [
    "-y", ...trim, "-i", src,
    "-vf", scale,
    "-c:v", "libx264", "-crf", "26", "-preset", "slow",
    "-pix_fmt", "yuv420p",
    "-movflags", "+faststart",
    "-an", // strip audio outright: a muted track still ships its bytes
    mp4,
  ]);
  produced.push(mp4);

  const webm = path.join(OUT_DIR, `${stem}.webm`);
  sh("ffmpeg", [
    "-y", ...trim, "-i", src,
    "-vf", scale,
    "-c:v", "libvpx-vp9", "-b:v", "0", "-crf", "40",
    "-row-mt", "1",
    "-an",
    webm,
  ]);
  produced.push(webm);

  const poster = path.join(OUT_DIR, `${stem}-poster.${POSTER_EXT}`);
  sh("ffmpeg", [
    "-y", ...trim, "-i", src,
    "-vf", scale, "-frames:v", "1",
    poster,
  ]);
  produced.push(poster);

  return produced;
}

/** Cut the short-form loop. Returns {file, seconds, mbytes, downgrades[]}. */
function encodeGif(src, stem, startSec, lenSec) {
  const frames = path.join(OUT_DIR, `.frames-${stem}`);
  const gif = path.join(OUT_DIR, `${stem}.gif`);
  const downgrades = [];

  // Length is always the cheapest thing to cut, so the ladder walks
  // fps/width DOWN only after the clip is already at its budget length.
  const attempts = [
    { fps: OPTS.gifFps, width: OPTS.gifWidth },
    { fps: 12, width: OPTS.gifWidth },
    { fps: 12, width: 640 },
    { fps: 10, width: 560 },
  ];

  for (const [i, a] of attempts.entries()) {
    fs.rmSync(frames, { recursive: true, force: true });
    fs.mkdirSync(frames, { recursive: true });
    sh("ffmpeg", [
      "-y",
      "-ss", String(Math.max(0, startSec)),
      "-t", String(lenSec),
      "-i", src,
      "-vf", `fps=${a.fps},scale=${a.width}:-1:flags=lanczos`,
      path.join(frames, "%04d.png"),
    ]);
    const pngs = fs.readdirSync(frames).filter((f) => f.endsWith(".png")).sort();
    if (pngs.length === 0) {
      fs.rmSync(frames, { recursive: true, force: true });
      return null;
    }

    if (HAVE_GIFSKI) {
      sh("gifski", [
        "--fps", String(a.fps),
        "-Q", "80",
        "-o", gif,
        ...pngs.map((f) => path.join(frames, f)),
      ]);
    } else {
      // ffmpeg palettegen/paletteuse — visibly worse banding than
      // gifski, which is why the header tells you to install it.
      const palette = path.join(frames, "palette.png");
      sh("ffmpeg", ["-y", "-i", path.join(frames, "%04d.png"), "-vf", "palettegen=stats_mode=diff", palette]);
      sh("ffmpeg", [
        "-y",
        "-framerate", String(a.fps),
        "-i", path.join(frames, "%04d.png"),
        "-i", palette,
        "-lavfi", "paletteuse=dither=bayer:bayer_scale=5",
        "-loop", "0",
        gif,
      ]);
    }
    fs.rmSync(frames, { recursive: true, force: true });

    const size = mb(gif);
    if (size <= OPTS.gifMaxMb || i === attempts.length - 1) {
      if (i > 0) {
        downgrades.push(
          `stepped down to ${a.fps}fps/${a.width}px to fit the ${OPTS.gifMaxMb} MB budget`,
        );
      }
      if (size > OPTS.gifMaxMb) {
        downgrades.push(
          `STILL ${size.toFixed(1)} MB after every step-down — shorten the mark window`,
        );
      }
      return { file: gif, seconds: lenSec, mbytes: size, downgrades };
    }
  }
  return null;
}

// ── main ─────────────────────────────────────────────────────────────
fs.mkdirSync(OUT_DIR, { recursive: true });
fs.mkdirSync(RAW_DIR, { recursive: true });

const beats = readLedger();
const results = [];
const problems = [];

for (const b of beats) {
  if (OPTS.beat && b.id !== OPTS.beat) continue;

  if (b.status !== "passed") {
    problems.push({
      id: b.id,
      title: b.title,
      why:
        b.status === "skipped"
          ? `SKIPPED — ${b.skipReason ?? "no reason recorded"}`
          : `FAILED — ${(b.error ?? "").split("\n")[0] || "assertion failed"}`,
    });
    continue;
  }
  const video = resolveVideo(b);
  if (!video) {
    problems.push({
      id: b.id,
      title: b.title,
      why:
        `passed but no video on disk (recorded ${b.video ?? "none"}; ` +
        `also looked in ${b.outputDir ?? "no outputDir recorded"}) — was video capture off?`,
    });
    continue;
  }

  const dur = durationSec(video);
  const marks = Array.isArray(b.marks) ? b.marks : [];
  const firstMark = marks.length ? Math.min(...marks.map((m) => m.atMs)) / 1000 : 0;
  const start = Math.max(0, firstMark - OPTS.leadInSec);

  process.stdout.write(`▸ ${b.id} — ${(dur - start).toFixed(1)}s … `);
  const produced = encodeLadder(video, b.id, start, dur);

  // Short-form loop, cut on the mark the BEAT named (not a guess made
  // here), padded by the beat's own gifPadSec.
  let gif = null;
  const gifNotes = [];
  if (!OPTS.noGif) {
    const pad = Number(b.gifPadSec ?? 1.2);
    const named = b.gifMark ? marks.find((m) => m.name === b.gifMark) : null;
    const anchor = named ?? marks[marks.length - 1] ?? null;
    if (!anchor) {
      gifNotes.push("no marks recorded — no short-form loop cut");
    } else {
      if (b.gifMark && !named) {
        gifNotes.push(
          `gifMark "${b.gifMark}" was never reached this run (the beat took its ` +
            `best-effort branch); cut on "${anchor.name}" instead`,
        );
      }
      const gStart = Math.max(0, anchor.atMs / 1000 - pad);
      const gLen = Math.min(OPTS.gifMaxSec, Math.max(2, dur - gStart));
      gif = encodeGif(video, b.id, gStart, gLen);
      if (gif?.downgrades?.length) gifNotes.push(...gif.downgrades);
      if (gif) gifNotes.unshift(`cut on "${anchor.name}" (+${pad}s lead-in)`);
    }
  }

  process.stdout.write(`ok${gif ? ` · gif ${gif.mbytes.toFixed(1)} MB` : ""}\n`);
  results.push({ ...b, produced, gif, gifNotes, clipSec: dur - start });
}

// ── hand-recorded beats (B7 and anything else out-of-band) ───────────
const rawFiles = fs
  .readdirSync(RAW_DIR)
  .filter((f) => /\.(mov|mp4|m4v|webm)$/i.test(f))
  .filter((f) => !OPTS.beat || f.startsWith(OPTS.beat));

for (const f of rawFiles) {
  const src = path.join(RAW_DIR, f);
  const stem = f.replace(/\.[^.]+$/, "");
  const dur = durationSec(src);
  process.stdout.write(`▸ ${stem} (hand-recorded, ${dur.toFixed(1)}s) … `);
  const produced = encodeLadder(src, stem, 0, dur);
  const gif = OPTS.noGif
    ? null
    : encodeGif(src, stem, 0, Math.min(OPTS.gifMaxSec, dur));
  process.stdout.write(`ok${gif ? ` · gif ${gif.mbytes.toFixed(1)} MB` : ""}\n`);
  results.push({
    id: stem,
    title: "(hand-recorded)",
    claim: "",
    status: "passed",
    notes: ["captured out-of-band; correctness is human-attested, not asserted"],
    produced,
    gif,
    gifNotes: [],
    clipSec: dur,
  });
}

// ── manifest ─────────────────────────────────────────────────────────
const rel = (p) => path.relative(DEMO_DIR, p);
const lines = [];
lines.push("<!-- generated by tests/e2e/scripts/demo-export.mjs — do not edit -->");
lines.push("# Demo export manifest");
lines.push("");
lines.push(
  `Exported ${results.length} clip(s)` +
    (problems.length ? `, ${problems.length} beat(s) NOT exported.` : "."),
);
if (!HAVE_GIFSKI) {
  lines.push("");
  lines.push(
    "> gifski was not on PATH — GIFs fell back to ffmpeg palettegen, which bands " +
      "visibly on gradients. `brew install gifski` and re-run for the shipping quality.",
  );
}
lines.push("");

if (results.length) {
  lines.push("## Produced");
  lines.push("");
  lines.push("| Beat | Clip | GIF | Artifacts |");
  lines.push("|---|---|---|---|");
  for (const r of results) {
    lines.push(
      `| **${r.id}** — ${r.title} | ${r.clipSec.toFixed(1)}s | ` +
        `${r.gif ? `${r.gif.seconds.toFixed(1)}s · ${r.gif.mbytes.toFixed(1)} MB` : "—"} | ` +
        `${[...r.produced, r.gif?.file].filter(Boolean).map((p) => `\`${rel(p)}\``).join("<br>")} |`,
    );
  }
  lines.push("");
  for (const r of results) {
    const notes = [...(r.notes ?? []), ...(r.gifNotes ?? [])];
    if (!notes.length) continue;
    lines.push(`### ${r.id}`);
    if (r.claim) lines.push(`> ${r.claim}`);
    lines.push("");
    for (const n of notes) lines.push(`- ${n}`);
    lines.push("");
  }
}

if (problems.length) {
  lines.push("## NOT exported");
  lines.push("");
  lines.push(
    "These beats are absent from the reel. That is the intended behaviour — a beat " +
      "that could not be verified is never filled in from an older take.",
  );
  lines.push("");
  for (const p of problems) lines.push(`- **${p.id}** — ${p.title}\n  - ${p.why}`);
  lines.push("");
}

lines.push("## Embed");
lines.push("");
lines.push("```html");
lines.push(
  `<video autoplay loop muted playsinline preload="metadata" poster="<beat>-poster.${POSTER_EXT}">`,
);
lines.push('  <source src="<beat>.webm" type="video/webm">');
lines.push('  <source src="<beat>.mp4" type="video/mp4">');
lines.push("</video>");
lines.push("```");
lines.push("");

fs.writeFileSync(MANIFEST, lines.join("\n"));

console.log(`\nmanifest → ${MANIFEST}`);
if (problems.length) {
  console.log(`\n${problems.length} beat(s) NOT exported:`);
  for (const p of problems) console.log(`  ✗ ${p.id}: ${p.why}`);
}
// A capture run where nothing exported is a failed export, not a quiet success.
if (results.length === 0) {
  console.error("\nnothing exported.");
  process.exit(2);
}
