// Mock daemon for client tests (extension plan §tests). ~80 lines of
// node:http serving the canned behaviours the client must survive:
// happy-path SSE, slow-drip SSE (abort must actually close the
// socket), HTTP 503 with the actionable body, and /status probes.

import http from "node:http";

export function startMockDaemon() {
  const state = {
    mode: "happy", // happy | slow | error503 | noFim
    lastRequestBody: null,
    aborted: false,
  };

  const server = http.createServer((req, res) => {
    if (req.url === "/status") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          inference:
            state.mode === "noFim"
              ? { fim: null }
              : {
                  fim: {
                    slot: "fim",
                    model_id: "mock-coder-1b",
                    fim_style: "qwen_coder",
                    aliased_to_fast: false,
                  },
                },
        }),
      );
      return;
    }
    if (req.url === "/v1/completions" && req.method === "POST") {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        state.lastRequestBody = JSON.parse(body);
        if (state.mode === "error503") {
          res.writeHead(503, { "content-type": "application/json" });
          res.end(
            JSON.stringify({
              error: { message: "FIM is not configured on this daemon. Add [models.fim] …" },
            }),
          );
          return;
        }
        res.writeHead(200, {
          "content-type": "text/event-stream",
          "cache-control": "no-cache",
        });
        const chunk = (text, finish = null, debug = false) =>
          `data: ${JSON.stringify({
            id: "cmpl-mock",
            object: "text_completion",
            created: 0,
            model: "mock-coder-1b",
            choices: debug ? [] : [{ index: 0, text, finish_reason: finish }],
            ...(debug
              ? { sovereign_debug: { model_id: "mock-coder-1b", stop_rule: "newline", timings_ms: { ttft: 12, total: 34 } } }
              : {}),
          })}\n\n`;

        if (state.mode === "slow") {
          // Drip one chunk, then hang until the client aborts.
          res.write(chunk("x"));
          req.socket.on("close", () => {
            state.aborted = true;
          });
          return; // never ends on its own
        }

        res.write(chunk("return "));
        res.write(chunk("a + b;"));
        res.write(chunk("", null, true));
        res.write(chunk("", "stop"));
        res.write("data: [DONE]\n\n");
        res.end();
      });
      return;
    }
    res.writeHead(404);
    res.end();
  });

  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      resolve({
        state,
        port: server.address().port,
        endpoint: `http://127.0.0.1:${server.address().port}`,
        close: () => new Promise((r) => server.close(r)),
      });
    });
  });
}
