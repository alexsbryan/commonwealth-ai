# sovereign-desktop — contributor conventions

Conventions enforced for the Svelte 5 + Tauri 2 desktop frontend at
`crates/sovereign-desktop/src/`. Pair with
[`../../ARCH_PRINCIPLES.md`](../../ARCH_PRINCIPLES.md) — these are
the frontend-specific addenda.

---

## State management — three tiers, smallest fit wins

Pick the smallest tier that fits.

### Tier 1 — Component-local `$state`

Use when state lives inside one component and doesn't outlive its
mount. Form fields, toggles, hover states, in-flight form
submissions.

```svelte
<script lang="ts">
  let name = $state("");
  let saving = $state(false);
</script>
```

No discipline beyond: reassign with `=`, don't mutate nested
objects in place (see the immutability rule below).

### Tier 2 — Runed module (`.svelte.ts`)

Use when the same state is shared across multiple components in the
same feature but doesn't have distinct *named states*. Examples:

- `src/lib/stores/insights.svelte.ts` — list of clipped insights
- `src/lib/stores/sinks.svelte.ts` — sink connection status

Pattern: private `let _foo = $state(...)` at module scope, a single
exported object with getters and mutator methods. All mutators go
through `produce()`.

```ts
import { produce } from "immer";
let _items: Item[] = $state([]);

export const itemStore = {
  get items() { return _items; },
  add(item: Item) {
    _items = produce(_items, (draft) => { draft.unshift(item); });
  },
};
```

Test with Vitest via mocked API layer — see
`src/lib/stores/insights.test.ts` for the canonical pattern.

### Tier 3 — XState machine (`machines/*.machine.ts`)

Use when the feature has **named states** with **guarded
transitions** — not just "loading / ready / error" but things like:

- Chat streaming lifecycle (idle → sending → streaming → complete →
  refining → complete)
- Setup wizard flow (hardware check → model → embed → tier →
  bootstrap)
- Corpus install phases (downloading → extracting → … → indexing
  → done)
- Cross-cutting request/response choreography (approval,
  info-request)

Heuristic: if you find yourself writing `if (state === 'a' &&
event === 'b' && !isLoading)`, you want a machine. Toggling a
boolean doesn't.

Consume via `@xstate/svelte`:

```svelte
<script lang="ts">
  import { useMachine } from "@xstate/svelte";
  import { skillsMachine } from "../machines/skills.machine";
  const { snapshot, send } = useMachine(skillsMachine);
</script>

{#if $snapshot.matches("loading")}
  <p>Loading skills…</p>
{:else if $snapshot.matches("error")}
  <p>Error. <button onclick={() => send({ type: "RETRY" })}>Retry</button></p>
{:else}
  <ul>{#each $snapshot.context.skills as s}<li>{s.name}</li>{/each}</ul>
{/if}
```

Test machines as pure functions — no DOM, no Svelte, just actor
I/O. See `src/lib/machines/skills.machine.test.ts`.

---

## The immutability rule (load-bearing)

**Never mutate a nested `$state` proxy in place.** Every state
write must produce a new top-level reference. Use `immer`'s
`produce()`:

```ts
// ✗ Wrong — nested mutation, then outer reassignment:
messages[idx].metadata = p.metadata;
messages = [...messages];
```

Looks correct but isn't. Consumers can hold `$derived` closures
over the nested `metadata` reference; the outer array reassignment
rerenders the `{#each}` block but the
`$derived(metadata?.provenance)` inside the item component doesn't
re-run because its input reference hasn't changed. Symptom:
provenance only appears after the user navigates to another
conversation and back, which rehydrates messages from disk with
fresh object references.

```ts
// ✓ Right — produce() yields a new top-level array, a new message
// object at `idx`, and a new metadata object inside it:
import { produce } from "immer";
messages = produce(messages, (draft) => {
  if (remaining) draft[idx].content += remaining;
  if (p.metadata) draft[idx].metadata = p.metadata;
});
```

Applies to **all three tiers**: component `$state`, runed module
state, and XState machine context
(`assign(({ context }) => produce(context, draft => { ... }))`).

---

## Tauri events are inputs, not state

`listen("message-complete", ...)` and friends should only ever
push events into a machine or update store state. Don't build
conditional logic in listener callbacks — that's where races live.
The chat-machine refactor moved all chat listeners behind the chat
machine for exactly this reason.

---

## Testing

- **Runes-only components** — component test with
  `@testing-library/svelte` if interaction matters; often not
  worth it.
- **Runed modules** — Vitest + mocked API layer. See
  `insights.test.ts`.
- **XState machines** — Vitest directly on the actor:
  `createActor()`, `send()`, assert `snapshot.value` and
  `snapshot.context`. No DOM. Model-based coverage via
  `@xstate/test` when exhaustiveness matters.

---

## What *not* to reach for

- **Svelte stores (`writable`, `readable`)** — the codebase no
  longer uses them; don't reintroduce. Runed modules cover the
  same use case with better ergonomics.
- **Svelte context API** — unused. If you think you need it, you
  probably want a runed module with an exported singleton.
- **Global event buses** — use Tauri's built-in `listen`/`emit`
  for frontend↔backend; props or store subscriptions for
  frontend↔frontend.

---

## Chat dispatch — invariant

`ChatView` dispatches `SEND_INITIATED` **before** any bridge
await (fixes the 60s blank-window bug). `ensureConversation`
uses `CONVERSATION_BOUND`, not `HYDRATE`, to preserve the
in-flight user bubble.

Test surface: `tests/e2e/specs/chat-chaos.spec.ts` probes the FSM
+ UI with unexpected events; a `pageerror` watcher auto-fails on
uncaught exceptions. Measure latency inside the page via
`performance.now()`, not Playwright side.

---

## E2E patterns

- **Mocked Tauri** — e2e suite at `tests/e2e/` runs against
  `vite dev` with `__TAURI_INTERNALS__` shimmed. Not
  `tauri-driver`.
- **Chaos invariants** — for any new feature with cross-process
  signals, add a chaos spec asserting (a) unknown peer / phantom
  event renders zero ghost rows and (b) no `pageerror` fires.
  See `mesh-health.spec.ts` for the canonical pattern.
