# sovereign-landing

The pre-launch landing page for `sovereign.dev`. Hand-rolled HTML, one Edge
function, no framework, no build step.

## What's here

```
landing/
├── index.html         ← the page (CSS inlined, ~10 KB before fonts)
├── favicon.svg
├── install.sh         ← the real curl-pipe installer (pulls from commonwealth-ai)
├── robots.txt
├── api/
│   ├── subscribe.js   ← POST /api/subscribe, forwards to Resend Audiences
│   └── desktop/
│       ├── download.js ← GET /download/:platform → newest desktop-v* asset
│       └── updater.js  ← Tauri updater manifest
├── scripts/
│   └── dev-server.js  ← local preview without vercel CLI
├── package.json       ← no runtime deps; type:module for the dev server
└── vercel.json        ← caching + security headers
```

Anything you drop at the project root becomes a public URL. When the desktop
build is ready, put binaries (or links to GitHub release assets) under
something like `landing/releases/` and they'll be reachable at
`sovereign.dev/releases/...`. Same trick for update manifests — drop them in
`landing/manifests/` and you've got a CDN-cached endpoint with zero code.

## Local preview

```
cd landing
npm run dev        # http://localhost:3000
```

The dev server serves static files at the root and routes `/api/*` to the
matching file under `api/`. It uses the same Web `Request` / `Response`
interface Vercel's Edge runtime uses, so the endpoint code is identical in
both environments.

You can preview the form end-to-end without any secrets — when
`RESEND_API_KEY` is unset the endpoint logs the email and returns success.

## Deploy

```
cd landing
vercel link        # one time
vercel deploy --prod
```

Or wire the GitHub repo to a Vercel project pointing at this subdirectory.
There's no build command. The framework preset is **Other**.

### Env vars (set in the Vercel dashboard)

| Var | Required | Purpose |
|---|---|---|
| `RESEND_API_KEY` | yes (in prod) | Server-side API key from [resend.com](https://resend.com) |
| `RESEND_AUDIENCE_ID` | yes (in prod) | UUID of the audience to append signups to |
| `ALLOWED_ORIGIN` | no | Comma-separated CORS allowlist. Omit to disable CORS (same-origin only) |

Without the Resend vars the endpoint still accepts signups but only logs
them — fine for dev, not fine for prod. Vercel will surface a `console.error`
in the function logs if Resend rejects an email.

## Swapping the email backend

Open `api/subscribe.js`, replace `forwardToBackend()`. The function gets the
validated email and the request; return `{ backend: 'name', ... }` on success
or throw `{ status, publicMessage }` on failure. That's it.

Candidates with a one-call HTTP API: Buttondown, ConvertKit, MailerLite,
Listmonk (self-hosted), or your own Postgres/KV.

## Updating copy

The page is hand-written HTML. Edit `index.html`. The CSS is inlined inside
a single `<style>` block at the top of `<head>` — no preprocessing.

GitHub links point at `github.com/alexsbryan/commonwealth-ai` — the source
repo, which is where the "report a bug" link in the footer lands. Update them
in three places when the canonical URL changes:
- the `<a>` tags in the hero and footer of `index.html`
- the `install.sh` banner
- `package.json` (if you add a `repository` field)

Which repo the *release assets* come from is a separate question, answered in
one place: `api/desktop/_releases.js` (shared by both edge endpoints) and the
matching `REPO` / `SHELF_REPO` pair at the top of `install.sh`. Both still read
the retired `svrnmesh-releases` shelf as a fallback and take the max semver
across the two; see the top-level RELEASING.md for when to drop it. Files under
`api/` whose name starts with `_` are modules, not routes — Vercel does not
turn them into functions.

## Performance budget

The first paint should fit in one HTTP roundtrip plus the font request. To
keep it that way:

- No external CSS or JS files (everything inline).
- No tracking pixels, analytics, or third-party widgets.
- Font is `font-display: swap` so text is readable immediately with the
  system mono fallback.
- Cache headers in `vercel.json` make repeat visits ~free.

If you add anything that breaks this, weigh whether it's worth it.
