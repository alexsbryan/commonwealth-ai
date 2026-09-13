# The sovereign daemon (:9741) does NOT serve /healthz or /health (both 404 — /health lives only in sovereign-server). Liveness is /v1/models…

Discovered 2026-07-20 while building scripts/desktop-soak.py: its preflight probed /healthz and
aborted instantly. The daemon on :9741 (sovereign-cli-daemon) serves /status and /v1/models (both
200) but /healthz and /health return 404 — the only /health route is in a DIFFERENT binary
(sovereign-server/src/main.rs:812). The runner's own harness already uses /v1/models
(harness.mjs discoverBrainModel).

The trap: `curl -s http://127.0.0.1:9741/healthz && echo ok` reports OK because curl EXITS 0 on a
404 (it got an HTTP response) — so bash health gates silently "pass" on a nonexistent endpoint.
Python's urllib.request.urlopen RAISES HTTPError on 4xx, so it correctly fails. Any health-gate must
require HTTP 200 from /v1/models (or /status), not "curl succeeded."

The pre-existing scripts/soak-persona.sh carries the same latent /healthz assumption; it only
survives because curl+&& accepts the 404. Related: [[reference_desktop_soak_script]]
