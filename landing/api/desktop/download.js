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
// cli-v* stream, and asset names embed the version anyway. Resolution matches
// updater.js: newest published, non-draft, non-prerelease desktop-v* tag.
//
// Same env as updater.js: GITHUB_OWNER, GITHUB_REPO, optional GITHUB_TOKEN.

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

  const owner = process.env.GITHUB_OWNER;
  const repo  = process.env.GITHUB_REPO;
  if (!owner || !repo) {
    console.error('[download] GITHUB_OWNER / GITHUB_REPO not configured');
    return text('server not configured', 500);
  }

  const headers = { accept: 'application/vnd.github+json', 'user-agent': 'svrnme.sh-download' };
  if (process.env.GITHUB_TOKEN) {
    headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  }

  // The unauthenticated /releases list excludes drafts; skip prereleases and
  // anything from the cli-v* stream.
  const res = await fetch(
    `https://api.github.com/repos/${owner}/${repo}/releases?per_page=20`,
    { headers }
  );
  if (!res.ok) {
    console.error(`[download] GitHub API ${res.status}`);
    return text('upstream error', 502);
  }
  const releases = await res.json();
  const release = releases.find(
    (r) => !r.draft && !r.prerelease && r.tag_name.startsWith('desktop-v')
  );
  if (!release) return text('no desktop release found', 404);

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
