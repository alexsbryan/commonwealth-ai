// SPDX-License-Identifier: AGPL-3.0-or-later
// Minimal local dev server — serves static files at root, proxies /api/* to
// the same Edge handler that runs on Vercel. No bundler, no watch loop.
//
// Usage:  node scripts/dev-server.js              # port 3000
//         PORT=4000 node scripts/dev-server.js

import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { extname, join, normalize, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { pathToFileURL } from 'node:url';

const ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)));
const PORT = Number(process.env.PORT ?? 3000);

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.css':  'text/css; charset=utf-8',
  '.js':   'application/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg':  'image/svg+xml',
  '.png':  'image/png',
  '.ico':  'image/x-icon',
  '.sh':   'text/x-shellscript; charset=utf-8',
  '.txt':  'text/plain; charset=utf-8',
};

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url ?? '/', `http://${req.headers.host}`);

    if (url.pathname.startsWith('/api/')) {
      return await serveApi(url, req, res);
    }

    let path = decodeURIComponent(url.pathname);
    if (path === '/' || path.endsWith('/')) path += 'index.html';
    if (!extname(path)) path += '.html';

    const filePath = normalize(join(ROOT, path));
    if (!filePath.startsWith(ROOT)) return send(res, 403, 'forbidden');

    try {
      const st = await stat(filePath);
      if (!st.isFile()) throw new Error('not a file');
    } catch {
      return send(res, 404, 'not found');
    }

    const body = await readFile(filePath);
    res.writeHead(200, { 'content-type': MIME[extname(filePath)] ?? 'application/octet-stream' });
    res.end(body);
  } catch (err) {
    console.error('[dev-server]', err);
    send(res, 500, 'internal error');
  }
});

async function serveApi(url, req, res) {
  // Map /api/subscribe → ./api/subscribe.js (mirrors Vercel's filesystem routing).
  const file = url.pathname.replace(/^\/api\//, '') + '.js';
  const mod = await import(pathToFileURL(join(ROOT, 'api', file)).href).catch(() => null);
  if (!mod?.default) return send(res, 404, 'no such endpoint');

  const chunks = [];
  for await (const c of req) chunks.push(c);
  const bodyBuf = Buffer.concat(chunks);

  const webReq = new Request(url.href, {
    method: req.method,
    headers: req.headers,
    body: req.method === 'GET' || req.method === 'HEAD' ? undefined : bodyBuf,
  });

  const webRes = await mod.default(webReq);
  const out = await webRes.arrayBuffer();
  const headers = {};
  webRes.headers.forEach((v, k) => { headers[k] = v; });
  res.writeHead(webRes.status, headers);
  res.end(Buffer.from(out));
}

function send(res, status, msg) {
  res.writeHead(status, { 'content-type': 'text/plain; charset=utf-8' });
  res.end(msg);
}

server.listen(PORT, () => {
  console.log(`sovereign-landing dev server  →  http://localhost:${PORT}`);
});
