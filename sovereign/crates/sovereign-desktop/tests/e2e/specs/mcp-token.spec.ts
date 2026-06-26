// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat, type Page } from "../fixtures/test-base";

// MCP bearer token — the file-backed secret store flow (Workshop → Connect).
//
// Before: enabling "Bearer auth" gave no way to enter a token (it was read
// from an env var). Now there's a real token field that writes to the app's
// secret store (`mcp_set_token` → ~/.sovereign/secrets), for both a new
// server and an existing one's row.

async function seedMcp(page: Page, servers: unknown[]) {
  await page.evaluate((srv) => {
    const w = window as unknown as {
      __sovereign_test__: {
        setHandler: (cmd: string, fn: (a: unknown) => unknown) => void;
      };
    };
    const calls: Record<string, unknown[]> = {};
    (window as unknown as { __mcpCalls: Record<string, unknown[]> }).__mcpCalls = calls;
    const rec = (k: string, a: unknown) => {
      (calls[k] ||= []).push(a);
    };
    w.__sovereign_test__.setHandler("mcp_list_servers", () => srv);
    w.__sovereign_test__.setHandler("mcp_add_server", (a) => (rec("add", a), null));
    w.__sovereign_test__.setHandler("mcp_set_token", (a) => (rec("set_token", a), null));
    w.__sovereign_test__.setHandler("mcp_clear_token", (a) => (rec("clear_token", a), null));
    w.__sovereign_test__.setHandler("mcp_remove_server", (a) => (rec("remove", a), null));
    w.__sovereign_test__.setHandler("mcp_test_connection", () => 3);
  }, servers);
}

async function gotoConnect(page: Page) {
  await page.getByTestId("nav-workshop").click();
  await page.getByTestId("workshop-tab-connect").click();
  await expect(page.getByRole("heading", { name: "Connect tools" })).toBeVisible();
}

function mcpCalls(page: Page) {
  return page.evaluate(
    () => (window as unknown as { __mcpCalls: Record<string, unknown[]> }).__mcpCalls,
  );
}

test.describe("MCP bearer token", () => {
  test("enabling bearer reveals a token field; Add stores the token", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedMcp(page, []);
    await gotoConnect(page);

    // No token field until bearer is on.
    await expect(page.getByTestId("mcp-token-input")).toHaveCount(0);
    await page.locator('.add-form input[type="checkbox"]').check();
    await expect(page.getByTestId("mcp-token-input")).toBeVisible();

    await page.getByPlaceholder("vision").fill("vision");
    await page.getByPlaceholder("https://host/mcp").fill("https://h/mcp");
    await page.getByTestId("mcp-token-input").fill("sek-123");
    await page.getByRole("button", { name: "Add server" }).click();

    // The config add AND the secret-store write both fired, with the token.
    await expect.poll(async () => (await mcpCalls(page)).set_token?.length ?? 0).toBe(1);
    const calls = await mcpCalls(page);
    expect(calls.add?.length).toBe(1);
    expect(calls.set_token?.[0]).toMatchObject({ name: "vision", token: "sek-123" });
  });

  test("an existing server's token can be set from its row", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedMcp(page, [
      {
        name: "vision",
        url: "https://h/mcp",
        description: null,
        enabled: true,
        bearer: true,
        token_env: "SOVEREIGN_MCP_TOKEN_VISION",
        has_token: false,
        connected: null,
        tool_count: null,
        error: null,
      },
    ]);
    await gotoConnect(page);

    await expect(page.getByText("no token yet")).toBeVisible();
    await page.getByTestId("mcp-row-set-token").click();
    await page.getByTestId("mcp-row-token-input").fill("row-tok");
    await page.getByTestId("mcp-row-token-save").click();

    await expect.poll(async () => (await mcpCalls(page)).set_token?.length ?? 0).toBe(1);
    const calls = await mcpCalls(page);
    expect(calls.set_token?.[0]).toMatchObject({ name: "vision", token: "row-tok" });
  });
});
