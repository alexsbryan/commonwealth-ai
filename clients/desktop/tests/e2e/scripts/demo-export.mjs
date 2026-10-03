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
//
// The H.264 steps prefer libx264; on a build without it (Fedora's
// patent-policy build ships only libopenh264) they fall back to
// libopenh264 quality mode — same container and pixel format, and the
// downgrade is printed and written to MANIFEST.md, never silent.
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
// Hand-recorded beats (B3's mesh-app windows, B7's Pi) go through the
// SAME contract and the SAME ladder:
//
//   · A raw take is encoded ONLY if its gate — a real test in the same
//     run, see beat.ts `rawBeatTest` — passed. A .mov sitting in raw/
//     with no passing gate this run is refused and listed, because "a
//     human dropped a file in" is not evidence and the rest of the reel
//     is held to evidence.
//   · It is normalized to the reel's geometry and frame rate, and its
//     lower-thirds are burned in using the SAME chip the live beats draw
//     (tests/e2e/demo/reel-style.mjs, rasterized through Chromium). The
//     cue times come from raw/<beat-id>.captions.json, which the gate
//     seeds with the beat's scripted lines; only the operator can know
//     the times, so only the times are theirs to fill in.
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";
import { CAPTION, REEL, captionOverlayHtml } from "../demo/reel-style.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
// Overridable so the exporter can be exercised against a scratch
// artifacts tree (a sample take, a regression fixture) without touching
// the operator's real ledger.
const DEMO_DIR = process.env.SOVEREIGN_DEMO_ARTIFACTS ?? path.join(CRATE_ROOT, "test-artifacts/demo");
const LEDGER = path.join(DEMO_DIR, "ledger.jsonl");
const RAW_DIR = path.join(DEMO_DIR, "raw");
const OUT_DIR = path.join(DEMO_DIR, "out");
const MANIFEST = path.join(DEMO_DIR, "MANIFEST.md");
/** Normalized + captioned masters for raw takes. Kept on disk so a
 *  re-export doesn't re-render, and so you can eyeball what the ladder
 *  was actually fed when a clip looks wrong. */
const MASTER_DIR = path.join(RAW_DIR, ".master");
/** The variable font the app itself loads (src/main.ts). Inlined into
 *  the caption plate so the burned-in type is the app's type and not
 *  whatever Chromium falls back to. */
const FONT_FILE = path.join(
  CRATE_ROOT,
  "node_modules/@fontsource-variable/ibm-plex-sans/files/ibm-plex-sans-latin-wght-normal.woff2",
);
const RAW_EXT = /\.(mov|mp4|m4v|webm|mkv)$/i;

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
  /** Skip the frosted backdrop behind burned-in captions (the live chip
   *  gets it from CSS `backdrop-filter`). Escape hatch for an ffmpeg
   *  build where the mask composite misbehaves — the caption still
   *  renders, just flat. */
  noCaptionBlur: has("no-caption-blur"),
  /** Re-render raw masters even when one is already on disk. */
  freshMasters: has("fresh-masters"),
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
// Not every ffmpeg build ships libx264 (Fedora's build has only
// libopenh264, and no preset — its quality knob is `-q:v` in quality
// mode). Same degrade-don't-fail class as the webp/png poster choice:
// the ladder's mp4 stays real H.264 yuv420p either way, and the
// substitution is said out loud.
const H264_ENC = (() => {
  const encoders =
    spawnSync("ffmpeg", ["-hide_banner", "-encoders"], { encoding: "utf8" }).stdout ?? "";
  if (encoders.includes("libx264")) {
    return {
      codec: "libx264",
      qualityArgs: (crf, preset) => ["-crf", String(crf), "-preset", preset],
      note: null,
    };
  }
  if (encoders.includes("libopenh264")) {
    console.warn(
      "  ! this ffmpeg build has no libx264 (Fedora's patent-policy build) — the H.264\n" +
        "    steps use libopenh264 quality mode. Same container and pixel format; the\n" +
        "    manifest records the substitution.",
    );
    return {
      codec: "libopenh264",
      qualityArgs: (q) => ["-rc_mode", "quality", "-q:v", String(q)],
      note:
        "this ffmpeg build has no libx264 (Fedora's patent-policy build) — the H.264 " +
        "steps used libopenh264 quality mode; the mp4s are still real H.264 yuv420p",
    };
  }
  return {
    codec: "libx264", // let ffmpeg raise the real "encoder not found" error
    qualityArgs: (crf, preset) => ["-crf", String(crf), "-preset", preset],
    note:
      "neither libx264 nor libopenh264 is in this ffmpeg build — the mp4 steps failed",
  };
})();

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
    "-c:v", H264_ENC.codec, ...H264_ENC.qualityArgs(26, "slow"),
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

// ── raw takes: the operator's footage, the machine's contract ────────

