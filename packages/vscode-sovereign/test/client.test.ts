import { afterAll, beforeAll, describe, expect, it } from "vitest";
import {
  completeFim,
  predictEdits,
  probeStatus,
  servesFim,
  servesNextEdit,
} from "../src/client";
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
      message: expect.stringContaining("[models.edit]"),
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
  it("reads inference.edit, with both lanes, when fully specialised", async () => {
    const s = await probeStatus(mock.endpoint);
    expect(s.daemonUp).toBe(true);
    expect(s.edit?.model_id).toBe("mock-coder-1b");
    expect(s.edit?.slot).toBe("edit");
    expect(servesFim(s.edit!)).toBe(true);
    expect(servesNextEdit(s.edit!)).toBe(true);
    // Nothing to say about an arrangement that is already right.
    expect(s.edit?.advice).toBeUndefined();
    expect(s.edit?.degraded).toBe(false);
  });

  it("falls back to inference.fim against a pre-two-lane daemon", async () => {
    // The old key is the ONLY one such a daemon emits; reading `edit`
    // alone would report "no editing model" against a healthy install.
    mock.state.mode = "legacy";
    const s = await probeStatus(mock.endpoint);
    expect(s.daemonUp).toBe(true);
    expect(s.edit?.model_id).toBe("mock-coder-1b");
    expect(servesFim(s.edit!)).toBe(true);
    // Fields the old daemon cannot supply read as absent, not as false
    // claims about the arrangement.
    expect(s.edit?.degraded).toBeUndefined();
    expect(s.edit?.advice).toBeUndefined();
    mock.state.mode = "happy";
  });

  it("reports edit=null when the daemon has no editing model at all", async () => {
    mock.state.mode = "noEdit";
    const s = await probeStatus(mock.endpoint);
    expect(s.daemonUp).toBe(true);
    expect(s.edit).toBeNull();
    mock.state.mode = "happy";
  });

  it("reports a next-edit-only model as served, not as missing", async () => {
    // A chat model with no FIM markers: /v1/completions 503s by design
    // and /v1/edit_predictions works. Absence of fim_style must never
    // read as a broken daemon.
    mock.state.mode = "nextEditOnly";
    const s = await probeStatus(mock.endpoint);
    expect(s.edit).not.toBeNull();
    expect(servesNextEdit(s.edit!)).toBe(true);
    expect(servesFim(s.edit!)).toBe(false);
    expect(s.edit?.fim_style).toBeUndefined();
    expect(s.edit?.degraded).toBe(false);
    expect(s.edit?.advice).toContain("[models.edit].path");
    mock.state.mode = "happy";
  });

  it("surfaces degraded + the daemon's advice verbatim", async () => {
    mock.state.mode = "degraded";
    const s = await probeStatus(mock.endpoint);
    expect(s.edit?.degraded).toBe(true);
    expect(s.edit?.slot).toBe("fast");
    expect(s.edit?.aliased_to_fast).toBe(true);
    expect(servesNextEdit(s.edit!)).toBe(true);
    // Rendered as-is by the status bar and the diagnose ladder — this
    // string is composed in exactly one place, the daemon.
    expect(s.edit?.advice).toContain("resident chat model");
    expect(s.edit?.advice).toContain("3x faster");
    mock.state.mode = "happy";
  });

  it("reports daemonUp=false on connection failure", async () => {
    const s = await probeStatus("http://127.0.0.1:1", 500);
    expect(s.daemonUp).toBe(false);
  });
});

describe("predictEdits", () => {
  it("posts history + text and parses edits with the debug block", async () => {
    mock.state.mode = "happy";
    const ctrl = new AbortController();
    const r = await predictEdits(
      mock.endpoint,
      {
        history: [{ before: "log", after: "debug", left: "console.", right: "(1);" }],
        text: "abc; console.log(2);",
        cursor: 4,
        path: "t.ts",
        language: "typescript",
      },
      ctrl.signal,
    );
    expect(r.engine).toBe("rule");
    expect(r.edits).toEqual([{ start: 5, end: 17, new_text: "console.debug(" }]);
    expect(r.debug?.rule_key).toBe('["console.log(","console.debug("]');
    expect(r.debug?.support).toBe(2);
    // Debug always requested; history rode along verbatim.
    expect(mock.state.lastEditPredictionBody?.debug).toBe(true);
    expect(mock.state.lastEditPredictionBody?.history?.[0]?.before).toBe("log");
    expect(mock.state.lastEditPredictionBody?.cursor).toBe(4);
  });

  it("surfaces the daemon's actionable 400 as DaemonError", async () => {
    mock.state.mode = "error400";
    const ctrl = new AbortController();
    await expect(
      predictEdits(mock.endpoint, { history: [], text: "x", cursor: 0 }, ctrl.signal),
    ).rejects.toMatchObject({
      name: "DaemonError",
      message: expect.stringContaining("caps the search space"),
    });
    mock.state.mode = "happy";
  });
});
