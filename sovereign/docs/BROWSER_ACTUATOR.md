# Driving a browser from Sovereign

Sovereign can drive a real web browser — open pages, read them, click,
type, submit forms — by speaking to [`@playwright/mcp`](https://github.com/microsoft/playwright-mcp)
as an ordinary MCP server. There is nothing browser-specific in the agent:
the browser is just another set of tools in the registry, reached over the
same MCP path as any other server. This is the first heterogeneous-app
actuator (the path that later generalises to spreadsheets, slides, and a
design canvas).

## Setup

1. **Install the browser** (one-time, ~115 MB). Playwright MCP defaults to
   the system "chrome" channel; install the bundled build instead:

   ```bash
   npx @playwright/mcp@latest install-browser chrome-for-testing
   ```

2. **Register the server** in `~/.svrnmesh/config.toml`:

   ```toml
   [[mcp_servers]]
   name = "playwright"
   enabled = true

   [mcp_servers.transport]
   type = "stdio"
   command = "npx"
   args = [
     "-y", "@playwright/mcp@latest",
     "--headless",          # drop for a visible browser
     "--no-sandbox",        # required inside containers / toolboxes
     "--isolated",          # fresh profile per run
     "--browser", "chromium",
   ]
   ```

   (or add it from **Settings → MCP** / `sovereign mcp add`.)

3. **Restart** the daemon (or chat surface). `sovereign tools list` now shows
   23 `mcp_playwright_browser_*` tools.

## How it behaves inside the agent

- **One persistent session.** The stdio server is a single long-lived child
  owned by the process-lifetime tool registry, so the browser — and its
  page, cookies, and scroll position — survives across every tool call in a
  task (and across turns, on the daemon). The planner can navigate, read,
  then act, and the second call sees the first call's page.

- **Reads and writes are classified automatically.** MCP servers don't
  declare behaviour, so Sovereign infers it from the tool name
  (`sovereign-tools/src/mcp/mod.rs::infer_behaviour`). Browser *mutations*
  (`browser_click`, `browser_type`, `browser_fill_form`, `browser_press_key`,
  `browser_select_option`, `browser_file_upload`, …) are **Write +
  NonIdempotent**, so the approval gate fires and the replay-safety ledger
  ([step-execution idempotency](./SYSTEM_OVERVIEW.md#4-sovereign--the-local-agent))
  guards them — a form submit won't fire twice across a crash or a replan.
  Browser *reads* (`browser_snapshot`, `browser_take_screenshot`,
  `browser_navigate`, `browser_console_messages`, …) are **Read +
  Idempotent**: no ledger row, freely retryable. Genuinely ambiguous tools
  (`browser_evaluate`, `browser_run_code_unsafe`, `browser_tabs`) stay in the
  conservative Write/NonIdempotent quadrant.

## Proof

`sovereign-tools/tests/playwright_actuator.rs` drives real headless Chromium
through the production `McpServerManager::from_config` path: it registers the
server, asserts the classification on the live tool metadata, then
navigate → snapshot → type-and-submit against a local fixture and asserts the
form POST reached the server. It is `#[ignore]`d (CI has no browser); run it
where `npx` + the browser are installed:

```bash
cargo test -p sovereign-tools --test playwright_actuator -- --ignored --nocapture
```

## Notes and gotchas

- **stdout must be pure JSON-RPC.** Sovereign's stdio transport is strict
  request/response, so a server that printed logs to stdout would desync.
  `@playwright/mcp` logs to stderr — verified clean.
- **Bundled Chromium vs the chrome channel.** Without `--browser chromium`,
  Playwright MCP looks for system Google Chrome (`/opt/google/chrome`) and the
  `chrome-for-testing` distribution; the `install-browser` step above and the
  flag are what make it use the bundled build.
- **Untrusted content.** A web page is untrusted input. Today the containment
  is least-privilege + the approval gate on writes; treating page text as
  *data, never instructions* (taint / quarantine) is the separate injection
  track, not yet built. Don't point this at the open web on a session that
  also holds dangerous capabilities.
</content>
