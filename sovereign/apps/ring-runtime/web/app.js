// The guest page's logic. External (not inline) so a strict CSP can run here
// — see index.html's `script-src 'self' 'wasm-unsafe-eval'`: the wasm module
// needs `wasm-unsafe-eval`, and nothing else needs an allowance.
//
// The version query on the wasm import is stamped at build time (the content
// hash of the module), so a service worker or CDN cache cannot serve a stale
// runtime after a rebuild.
import init, { dial_guest } from '/ring/wasm/ring_runtime.js?v=__VERSION__';

const status = document.getElementById('status');
const response = document.getElementById('response');
const traceEl = document.getElementById('trace');
const labelEl = document.getElementById('label');

const lines = [];
window.__trace = (l) => {
  lines.push(l);
  traceEl.textContent = lines.join('\n');
};

// The link's fragment: token, exp, iroh (the dial string), s (the label),
// path (optional — the grant-scoped route; default is the rail read). The
// fragment is never sent to this page's origin, which is why the credential
// rides there and not in the query.
const params = new URLSearchParams(location.hash.slice(1));
const token = params.get('token') || '';
const dial = params.get('iroh') || '';
const path = params.get('path') || '/v1/rail/log';
const label = params.get('s') || '';
if (label) labelEl.textContent = label;

function fail(msg) {
  status.textContent = msg;
  status.className = 'bad';
}

if (!token || !dial) {
  fail('Open this page from a guest link — its fragment carries token= and iroh=.');
} else {
  try {
    await init();
    status.textContent = 'Dialing the wall over the relay…';
    const pending = dial_guest(dial, token, path);
    const timeout = new Promise((r) => setTimeout(() => r('__TIMEOUT__'), 120000));
    const res = await Promise.race([pending, timeout]);
    if (res === '__TIMEOUT__') {
      fail('Timed out — the last layer below is where it hung.');
    } else {
      const body = (res.split('RESPONSE-BODY: ')[1] || '').trim();
      const st = (res.match(/RESPONSE-STATUS: (.*)/) || [])[1] || '';
      status.textContent = st.startsWith('HTTP/1.1 2') ? 'Connected' : 'Refused';
      status.className = st.startsWith('HTTP/1.1 2') ? 'good' : 'bad';
      response.textContent = body || res;
    }
  } catch (e) {
    fail('Could not run the runtime: ' + (e && e.stack ? e.stack : String(e)));
  }
}

// Cache the app shell after first load, so a repeat visit needs no origin
// traffic at all (docs/RING_APP_LIBRARY.md: "no traffic after first load").
// Registered last and best-effort: the page works without it, and a failed
// registration must never break the dial.
if ('serviceWorker' in navigator) {
  window.addEventListener('load', () => {
    navigator.serviceWorker.register('/ring/sw.js').catch(() => {});
  });
}
