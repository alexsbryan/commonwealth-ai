<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { useMachine } from "@xstate/svelte";
  import { fromPromise } from "xstate";
  import { listSkills, toggleSkill } from "../api";
  import { skillsMachine } from "../machines/skills.machine";

  // Provide the real Tauri-backed actor implementations to the machine
  // here, at the component boundary. The machine itself (and its tests)
  // never see `invoke()` — the side effects are pluggable. Any future
  // changes to the API client (retries, batching) happen here, not in
  // the state machine.
  const machine = skillsMachine.provide({
    actors: {
      fetchSkills: fromPromise(() => listSkills()),
      toggleSkill: fromPromise(
        ({ input }: { input: { id: string; active: boolean } }) =>
          toggleSkill(input.id, input.active),
      ),
    },
  });

  const { snapshot, send } = useMachine(machine);

  // `backend-ready` is Tauri's signal that the Rust runtime has finished
  // bootstrap. Forwarded into the machine as BOOTSTRAP_COMPLETE so the
  // `waitingForBackend` state can fast-path into `loading` without
  // having to wait for the 2s polling fallback.
  //
  // Tauri events aren't replayed — if the event fires before this
  // listener attaches (e.g. the user opens Settings long after the app
  // has warmed up), we miss it. That's fine: the machine's polling
  // fallback covers that case. The listener is pure optimization.
  let unlistenBackendReady: UnlistenFn | null = null;

  onMount(async () => {
    unlistenBackendReady = await listen("backend-ready", () => {
      send({ type: "BOOTSTRAP_COMPLETE" });
    });
  });

  onDestroy(() => {
    unlistenBackendReady?.();
  });
</script>

<div class="skill-manager">
  <h3>Skills</h3>
  {#if $snapshot.matches("loading") || $snapshot.matches("waitingForBackend")}
    <p class="empty">Loading skills…</p>
  {:else if $snapshot.matches("error")}
    <p class="empty">
      Could not load skills: {$snapshot.context.errorMessage}
      <button
        class="retry"
        onclick={() => send({ type: "RETRY" })}
      >Retry</button>
    </p>
  {:else if $snapshot.context.skills.length === 0}
    <p class="empty">No skills found. Place skill directories in your skills folder.</p>
  {:else}
    {#each $snapshot.context.skills as skill (skill.id)}
      <div class="skill-item">
        <div class="skill-info">
          <span class="skill-name">
            {skill.name}
            {#if skill.trust_level === "communityreviewed"}
              <span class="trust-badge community">Verified</span>
            {:else if skill.trust_level === "authorsigned"}
              <span class="trust-badge signed">Signed</span>
            {:else}
              <span class="trust-badge unsigned">Unsigned</span>
            {/if}
          </span>
          <span class="skill-desc">{skill.description}</span>
        </div>
        <label class="toggle">
          <input
            type="checkbox"
            checked={skill.active}
            onchange={() =>
              send({
                type: "TOGGLE_SKILL",
                id: skill.id,
                active: !skill.active,
              })}
            disabled={$snapshot.matches("toggling") &&
              $snapshot.context.togglingId === skill.id}
          />
          <span class="slider"></span>
        </label>
      </div>
    {/each}
  {/if}
</div>

<style>
  .skill-manager h3 {
    font-size: 0.9rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    margin-bottom: 12px;
  }

  .empty {
    color: var(--text-muted);
    font-size: 0.9rem;
  }

  .retry {
    margin-left: 8px;
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-secondary);
    padding: 2px 10px;
    border-radius: 4px;
    cursor: pointer;
    font-size: 0.8rem;
  }
  .retry:hover {
    border-color: var(--text-muted);
    color: var(--text-primary, var(--text-secondary));
  }

  .skill-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 0;
    border-bottom: 1px solid var(--border);
  }

  .skill-item:last-child {
    border-bottom: none;
  }

  .skill-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .skill-name {
    font-weight: 500;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .trust-badge {
    font-size: 0.65rem;
    padding: 1px 6px;
    border-radius: 8px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.3px;
  }
  .trust-badge.community {
    background: rgba(34, 197, 94, 0.15);
    color: var(--success, #22c55e);
  }
  .trust-badge.signed {
    background: var(--sky-dim);
    color: var(--sky);
  }
  .trust-badge.unsigned {
    background: rgba(156, 163, 175, 0.15);
    color: var(--text-muted);
  }

  .skill-desc {
    font-size: 0.8rem;
    color: var(--text-muted);
  }

  .toggle {
    position: relative;
    display: inline-block;
    width: 40px;
    height: 22px;
    flex-shrink: 0;
    cursor: pointer;
  }

  .toggle input {
    opacity: 0;
    width: 0;
    height: 0;
  }

  .slider {
    position: absolute;
    inset: 0;
    background: var(--border);
    border-radius: 22px;
    transition: background 0.2s;
  }

  .slider::before {
    content: "";
    position: absolute;
    width: 16px;
    height: 16px;
    left: 3px;
    bottom: 3px;
    background: var(--text-secondary);
    border-radius: 50%;
    transition: transform 0.2s;
  }

  .toggle input:checked + .slider {
    background: var(--success);
  }

  .toggle input:checked + .slider::before {
    transform: translateX(18px);
    background: var(--text-on-accent);
  }
</style>
