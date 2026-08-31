// SPDX-License-Identifier: AGPL-3.0-or-later
// Tauri updater manifest for the Sovereign desktop app.
//
// The desktop's `tauri-plugin-updater` polls this endpoint with the user's
// updater OS + currently-installed version. We query GitHub Releases for the
// latest `desktop-v*` tag, compare versions, and either:
//   - 204 No Content  -> the user is up to date
//   - 200 JSON        -> manifest listing EVERY per-arch artifact for that OS
//                         + each one's signature
//
// ─── The target contract (why this endpoint returns ALL arches) ─────────
// tauri-plugin-updater interpolates `{{target}}` in the endpoint URL with
// its `updater_os()` value, which is OS-ONLY — `darwin`, `linux`, or
// `windows` (see updater.rs::updater_os; there is NO arch in it). Our
// configured URL is
//   https://svrnme.sh/api/desktop/updater/{{target}}/{{current_version}}
// so a real app requests `.../updater/darwin/0.1.20` — it never sends the
// architecture. The plugin then picks the right artifact CLIENT-SIDE: its
// `get_urls()` searches the returned manifest's `platforms` map for the
// combined key `{os}-{arch}` (e.g. `darwin-x86_64` / `darwin-aarch64`).
//
// Therefore the manifest MUST contain a `platforms` entry per arch, keyed by
// the combined `{os}-{arch}` string, and the endpoint must accept the OS-only
// path segment. (A prior version keyed only by the combined string AND only
// accepted a combined path segment — so every real poll came in as `darwin`,
// matched nothing, and 400'd. The desktop app soft-fails a failed check to
// "you're up to date", so the break was silent. Regression fixed 2026-07-15.)
//
// We still accept a combined `{os}-{arch}` segment too (defensive: manual
// probes, and any future build whose URL template adds `{{arch}}`), in which
// case the manifest carries just that one arch.
//
// Which repo the assets come from, the retired-shelf fallback, and the env
// that configures both live in `_releases.js`, shared with the download
// endpoint — one decider, not two.

import { isNewer, latestRelease } from './_releases.js';

// Combined Tauri targets -> regex matching the bundle artifact name.
// Names track productName in tauri.conf.json ("svrnmesh" since the 2026-06-29
// rename). Tauri emits the macOS updater archive as a bare `svrnmesh.app.tar.gz`;
// the release pipeline arch-qualifies it to `svrnmesh_<ver>_<aarch64|x64>.app.tar.gz`
// before upload so the two mac targets can coexist in one release and match here.
const TARGET_TO_ASSET_PATTERN = {
  'darwin-aarch64': /svrnmesh[._].*aarch64.*\.app\.tar\.gz$/i,
  'darwin-x86_64':  /svrnmesh[._].*(x64|x86_64).*\.app\.tar\.gz$/i,
  'linux-x86_64':   /svrnmesh[._].*amd64.*\.AppImage$/i,
  'windows-x86_64': /svrnmesh[._].*x64.*-setup\.exe$/i,
};

// OS-only target (what the plugin actually sends) -> the combined targets to
// include in that OS's manifest. The plugin's get_urls() selects the right one.
const OS_TO_TARGETS = {
  'darwin':  ['darwin-aarch64', 'darwin-x86_64'],
  'linux':   ['linux-x86_64'],
  'windows': ['windows-x86_64'],
};

export const config = { runtime: 'edge' };

