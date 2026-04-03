<script lang="ts">
  import { onMount } from "svelte";
  import { listSkills, toggleSkill } from "../api";
  import type { SkillEntry } from "../types";

  let skills: SkillEntry[] = $state([]);
  let toggling: string | null = $state(null);

  onMount(async () => {
    try {
      skills = await listSkills();
    } catch (e) {
      console.error("Failed to load skills:", e);
    }
  });

  async function handleToggle(skill: SkillEntry) {
    if (toggling) return;
    toggling = skill.id;
    try {
      await toggleSkill(skill.id, !skill.active);
      skill.active = !skill.active;
      skills = [...skills]; // Trigger reactivity.
    } catch (e) {
      console.error("Failed to toggle skill:", e);
    }
    toggling = null;
  }
</script>

<div class="skill-manager">
  <h3>Skills</h3>
  {#if skills.length === 0}
    <p class="empty">No skills found. Place skill directories in your skills folder.</p>
  {:else}
    {#each skills as skill (skill.id)}
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
            onchange={() => handleToggle(skill)}
            disabled={toggling === skill.id}
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
    background: rgba(59, 130, 246, 0.15);
    color: #3b82f6;
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
    background: white;
  }
</style>
