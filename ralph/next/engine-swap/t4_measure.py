#!/usr/bin/env python3
"""es-t4's measurement: a Codex-shaped `/v1/responses` turn, counted.

One turn here is the request `routes_responses::translate_request` builds for
a Codex `/v1/responses` call: a `required` choice over the frontdoor's tools,
a zero think budget with thinking off, the 16K floor and Codex's 0.7
temperature. It is sent to a running daemon's `/v1/chat/completions`, which is
where the translated turn goes.

Tokens of the answer: of N turns, how many answered with a call to an offered
tool (`calls_made`), how many answered in prose instead, and how many turns
the daemon failed outright (`misses_failed`). A `required` reply that misses
is asked once more inside the daemon (es-t1's call check,
`oicp_client::call_check`), so a turn that ends in prose is one the check
recovered from; the daemon logs every such recovery at warn.

Usage: t4_measure.py <port> <label>
"""
import json
import sys
import urllib.error
import urllib.request

TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "exec_command",
            "description": "Run a shell command and return its output.",
            "parameters": {
                "type": "object",
                "properties": {"cmd": {"type": "string"}},
                "required": ["cmd"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "Read a file from the repository.",
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "write_file",
            "description": "Replace a file's contents.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                },
                "required": ["path", "content"],
            },
        },
    },
]

PROMPTS = [
    "List the files in the current directory.",
    "Show me the contents of Cargo.toml.",
    "Which Rust toolchain does this repository pin?",
    "How many crates are in the Cargo workspace?",
    "Find the tests for the daemon frontdoor.",
    "What does the README say this repository is?",
    "Show the last three commits.",
    "Where is the engine kind configured?",
]

OFFERED = {t["function"]["name"] for t in TOOLS}


MODEL = ""


def turn(prompt, budget=True):
    body = {
        # A client's own model name resolves nowhere on the router; the
        # measurement names the model the router actually serves.
        "model": MODEL,
        "messages": [
            {"role": "system", "content": "You are a coding agent."},
            {"role": "user", "content": prompt},
        ],
        "tools": TOOLS,
        "tool_choice": "required",
        "think_budget": 0,
        "chat_template_kwargs": {"enable_thinking": False},
        "max_tokens": 16384,
        "temperature": 0.7,
        "stream": False,
    }
    if not budget:
        # The pre-es-t4 shape: no think budget, so the flattened turn is the
        # only difference left beside the grammar.
        body.pop("think_budget")
        body.pop("chat_template_kwargs")
    return body


def called_names(reply):
    """Every tool name a reply called, as production reads it: native
    `tool_calls`, or the `<tool_call>` envelope the adapter re-emits
    (`response_wire::tool_call_envelope`). A bare JSON envelope is not
    counted, because the adapter's own parser does not read it either
    (`inference_adapter.rs`: `parse_tool_envelope_direct` needs the grammar
    that produced it)."""
    names = []
    for choice in reply.get("choices") or []:
        message = choice.get("message") or {}
        for call in message.get("tool_calls") or []:
            names.append((call.get("function") or {}).get("name"))
        rest = message.get("content") or ""
        while "<tool_call>" in rest:
            _, _, rest = rest.partition("<tool_call>")
            body, _, rest = rest.partition("</tool_call>")
            try:
                names.append(json.loads(body).get("name"))
            except json.JSONDecodeError:
                names.append(None)
    return [n for n in names if n]


def post(port, body):
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=900) as response:
            return json.loads(response.read()), None
    except urllib.error.HTTPError as error:
        return None, f"HTTP {error.code}: {error.read().decode()[:300]}"
    except Exception as error:  # noqa: BLE001 - the miss is the datum
        return None, repr(error)


def main():
    global MODEL
    port, label, MODEL = int(sys.argv[1]), sys.argv[2], sys.argv[3]
    budget = "--no-budget" not in sys.argv[4:]
    made, prose, failed = 0, 0, 0
    for prompt in PROMPTS:
        reply, error = post(port, turn(prompt, budget))
        if error is not None:
            failed += 1
            print(f"{label}\t{prompt!r}\tfailed\t{error}")
            continue
        names = called_names(reply)
        finish = (reply.get("choices") or [{}])[0].get("finish_reason")
        hit = sorted(set(names) & OFFERED)
        if hit:
            made += 1
            print(f"{label}\t{prompt!r}\tcalled {hit} (of {len(names)})\tfinish={finish}")
        else:
            prose += 1
            print(f"{label}\t{prompt!r}\tno call, finish={finish}\tcalled {names}")
    total = len(PROMPTS)
    print(
        json.dumps(
            {
                "label": label,
                "turns": total,
                "calls_made": made,
                "misses_recovered_in_prose": prose,
                "misses_failed": failed,
                "retry_rate": round((prose + failed) / total, 3),
            }
        )
    )


if __name__ == "__main__":
    main()
