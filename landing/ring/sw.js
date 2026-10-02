// The app-shell cache. Cache-first, one version: 0a71a2b5baf0 is the runtime's
// content hash, stamped at build time, so a rebuild replaces the cache rather
// than serving a stale runtime (the failure this exists to avoid is a guest
// stuck on last week's wasm). The version is also on every shell URL.
const CACHE = 'ring-runtime-0a71a2b5baf0';
const SHELL = [
  '/ring',
  '/ring/app.js?v=0a71a2b5baf0',
  '/ring/wasm/ring_runtime.js?v=0a71a2b5baf0',
  '/ring/wasm/ring_runtime_bg.wasm?v=0a71a2b5baf0',
];

self.addEventListener('install', (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener('activate', (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

// Only the shell is cached. The dial itself is not HTTP to this origin — it is
// iroh — so requests to it never reach here.
self.addEventListener('fetch', (e) => {
  const url = new URL(e.request.url);
  if (url.origin !== self.location.origin || e.request.method !== 'GET') return;
  e.respondWith(
    caches.match(e.request, { ignoreSearch: true }).then((hit) => hit || fetch(e.request)),
  );
});