/** The take the operator recorded for `id`, or null. */
function findRawTake(id) {
  if (!fs.existsSync(RAW_DIR)) return null;
  const hit = fs
    .readdirSync(RAW_DIR)
    .filter((f) => RAW_EXT.test(f))
    .find((f) => f.replace(/\.[^.]+$/, "") === id);
  return hit ? path.join(RAW_DIR, hit) : null;
}

/** Cue sheet for a raw take: trim handles + caption times. The gate
 *  seeds it; the operator fills in the numbers against their recording.
 *  Absent or malformed is not fatal — the clip still exports, and the
 *  manifest says what it went out without. */
function readCaptionSheet(id) {
  const file = path.join(RAW_DIR, `${id}.captions.json`);
  const empty = { file, trimInSec: null, trimOutSec: null, captions: [], notes: [] };
  if (!fs.existsSync(file)) {
    return { ...empty, notes: [`no cue sheet at ${path.basename(file)} — no lower-thirds burned in`] };
  }
  let parsed;
  try {
    parsed = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (e) {
    return { ...empty, notes: [`cue sheet ${path.basename(file)} is not valid JSON (${e.message}) — ignored`] };
  }
  const notes = [];
  const captions = [];
  for (const c of parsed.captions ?? []) {
    if (typeof c?.at !== "number" || !Number.isFinite(c.at)) {
      notes.push(`caption "${String(c?.text ?? "").slice(0, 48)}…" has no \`at\` time — not burned in`);
      continue;
    }
    captions.push({ at: c.at, text: String(c.text ?? ""), holdMs: Number(c.holdMs ?? CAPTION.holdMs) });
  }
  captions.sort((a, b) => a.at - b.at);
  return {
    file,
    trimInSec: typeof parsed.trimInSec === "number" ? parsed.trimInSec : null,
    trimOutSec: typeof parsed.trimOutSec === "number" ? parsed.trimOutSec : null,
    captions,
    notes,
  };
}

/** Rasterize each caption to a full-frame RGBA plate through the SAME
 *  CSS the live overlay sets. One browser for the whole export. */
async function renderCaptionPlates(jobs) {
  if (jobs.length === 0) return;
  let fontDataUri = null;
  if (fs.existsSync(FONT_FILE)) {
    fontDataUri = `data:font/woff2;base64,${fs.readFileSync(FONT_FILE).toString("base64")}`;
  } else {
    console.warn(
      `  ! ${path.relative(CRATE_ROOT, FONT_FILE)} is missing — burned-in captions will fall\n` +
        `    back to a system font and will NOT match the live beats. \`npm install\`.`,
    );
  }
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({
      viewport: { width: REEL.width, height: REEL.height },
      deviceScaleFactor: 1,
    });
    for (const job of jobs) {
      // Two plates per cue: the chip as it looks, and its SHAPE. The
      // shape is what `backdrop-filter` blurs — masking the blur with
      // the chip's own 82%-opaque paint composites the frost at 82% and
      // reads darker than the live caption.
      for (const [file, maskPlate] of [
        [job.png, false],
        [job.maskPng, true],
      ]) {
        await page.setContent(captionOverlayHtml(job.text, { fontDataUri, maskPlate }), {
          waitUntil: "load",
        });
        await page.evaluate(() => document.fonts.ready);
        await page.screenshot({ path: file, omitBackground: true });
      }
    }
  } finally {
    await browser.close();
  }
}

/**
 * Normalize a raw take to the reel's frame and burn its captions in.
 *
 * Geometry: scaled to fit and centered on the reel background rather
 * than stretched — a take shot at the wrong aspect gets framed, not
 * distorted, and the manifest says it was padded so it can be reshot.
 *
 * Captions: each plate is a full-length RGBA stream that is transparent
 * except during its cue (fade drives the alpha directly on the clip's
 * timeline, so no PTS juggling). The frosted backdrop is the blurred
 * frame carrying the plate's own alpha — which reproduces CSS
 * `backdrop-filter` including the chip's rounded corners, instead of
 * approximating it with a rectangle.
 */
