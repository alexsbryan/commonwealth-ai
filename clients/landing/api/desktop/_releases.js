// SPDX-License-Identifier: AGPL-3.0-or-later
// "Which published release is newest for tag prefix P, and where does it
// live" — the one decider both edge endpoints use.
//
// (The leading `_` keeps Vercel from routing this file as a function; it is a
// shared module, not an endpoint.)
//
// ─── Why two repos ──────────────────────────────────────────────────────
// Releases publish to the source repo, `alexsbryan/commonwealth-ai`, now that
// it is public. Until 2026-08-31 they went to `alexsbryan/svrnmesh-releases` —
// a public shelf that existed only because a private repo's release assets are
// not anonymously fetchable. That indirection is retired, but the shelf still
// holds every current asset (desktop-v0.4.0, cli-v0.6.0) until the next
// release is cut, and apps installed before the move poll these same
// endpoints. So both repos are queried and the winner is the MAX SEMVER across
// them.
//
// Max-semver across, not first-repo-with-a-hit: commonwealth-ai carries stale
// `desktop-v0.1.19` / `cli-v0.1.19` tags from July, so "first repo that has
// any" would roll every user BACKWARD by three minor versions.
//
// Retiring the fallback: set GITHUB_FALLBACK_REPO="" in the Vercel project
// (or delete DEFAULT_FALLBACK below) once the source repo carries the newest
// release of every stream.
//
// Env (Vercel project settings; every one has a working default):
//   GITHUB_OWNER          primary repo owner  (default "alexsbryan")
//   GITHUB_REPO           primary repo name   (default "commonwealth-ai")
//   GITHUB_FALLBACK_REPO  "owner/name" of the retired shelf; "" disables it
//   GITHUB_TOKEN          raises the API rate limit from 60/h to 5000/h.
//                         STRONGLY recommended: the anonymous 60/h is shared
//                         across Vercel's egress IPs, so under any load GitHub
//                         403s -> these endpoints 502 -> the app silently shows
//                         "up to date". Set it. It matters twice as much while
//                         two repos are queried per request.

const DEFAULT_OWNER = 'alexsbryan';
const DEFAULT_REPO = 'commonwealth-ai';
const DEFAULT_FALLBACK = 'alexsbryan/svrnmesh-releases';

/** Repos to query, in order. Order is the tie-break: primary wins a draw. */
export function releaseRepos() {
  const owner = process.env.GITHUB_OWNER || DEFAULT_OWNER;
  const repo = process.env.GITHUB_REPO || DEFAULT_REPO;
  const primary = `${owner}/${repo}`;
  // `??` not `||`: GITHUB_FALLBACK_REPO="" is how you turn the shelf off.
  const fallback = process.env.GITHUB_FALLBACK_REPO ?? DEFAULT_FALLBACK;
  return fallback && fallback !== primary ? [primary, fallback] : [primary];
}

export function githubHeaders(userAgent) {
  const headers = {
    accept: 'application/vnd.github+json',
    'user-agent': userAgent,
  };
  if (process.env.GITHUB_TOKEN) {
    headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  }
  return headers;
}

// SemVer comparison: returns true iff `a` is strictly newer than `b`.
// Handles pre-release tags by lexical compare on the suffix (1.0.0 > 1.0.0-rc1
// per semver section 11; pre-release < release).
export function isNewer(a, b) {
  const parse = (v) => {
    const [main, pre = ''] = String(v).split('-');
    const [maj, min, pat] = main.split('.').map((n) => parseInt(n, 10));
    return { maj: maj || 0, min: min || 0, pat: pat || 0, pre };
  };
  const A = parse(a);
  const B = parse(b);
  if (A.maj !== B.maj) return A.maj > B.maj;
  if (A.min !== B.min) return A.min > B.min;
  if (A.pat !== B.pat) return A.pat > B.pat;
  if (!A.pre && B.pre) return true; // 1.0.0 > 1.0.0-rc1
  if (A.pre && !B.pre) return false;
  return A.pre > B.pre;
}

/**
 * Newest published release carrying `tagPrefix`, across every configured repo.
 *
 * Selection is MAX SEMVER, never GitHub's list order: `/releases` sorts by
 * `created_at`, and every release here shares one (it derives from the tagged
 * commit's date), so the order among them is an unstable internal tiebreak.
 * Trusting `[0]` handed an app 0.2.0 instead of 0.2.1 during the replication
 * window right after publish (2026-07-15); install.sh hit the same trap.
 *
 * Returns `{ release, version, repo, fellBack }`, or `null` when no repo has
 * one. Throws only when EVERY repo's call failed — a fallback that 502s must
 * not turn a working primary into an outage, and vice versa.
 */
export async function latestRelease({
  tagPrefix,
  includePrerelease = false,
  perPage = 30,
  userAgent = 'svrnme.sh',
}) {
  const repos = releaseRepos();
  const headers = githubHeaders(userAgent);
  let best = null;
  let errors = 0;

  for (const repo of repos) {
    let payload;
    try {
      const res = await fetch(
        `https://api.github.com/repos/${repo}/releases?per_page=${perPage}`,
        { headers }
      );
      if (!res.ok) throw new Error(`github api ${res.status}`);
      payload = await res.json();
    } catch (e) {
      console.error('[releases] fetch failed', { repo, err: String(e) });
      errors++;
      continue;
    }
    if (!Array.isArray(payload)) continue;

    for (const r of payload) {
      if (r.draft) continue;
      if (r.prerelease && !includePrerelease) continue;
      if (typeof r.tag_name !== 'string' || !r.tag_name.startsWith(tagPrefix)) continue;
      const version = r.tag_name.slice(tagPrefix.length);
      // Strictly newer, so on a tie the earlier repo (the primary) keeps it.
      if (best === null || isNewer(version, best.version)) {
        best = { release: r, version, repo, fellBack: repo !== repos[0] };
      }
    }
  }

  if (best === null && errors === repos.length) {
    throw new Error(`every release repo failed (${repos.join(', ')})`);
  }
  if (best?.fellBack) {
    console.warn('[releases] newest is on the retired shelf', {
      tagPrefix,
      repo: best.repo,
      version: best.version,
    });
  }
  return best;
}
