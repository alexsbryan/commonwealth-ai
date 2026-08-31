// SPDX-License-Identifier: AGPL-3.0-or-later
// Version-free desktop download redirects.
//
//   GET /download/mac-arm        -> 302 to the newest desktop-v* .dmg (Apple Silicon)
//   GET /download/mac-intel      -> 302 ... x64 .dmg
//   GET /download/linux-appimage -> 302 ... .AppImage
//   GET /download/linux-deb      -> 302 ... .deb
//   GET /download/linux-rpm      -> 302 ... .rpm
//   GET /download/windows        -> 302 ... -setup.exe
//
// Exists so nothing user-facing ever hardcodes a version or filename: the
// landing page, docs, and READMEs link /download/<platform> and each release
// publish updates what that means. GitHub's own /releases/latest/download/
// can't do this — `latest` is a single repo-global pointer shared with the
// cli-v* stream, and asset names embed the version anyway. Resolution is the
// shared max-semver pick, minus prereleases (the updater keeps those).
//
// Which repo the assets come from, the retired-shelf fallback, and the env
// that configures both live in `_releases.js`, shared with the updater
// endpoint — one decider, not two.

import { latestRelease } from './_releases.js';

const PLATFORM_TO_ASSET_PATTERN = {
  // Installer artifacts (what a human downloads), NOT the updater archives.
  'mac-arm':        /svrnmesh[._].*aarch64.*\.dmg$/i,
  'mac-intel':      /svrnmesh[._].*(x64|x86_64).*\.dmg$/i,
  'linux-appimage': /svrnmesh[._].*amd64.*\.AppImage$/i,
  'linux-deb':      /svrnmesh[._].*amd64.*\.deb$/i,
  'linux-rpm':      /svrnmesh[.-].*x86_64.*\.rpm$/i,
  'windows':        /svrnmesh[._].*x64.*-setup\.exe$/i,
};

export const config = { runtime: 'edge' };

export default async function handler(req) {
  const url = new URL(req.url);
  const platform = url.searchParams.get('platform') ?? '';
  const pattern = PLATFORM_TO_ASSET_PATTERN[platform];
  if (!pattern) {
    return text(
      `unknown platform "${platform}" — one of: ${Object.keys(PLATFORM_TO_ASSET_PATTERN).join(', ')}`,
      404
    );
  }

  // Drafts and prereleases are not something a human should land on from a
  // download button; the cli-v* / vscode-v* streams are filtered by prefix.
  let latest;
  try {
    latest = await latestRelease({
      tagPrefix: 'desktop-v',
      perPage: 20,
      userAgent: 'svrnme.sh-download',
    });
  } catch (e) {
    console.error('[download] github fetch failed', e);
    return text('upstream error', 502);
  }
  if (!latest) return text('no desktop release found', 404);
  const release = latest.release;

  const asset = release.assets.find((a) => pattern.test(a.name));
  if (!asset) return text(`no ${platform} asset in ${release.tag_name}`, 404);

  return new Response(null, {
    status: 302,
    headers: {
      location: asset.browser_download_url,
      // Cache the redirect briefly at the edge so a release burst doesn't
      // hammer the GitHub API, but a new publish shows up within minutes.
      'cache-control': 'public, max-age=0, s-maxage=300',
    },
  });
}

function text(body, status) {
  return new Response(body, {
    status,
    headers: { 'content-type': 'text/plain; charset=utf-8' },
  });
}
