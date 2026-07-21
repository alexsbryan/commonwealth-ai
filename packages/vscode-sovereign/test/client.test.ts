import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { completeFim, probeStatus } from "../src/client";
import { startMockDaemon, type MockDaemon } from "./fixtures/mock-daemon.mjs";

let mock: MockDaemon;
beforeAll(async () => {
  mock = await startMockDaemon();
});
afterAll(async () => {
  await mock.close();
});

describe("completeFim", () => {
  it("accumulates SSE into final text with debug + finish_reason", async () => {
    const ctrl = new AbortController();
    const r = await completeFim(
      mock.endpoint,
      { prefix: "def add(a, b):\n    ", suffix: "\n", path: "t.py", language: "python" },
      ctrl.signal,
    );
    expect(r.text).toBe("return a + b;");
    expect(r.finishReason).toBe("stop");
    expect(r.debug?.stop_rule).toBe("newline");
    expect(r.debug?.timings_ms?.ttft).toBe(12);
    // Rich wire shape went out.
    expect(mock.state.lastRequestBody?.prefix).toContain("def add");
    expect(mock.state.lastRequestBody?.stream).toBe(true);
    expect(mock.state.lastRequestBody?.debug).toBe(true);
  });

  it("abort actually closes the socket mid-stream", async () => {
    mock.state.mode = "slow";
    const ctrl = new AbortController();
    const pending = completeFim(mock.endpoint, { prefix: "x", suffix: "" }, ctrl.signal);
    // Let the first chunk land, then abort.
    await new Promise((r) => setTimeout(r, 100));
    ctrl.abort();
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
    // The server must have SEEN the close — an abort that leaves the
    // connection hanging would keep the model generating.
    await new Promise((r) => setTimeout(r, 100));
    expect(mock.state.aborted).toBe(true);
    mock.state.mode = "happy";
  });

  it("surfaces the daemon's 503 message verbatim", async () => {
    mock.state.mode = "error503";
    const ctrl = new AbortController();
    await expect(
      completeFim(mock.endpoint, { prefix: "x", suffix: "" }, ctrl.signal),
    ).rejects.toMatchObject({
      name: "DaemonError",
      status: 503,
      message: expect.stringContaining("[models.fim]"),
    });
    mock.state.mode = "happy";
  });

  it("reports unreachable daemons with the start hint", async () => {
    const ctrl = new AbortController();
    await expect(
      completeFim("http://127.0.0.1:1", { prefix: "x", suffix: "" }, ctrl.signal),
    ).rejects.toMatchObject({
      name: "DaemonError",
      message: expect.stringContaining("sovereign daemon run"),
    });
  });
});

describe("probeStatus", () => {
  it("reads inference.fim when configured", async () => {
    const s = await probeStatus(mock.endpoint);
    expect(s.daemonUp).toBe(true);
    expect(s.fim?.model_id).toBe("mock-coder-1b");
  });

  it("reports fim=null when the daemon has no FIM slot", async () => {
    mock.state.mode = "noFim";
    const s = await probeStatus(mock.endpoint);
    expect(s.daemonUp).toBe(true);
    expect(s.fim).toBeNull();
    mock.state.mode = "happy";
  });

  it("reports daemonUp=false on connection failure", async () => {
    const s = await probeStatus("http://127.0.0.1:1", 500);
    expect(s.daemonUp).toBe(false);
  });
});