async function buildRawMaster(src, id, sheet) {
  const master = path.join(MASTER_DIR, `${id}.mp4`);
  if (fs.existsSync(master) && !OPTS.freshMasters) return { master, reused: true, notes: [] };
  fs.mkdirSync(MASTER_DIR, { recursive: true });

  const notes = [];
  const srcDur = durationSec(src);
  const trimIn = Math.max(0, sheet.trimInSec ?? 0);
  const trimOut =
    sheet.trimOutSec !== null && sheet.trimOutSec > trimIn ? sheet.trimOutSec : srcDur;
  const clipDur = Math.max(0.1, trimOut - trimIn);

  const probe = JSON.parse(
    sh("ffprobe", [
      "-v", "error",
      "-select_streams", "v:0",
      "-show_entries", "stream=width,height",
      "-of", "json",
      src,
    ]).toString(),
  );
  const sw = Number(probe.streams?.[0]?.width ?? 0);
  const sh_ = Number(probe.streams?.[0]?.height ?? 0);
  if (sw && sh_) {
    const srcAspect = sw / sh_;
    const reelAspect = REEL.width / REEL.height;
    if (Math.abs(srcAspect - reelAspect) > 0.01) {
      notes.push(
        `recorded at ${sw}×${sh_} (${srcAspect.toFixed(2)}:1), padded onto the reel's ` +
          `${REEL.width}×${REEL.height} frame — reshoot at ${REEL.width}×${REEL.height} (or any 16:10) ` +
          `to fill it`,
      );
    }
  }

  // Caption plates.
  const cues = sheet.captions.map((c, i) => ({
    ...c,
    png: path.join(MASTER_DIR, `.cap-${id}-${i}.png`),
    maskPng: path.join(MASTER_DIR, `.capmask-${id}-${i}.png`),
  }));

  await renderCaptionPlates(cues);

  // Input order: [0] the take, then per cue the chip plate and (unless
  // captions are flat) its shape plate.
  const perCue = OPTS.noCaptionBlur ? 1 : 2;
  const inputs = ["-ss", String(trimIn), "-t", String(clipDur), "-i", src];
  for (const c of cues) {
    inputs.push("-loop", "1", "-t", String(clipDur), "-i", c.png);
    if (!OPTS.noCaptionBlur) inputs.push("-loop", "1", "-t", String(clipDur), "-i", c.maskPng);
  }

  const fi = CAPTION.fadeInMs / 1000;
  const fo = CAPTION.fadeOutMs / 1000;
  const chain = [];
  chain.push(
    `[0:v]scale=${REEL.width}:${REEL.height}:force_original_aspect_ratio=decrease:flags=lanczos,` +
      `pad=${REEL.width}:${REEL.height}:(ow-iw)/2:(oh-ih)/2:color=${REEL.bg},` +
      `setsar=1,fps=${REEL.fps}[base]`,
  );

  let cursor = "base";
  if (cues.length && !OPTS.noCaptionBlur) {
    chain.push(`[base]split=2[base_k][base_b]`);
    chain.push(`[base_b]gblur=sigma=${CAPTION.blurPx}[blurred]`);
    chain.push(`[blurred]split=${cues.length}${cues.map((_, i) => `[bl${i}]`).join("")}`);
    cursor = "base_k";
  }

  // Each plate is a full-length RGBA stream that fade drives to
  // transparent outside its cue — the alpha animates on the CLIP's
  // timeline, so no PTS shifting and no chance of the frosted backdrop
  // sampling the wrong moment.
  cues.forEach((c, i) => {
    const outSt = c.at + Math.max(0.1, c.holdMs / 1000);
    const fades =
      `fade=t=in:st=${c.at.toFixed(3)}:d=${fi}:alpha=1,` +
      `fade=t=out:st=${outSt.toFixed(3)}:d=${fo}:alpha=1`;
    const chipIn = i * perCue + 1;
    chain.push(`[${chipIn}:v]format=rgba,${fades}[cap${i}]`);
    if (OPTS.noCaptionBlur) {
      chain.push(`[${cursor}][cap${i}]overlay=0:0:format=auto[v${i}]`);
    } else {
      chain.push(`[${chipIn + 1}:v]format=rgba,${fades},alphaextract[capm${i}]`);
      chain.push(`[bl${i}]format=rgba[blr${i}]`);
      chain.push(`[blr${i}][capm${i}]alphamerge[frost${i}]`);
      chain.push(`[${cursor}][frost${i}]overlay=0:0:format=auto[vf${i}]`);
      chain.push(`[vf${i}][cap${i}]overlay=0:0:format=auto[v${i}]`);
    }
    cursor = `v${i}`;
  });

  // Visually lossless intermediate: the shipping ladder re-encodes from
  // this, and one generation at CRF 16 costs nothing you can see.
  sh("ffmpeg", [
    "-y",
    ...inputs,
    "-filter_complex", chain.join(";"),
    "-map", `[${cursor}]`,
    "-c:v", H264_ENC.codec, ...H264_ENC.qualityArgs(16, "medium"),
    "-pix_fmt", "yuv420p",
    "-r", String(REEL.fps),
    "-an",
    master,
  ]);
  if (H264_ENC.note) notes.push(H264_ENC.note);

  for (const c of cues) {
    fs.rmSync(c.png, { force: true });
    fs.rmSync(c.maskPng, { force: true });
  }
  if (cues.length) {
    notes.push(
      `${cues.length} lower-third(s) burned in at ` +
        cues.map((c) => `${c.at.toFixed(1)}s`).join(", ") +
        (OPTS.noCaptionBlur ? " (flat — --no-caption-blur)" : ""),
    );
  }
  if (trimIn > 0 || trimOut < srcDur - 0.01) {
    notes.push(`trimmed to ${trimIn.toFixed(1)}s–${trimOut.toFixed(1)}s of ${srcDur.toFixed(1)}s`);
  }
  return { master, reused: false, notes, cues };
}

