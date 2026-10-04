// SPDX-License-Identifier: AGPL-3.0-or-later
// POST /api/subscribe — append an email to a Resend audience.
//
// Backend choice: Resend Audiences (https://resend.com/audiences). One HTTP
// call, no SDK, free tier of 3k contacts. Swap by editing forwardToBackend().
//
// Required env:
//   RESEND_API_KEY       — server-side, from resend.com dashboard
//   RESEND_AUDIENCE_ID   — UUID of the audience to append to
//
// Optional env:
//   ALLOWED_ORIGIN       — CORS origin allowlist (defaults to same-origin)

const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export const config = { runtime: 'edge' };

export default async function handler(req) {
  if (req.method === 'OPTIONS') return cors(new Response(null, { status: 204 }), req);
  if (req.method !== 'POST') return cors(json({ error: 'method not allowed' }, 405), req);

  let body;
  try {
    body = await req.json();
  } catch {
    return cors(json({ error: 'invalid json body' }, 400), req);
  }

  const email = typeof body?.email === 'string' ? body.email.trim().toLowerCase() : '';
  if (!email || email.length > 320 || !EMAIL_RE.test(email)) {
    return cors(json({ error: "that doesn't look like an email" }, 400), req);
  }

  try {
    const result = await forwardToBackend(email, req);
    return cors(json({ ok: true, ...result }, 200), req);
  } catch (err) {
    const status = err?.status ?? 500;
    const message = err?.publicMessage ?? 'subscribe backend unavailable';
    console.error('[subscribe] failed', {
      status,
      upstreamStatus: err?.upstreamStatus,
      upstreamBody: err?.upstreamBody,
      email,
    });
    return cors(json({
      error: message,
      upstream_status: err?.upstreamStatus,
      upstream_body: err?.upstreamBody,
    }, status), req);
  }
}

async function forwardToBackend(email, req) {
  const apiKey = process.env.RESEND_API_KEY;
  const audienceId = process.env.RESEND_AUDIENCE_ID;

  if (!apiKey || !audienceId) {
    // Dev fallback: log and accept so the form is usable without secrets wired.
    console.log('[subscribe] no Resend env configured — accepting locally:', email);
    return { backend: 'local-log' };
  }

  const url = `https://api.resend.com/audiences/${encodeURIComponent(audienceId)}/contacts`;
  const res = await fetch(url, {
    method: 'POST',
    headers: {
      'authorization': `Bearer ${apiKey}`,
      'content-type': 'application/json',
    },
    body: JSON.stringify({ email }),
  });

  const text = await res.text();
  console.log('[subscribe] resend response', { status: res.status, body: text.slice(0, 500) });

  if (res.ok) return { backend: 'resend' };

  // Resend signals duplicates with status 409 (`validation_error` / `email already exists`).
  // Treat as success — the contact is on the list either way.
  if (res.status === 409 || /already.*(exist|subscrib)|duplicate/i.test(text)) {
    return { backend: 'resend', duplicate: true };
  }

  // Try to extract a readable upstream message.
  let upstreamMessage = text.slice(0, 300);
  try {
    const parsed = JSON.parse(text);
    upstreamMessage = parsed?.message ?? parsed?.error?.message ?? parsed?.name ?? upstreamMessage;
  } catch {}

  const err = new Error(`resend ${res.status}: ${upstreamMessage}`);
  err.status = res.status >= 500 ? 502 : res.status === 401 || res.status === 403 ? 500 : 400;
  err.publicMessage = res.status >= 500
    ? 'email service unavailable'
    : res.status === 401 || res.status === 403
      ? 'email backend misconfigured (check server logs)'
      : `could not add that email: ${upstreamMessage}`;
  err.upstreamStatus = res.status;
  err.upstreamBody = upstreamMessage;
  throw err;
}

function json(obj, status = 200) {
  return new Response(JSON.stringify(obj), {
    status,
    headers: { 'content-type': 'application/json; charset=utf-8' },
  });
}

function cors(res, req) {
  const origin = req.headers.get('origin') ?? '';
  const allow = process.env.ALLOWED_ORIGIN;
  if (allow && origin && (allow === '*' || allow.split(',').map(s => s.trim()).includes(origin))) {
    res.headers.set('access-control-allow-origin', allow === '*' ? '*' : origin);
    res.headers.set('access-control-allow-methods', 'POST, OPTIONS');
    res.headers.set('access-control-allow-headers', 'content-type');
    res.headers.set('vary', 'origin');
  }
  return res;
}