export default async function handler(req) {
  const url = new URL(req.url);
  const rawTarget = url.searchParams.get('target') ?? '';
  const currentVersion = url.searchParams.get('current_version') ?? '';

  if (!rawTarget || !currentVersion) {
    return text('missing target or current_version', 400);
  }

  // Resolve the requested path segment to the set of combined targets whose
  // artifacts belong in this manifest. Accept BOTH the OS-only form the plugin
  // sends (`darwin`) and a combined form (`darwin-x86_64`) for defensiveness.
  let wantedTargets;
  if (OS_TO_TARGETS[rawTarget]) {
    wantedTargets = OS_TO_TARGETS[rawTarget];
  } else if (TARGET_TO_ASSET_PATTERN[rawTarget]) {
    wantedTargets = [rawTarget];
  } else {
    return text(`unsupported target: ${rawTarget}`, 400);
  }

  // Highest-semver non-draft `desktop-v*` across the release repos. Drafts
  // are skipped so a partial cut doesn't roll out; prereleases are kept —
  // the desktop stream has used them for staged cuts and the version compare
  // below is the real gate. Why max-semver and not list order: `_releases.js`.
  let latest;
  try {
    latest = await latestRelease({
      tagPrefix: 'desktop-v',
      includePrerelease: true,
      perPage: 30,
      userAgent: 'sovereign-updater/1',
    });
  } catch (e) {
    console.error('[updater] github fetch failed', e);
    return text('github api unreachable', 502);
  }
  if (!latest) {
    return text('no desktop release found', 404);
  }
  const { release, version: latestVersion } = latest;
  if (!isNewer(latestVersion, currentVersion)) {
    // The plugin treats 204 as "you're up to date". This is the
    // happy-path response for every poll after the first install.
    return new Response(null, {
      status: 204,
      headers: { 'cache-control': 'public, max-age=60, s-maxage=60' },
    });
  }

  // Build a `platforms` entry for every arch of the requested OS that has a
  // signed artifact in this release. Each `.sig` is a tiny base64 blob from
  // `tauri-bundler`; we inline it so the plugin can verify against the
  // embedded pubkey before applying the download.
  const platforms = {};
  const missing = [];
  for (const t of wantedTargets) {
    const pattern = TARGET_TO_ASSET_PATTERN[t];
    const asset = release.assets.find(a => pattern.test(a.name));
    const sigAsset = asset && release.assets.find(a => a.name === `${asset.name}.sig`);
    if (!asset || !sigAsset) {
      missing.push({ target: t, assetFound: !!asset, sigFound: !!sigAsset });
      continue;
    }
    let signature;
    try {
      const sigRes = await fetch(sigAsset.browser_download_url);
      if (!sigRes.ok) throw new Error(`sig fetch ${sigRes.status}`);
      signature = (await sigRes.text()).trim();
    } catch (e) {
      console.error('[updater] sig fetch failed', { target: t, err: String(e) });
      missing.push({ target: t, sigFetch: 'failed' });
      continue;
    }
    platforms[t] = { signature, url: asset.browser_download_url };
  }

  if (Object.keys(platforms).length === 0) {
    console.warn('[updater] no signed artifacts for requested OS', {
      rawTarget, latestVersion, missing,
    });
    return text(`no signed artifact for ${rawTarget} at ${latestVersion}`, 404);
  }
  if (missing.length > 0) {
    // Partial coverage (e.g. one mac arch built, the other not yet). Still a
    // valid manifest for the arches we DO have; log the gap for observability.
    console.warn('[updater] partial arch coverage', { rawTarget, latestVersion, missing });
  }

  const manifest = {
    version: latestVersion,
    notes: stripMarkdown(release.body ?? ''),
    pub_date: release.published_at,
    platforms,
  };

  return new Response(JSON.stringify(manifest), {
    status: 200,
    headers: {
      'content-type': 'application/json; charset=utf-8',
      // Short cache: keeps GitHub API load down without forcing users to
      // wait minutes after a release to see it. The plugin polls on
      // app start + manual click, so 60s is fine.
      'cache-control': 'public, max-age=60, s-maxage=60',
    },
  });
}

function text(message, status) {
  return new Response(message, {
    status,
    headers: { 'content-type': 'text/plain; charset=utf-8' },
  });
}

// Strip common markdown so the in-app update dialog renders clean prose
// instead of `**bolded asterisks**`. Plain-text only; not a full parser.
function stripMarkdown(s) {
  return s
    .replace(/```[\s\S]*?```/g, '')     // code fences
    .replace(/`([^`]+)`/g, '$1')        // inline code
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1') // links -> link text
    .replace(/^#+\s+/gm, '')            // ATX headings
    .replace(/[*_]{1,3}([^*_]+)[*_]{1,3}/g, '$1') // emphasis
    .replace(/^>\s?/gm, '')             // blockquotes
    .trim()
    .slice(0, 2000);
}

