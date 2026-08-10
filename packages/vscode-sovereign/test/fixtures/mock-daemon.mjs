// Mock daemon for client tests (extension plan §tests). ~80 lines of
// node:http serving the canned behaviours the client must survive:
// happy-path SSE, slow-drip SSE (abort must actually close the
// socket), HTTP 503 with the actionable body, and /status probes.

import http from "node:http";

// The editing-slot arrangements /status can report. One object per
// arrangement so a test names the SHAPE it is exercising rather than
// hand-assembling JSON at the call site.
//
//   full          coder model: both lanes served (fim_style present)
//   nextEditOnly  ordinary chat model: next-edit yes, FIM no — supported
//   degraded      next-edit off the resident chat model, nobody picked it
//
// `advice` is the daemon's own wording, copied verbatim from
// sovereign-mesh::fim_adapter::edit_slot_advice — a client that
// paraphrases it is the bug this field exists to prevent.
const EDIT_SLOTS = {
  full: {
    slot: "edit",
    model_id: "mock-coder-1b",
    aliased_to_fast: false,
    degraded: false,
    next_edit_format: "region_instruct",
    fim_style: "qwen_coder",
  },
  nextEditOnly: {
    slot: "edit",
    model_id: "mock-chat-8b",
    aliased_to_fast: false,
    degraded: false,
    next_edit_format: "region_instruct",
    advice:
      "This editing model serves next-edit but not fill-in-the-middle: its " +
      "tokenizer carries no FIM markers, so /v1/completions returns 503. " +
      "Point [models.edit].path at a coder GGUF (Mellum2, Qwen2.5-Coder) " +
      "if you need inline completion.",
  },
  degraded: {
    slot: "fast",
    model_id: "mock-chat-8b",
    aliased_to_fast: true,
    degraded: true,
    next_edit_format: "region_instruct",
    advice:
      "Next-edit is being served by the resident chat model because no " +
      "[models.edit] is configured. Suggestions work. A dedicated edit " +
      "model (~1.5 GB) returns them roughly 3x faster and adds " +
      "/v1/completions: set [models.edit].path in ~/.sovereign/config.toml.",
  },
};

/** The `inference` object /status returns for a given mock mode.
 *
 *  A current daemon publishes the SAME object under `edit` and under the
 *  deprecated `fim` mirror, so a client reading either key sees one
 *  arrangement and the two can never disagree. `legacy` reproduces a
 *  pre-split daemon — `fim` only, and no `degraded`/`advice` fields —
 *  which is what the client's fallback has to survive. */
function inferenceStatus(mode) {
  if (mode === "noEdit") return { edit: null, fim: null };
  if (mode === "legacy") {
    return {
      fim: {
        slot: "fim",
        model_id: "mock-coder-1b",
        fim_style: "qwen_coder",
        aliased_to_fast: false,
        next_edit_format: "region_instruct",
      },
    };
  }
  const slot =
    mode === "nextEditOnly"
      ? EDIT_SLOTS.nextEditOnly
      : mode === "degraded"
        ? EDIT_SLOTS.degraded
        : EDIT_SLOTS.full;
  return { edit: slot, fim: slot };
}

export function startMockDaemon() {
  const state = {
    // happy | slow | error503 | error400 | noEdit | nextEditOnly |
    // degraded | legacy   ("legacy" = a daemon from before the two-lane
    // split: only the inference.fim key, always with a fim_style)
    mode: "happy",
    lastRequestBody: null,
    aborted: false,
    lastEditPredictionBody: null,
    // Outcome reports received on /v1/edit_predictions/outcome.
    outcomes: [],
    // When set, the outcome route answers this status instead of 204 —
    // `404` stands in for a daemon predating the route.
    outcomeStatus: null,
    // Canned /v1/edit_predictions response; tests overwrite as needed.
    editPrediction: {
      object: "edit_prediction",
      engine: "rule",
      episode_id: "ep-canned-1",
      edits: [{ start: 5, end: 17, new_text: "console.debug(" }],
      sovereign_debug: {
        rule_find: "console.log(",
        rule_replace: "console.debug(",
        rule_key: '["console.log(","console.debug("]',
        support: 2,
        sites: 1,
        edits_capped: false,
        reason_silent: null,
        timings_ms: { total: 1 },
      },
    },
  };

  const server = http.createServer((req, res) => {
    if (req.url === "/status") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ inference: inferenceStatus(state.mode) }));
      return;
    }
    if (req.url === "/v1/edit_predictions/outcome" && req.method === "POST") {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        state.outcomes.push(JSON.parse(body));
        res.writeHead(state.outcomeStatus ?? 204);
        res.end();
      });
      return;
    }
    if (req.url === "/v1/edit_predictions" && req.method === "POST") {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        state.lastEditPredictionBody = JSON.parse(body);
        if (state.mode === "error400") {
          res.writeHead(400, { "content-type": "application/json" });
          res.end(
            JSON.stringify({ error: { message: "`text` is too large; caps the search space" } }),
          );
          return;
        }
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify(state.editPrediction));
      });
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
              error: { message: "FIM is not configured on this daemon. Add [models.edit] …" },
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
