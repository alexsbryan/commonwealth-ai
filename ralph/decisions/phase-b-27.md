<!-- ledger -->

**phase-b-27 · 2026-09-27 · pb-svrn-dials-serve → top_k and the turn's admission cross the chat wire as extension fields · operator** — this commit
- Needed: the golden round-trip test (5503cbbad) holds two fields as known losses with no field on the chat wire: `top_k` and `admission`. After the switch every model call of an admitted turn reaches serve's slot queue as fresh load, and the queue sheds fresh load past `max_wait_ms` (sovereign-inference model_slot.rs:1356). That is the failure note d6e13797 measured and runtime/admission.rs was built to end: 5 of 5 `judge_failed_open` exits were `queue_shed`, and 5 more turns died at the draft with "host busy". `top_k` drops only when `[inference] top_k` is configured (default None), and serve then samples with the model family's default.
- Chose (operator, from the seat's escalation): carry both as extension fields on oicp-types' `ChatCompletionRequest`, beside `sampling_mode`, `lark_grammar` and `stable_prefix_len`, which it already carries. The golden test's known-loss set becomes empty.
- Because:
  - Principle 6: an admitted continuation reaching the queue as fresh load substitutes one request class for another, and a user sees it as a turn that fails under load.
  - Principle 12: the trust decision belongs to the listener that parses the wire. After the switch a peer's request reaches serve through the daemon's peer listener and then over loopback, so "honour it on serve's loopback" alone would let a mesh peer claim a continuation.
  - Refused: carrying admission only (it leaves a configured knob silently ignored for one field's cost), and carrying neither (it regresses a fixed, measured failure).
  - Boundary gate: 49 at 5503cbbad, as its body reports. No code is in this commit.

<!-- appendix -->

## phase-b-27 · 2026-09-27 — an admitted turn stays admitted across the dial to serve

<details><summary>reasoning, evidence, package</summary>

Escalated by the seat before the worker's halt, from the known-loss set in sovereign-serve tests/chat_round_trip.rs (`NO_CHAT_WIRE_FIELD = ["admission", "top_k"]`), whose comment named the halt. phase-b-26 pre-registered this case: "A field cannot be carried on the OpenAI chat wire. Then it needs an extension field on serve's loopback wire, which is a wire change the operator sees before it lands."

Where each field is read today: `top_k` by the sampler (embedded/sampler.rs:473, request, then mode, then family default, then 40), set from `inference_config.top_k` by nine handlers in sovereign-core; `admission` by every slot acquire in embedded/engine.rs (:3084 to :3776), which decides shed versus park (model_slot.rs:1356) and the park's ceiling (:1439). `TurnAdmission` is oicp-types completion.rs:336, an `Arc<str>` id, `#[serde(skip)]` today.

Constraints the row carries:
- The admission id is read only from a request whose connection is loopback at the listener that parses it. A non-loopback caller's value is dropped with a debug event, so a peer's request forwarded through the daemon's peer listener reaches serve as fresh load, as it does today.
- A third-party remote engine (sovereign-inference engine_factory.rs:368, `RemoteApiProvider` with an operator's endpoint and key) never receives the admission field. It rides every admitted turn, and a strict OpenAI-compatible API may refuse an unknown field.
- The in-flight release re-measure is unaffected: the bar's request sets neither field (chat_round_trip.rs, the latency-bar case compares with no known losses).

What would falsify this: an admitted turn on the dialing path is still shed under contention. Then the id crosses and serve's queue does not honour it, and the shed trace names which.

</details>