// ── main ─────────────────────────────────────────────────────────────
fs.mkdirSync(OUT_DIR, { recursive: true });
fs.mkdirSync(RAW_DIR, { recursive: true });

const beats = readLedger();
const results = [];
const problems = [];

for (const b of beats) {
  if (OPTS.beat && b.id !== OPTS.beat) continue;
  if (b.capture === "raw") continue; // handled below, against its take

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

// ── hand-recorded beats (B3's mesh-app windows, B7's Pi) ─────────────
//
// The gate authorizes the take; nothing else does. In particular a file
// that appears in raw/ with no passing gate in THIS run is refused —
// silently encoding it would reintroduce exactly the "ships without
// proof" hole the rest of this script exists to close.
const rawBeats = beats.filter((b) => b.capture === "raw" && (!OPTS.beat || b.id === OPTS.beat));
const gatedIds = new Set(rawBeats.filter((b) => b.status === "passed").map((b) => b.id));

for (const b of rawBeats) {
  if (b.status !== "passed") {
    problems.push({
      id: b.id,
      title: b.title,
      why:
        (b.status === "skipped"
          ? `GATE SKIPPED — ${b.skipReason ?? "no reason recorded"}`
          : `GATE FAILED — ${(b.error ?? "").split("\n")[0] || "assertion failed"}`) +
        ` (the take, if any, is not authorized)`,
    });
    continue;
  }
  const src = findRawTake(b.id);
  if (!src) {
    problems.push({
      id: b.id,
      title: b.title,
      why:
        `gate PASSED but there is no take at raw/${b.id}.{mov,mp4,m4v,webm,mkv}. ` +
        `Record it:\n` +
        (b.recordingGuide ?? []).map((s) => `    - ${s}`).join("\n"),
    });
    continue;
  }

  const sheet = readCaptionSheet(b.id);
  process.stdout.write(`▸ ${b.id} (hand-recorded) … `);
  const built = await buildRawMaster(src, b.id, sheet);
  const dur = durationSec(built.master);
  const produced = encodeLadder(built.master, b.id, 0, dur);

  // Short-form loop: raw takes have no marks, so cut on the first cue
  // (the operator's own idea of where the beat lands) or the head.
  let gif = null;
  const gifNotes = [];
  if (!OPTS.noGif) {
    const pad = Number(b.gifPadSec ?? 1.2);
    const anchor = sheet.captions[0]?.at ?? 0;
    const gStart = Math.max(0, anchor - pad);
    gif = encodeGif(built.master, b.id, gStart, Math.min(OPTS.gifMaxSec, Math.max(2, dur - gStart)));
    if (gif?.downgrades?.length) gifNotes.push(...gif.downgrades);
    if (gif) {
      gifNotes.unshift(
        sheet.captions.length
          ? `cut on the first cue at ${anchor.toFixed(1)}s (+${pad}s lead-in)`
          : `no cues on the sheet — cut from the head`,
      );
    }
  }

  process.stdout.write(
    `ok${built.reused ? " (reused master)" : ""}${gif ? ` · gif ${gif.mbytes.toFixed(1)} MB` : ""}\n`,
  );
  results.push({
    ...b,
    produced,
    gif,
    gifNotes,
    clipSec: dur,
    notes: [
      `hand-recorded from ${path.relative(DEMO_DIR, src)}; the CLAIM was gated by the ` +
        `\`${b.id}\` test in this run, the PIXELS are human-attested`,
      ...(b.notes ?? []),
      ...built.notes,
      ...sheet.notes,
    ],
  });
}

// Footage with no gate at all. Named, not ignored.
for (const f of fs.existsSync(RAW_DIR) ? fs.readdirSync(RAW_DIR) : []) {
  if (!RAW_EXT.test(f)) continue;
  const stem = f.replace(/\.[^.]+$/, "");
  if (gatedIds.has(stem)) continue;
  if (OPTS.beat && stem !== OPTS.beat) continue;
  problems.push({
    id: stem,
    title: "(unrecognized raw take)",
    why:
      `raw/${f} has no raw beat with a passing gate in this run, so it was NOT encoded. ` +
      `Hand-recorded footage ships only behind a gate — add a \`rawBeatTest\` whose id is ` +
      `\`${stem}\`, or rename the file to match an existing one.`,
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
if (H264_ENC.note) {
  lines.push("");
  lines.push(`> ${H264_ENC.note}.`);
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
