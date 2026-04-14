<script lang="ts">
  // Placeholder used by SetupWizard.test.ts in place of the real
  // persona / knowledge / websearch forms. Renders a stable marker so
  // the wizard-level test can detect when the transition has routed
  // into "a step body" without caring which form is mounted.
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  let _ignored: Record<string, unknown> = $props();
</script>

<div data-testid="stub-step">step body</div>
