// Tauri updater manifest for the Sovereign desktop app.
//
// The desktop's `tauri-plugin-updater` polls this endpoint with the user's
// target triple + currently-installed version. We query GitHub Releases for
// the latest `desktop-v*` tag, compare versions, and either:
//   - 204 No Content  -> the user is up to date
//   - 200 JSON        -> manifest pointing at the right per-platform artifact
//                         + its signature
//
// The plugin's URL pattern is interpolated by Tauri at request time:
//   https://svrnme.sh/api/desktop/updater/{{target}}/{{current_version}}
// where {{target}} resolves to e.g. `darwin-aarch64` and {{current_version}}
// to the running app's semver. Vercel's path-segment rewrite (see vercel.json)
// turns those segments into the `target` + `current_version` query params
// this handler reads.
//
// Required env vars (set in the Vercel project settings):
//   GITHUB_OWNER   -- repo owner, e.g. "alexsbryan"
//   GITHUB_REPO    -- repo name,  e.g. "commonwealth-ai"
//
// Optional:
//   GITHUB_TOKEN   -- raises the API rate limit from 60/h to 5000/h. Not
//                     required for a public repo but worth setting once
//                     the install base grows.

const PLATFORM_TO_ASSET_PATTERN = {
  // Tauri target triples on the LEFT, regex matching the bundle artifact
  // name on the RIGHT. Patterns match against Vercel-side string regex,
  // so escape carefully.
  'darwin-aarch64': /Sovereign[._].*aarch64.*\.app\.tar\.gz$/i,
  'darwin-x86_64':  /Sovereign[._].*x64.*\.app\.tar\.gz$/i,
  'linux-x86_64':   /sovereign[._].*amd64.*\.AppImage$/i,
  'windows-x86_64': /Sovereign[._].*x64.*-setup\.exe$/i,
};

export const config = { runtime: 'edge' };

export default async function handler(req) {
  const url = new URL(req.url);
  const target = url.searchParams.get('target') ?? '';
  const currentVersion = url.searchParams.get('current_version') ?? '';

  if (!target || !currentVersion) {
    return text('missing target or current_version', 400);
  }
  if (!PLATFORM_TO_ASSET_PATTERN[target]) {
    return text(`unsupported target: ${target}`, 400);
  }

  const owner = process.env.GITHUB_OWNER;
  const repo  = process.env.GITHUB_REPO;
  if (!owner || !repo) {
    console.error('[updater] GITHUB_OWNER / GITHUB_REPO not configured');
    return text('updater backend not configured', 500);
  }

  const headers = {
    'accept': 'application/vnd.github+json',
    'user-agent': 'sovereign-updater/1',
  };
  if (process.env.GITHUB_TOKEN) {
    headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  }

  // GitHub's /releases endpoint returns up to 30 most recent releases,
  // newest first. We filter for `desktop-v*` tags (the repo also ships
  // `cli-v*` and `daemon-v*` releases on separate cadences) and skip
  // drafts so a partial cut doesn't accidentally roll out.
  const releasesUrl = `https://api.github.com/repos/${owner}/${repo}/releases?per_page=30`;
  let releases;
  try {
    const res = await fetch(releasesUrl, { headers });
    if (!res.ok) {
      console.error('[updater] github releases', { status: res.status });
      return text(`github api ${res.status}`, 502);
    }
    releases = await res.json();
  } catch (e) {
    console.error('[updater] github fetch failed', e);
    return text('github api unreachable', 502);
  }

  const latest = releases.find(r =>
    !r.draft && typeof r.tag_name === 'string' && r.tag_name.startsWith('desktop-v')
  );
  if (!latest) {
    return text('no desktop release found', 404);
  }

  const latestVersion = latest.tag_name.replace(/^desktop-v/, '');
  if (!isNewer(latestVersion, currentVersion)) {
    // The plugin treats 204 as "you're up to date". This is the
    // happy-path response for every poll after the first install.
    return new Response(null, {
      status: 204,
      headers: { 'cache-control': 'public, max-age=60, s-maxage=60' },
    });
  }

  // Find the artifact + its .sig sidecar for this target.
  const pattern = PLATFORM_TO_ASSET_PATTERN[target];
  const asset = latest.assets.find(a => pattern.test(a.name));
  const sigAsset = asset && latest.assets.find(a => a.name === `${asset.name}.sig`);

  if (!asset || !sigAsset) {
    console.warn('[updater] missing asset or sig', {
      target,
      latestVersion,
      assetFound: !!asset,
      sigFound: !!sigAsset,
    });
    return text(`no signed artifact for ${target} at ${latestVersion}`, 404);
  }

  // The .sig is a tiny base64 blob produced by `tauri-bundler` at build
  // time. We inline it into the manifest body — the plugin verifies it
  // against the embedded pubkey before applying the downloaded artifact.
  let signature;
  try {
    const sigRes = await fetch(sigAsset.browser_download_url);
    if (!sigRes.ok) throw new Error(`sig fetch ${sigRes.status}`);
    signature = (await sigRes.text()).trim();
  } catch (e) {
    console.error('[updater] sig fetch failed', e);
    return text('signature unavailable', 502);
  }

  const manifest = {
    version: latestVersion,
    notes: stripMarkdown(latest.body ?? ''),
    pub_date: latest.published_at,
    platforms: {
      [target]: {
        signature,
        url: asset.browser_download_url,
      },
    },
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

// SemVer comparison: returns true iff `a` is strictly newer than `b`.
// Handles pre-release tags by lexical compare on the suffix (1.0.0 > 1.0.0-rc1
// per semver section 11; pre-release < release).
function isNewer(a, b) {
  const parse = v => {
    const [main, pre = ''] = String(v).split('-');
    const [maj, min, pat] = main.split('.').map(n => parseInt(n, 10));
    return { maj: maj || 0, min: min || 0, pat: pat || 0, pre };
  };
  const A = parse(a);
  const B = parse(b);
  if (A.maj !== B.maj) return A.maj > B.maj;
  if (A.min !== B.min) return A.min > B.min;
  if (A.pat !== B.pat) return A.pat > B.pat;
  if (!A.pre && B.pre) return true;       // 1.0.0 > 1.0.0-rc1
  if (A.pre && !B.pre) return false;
  return A.pre > B.pre;
}
