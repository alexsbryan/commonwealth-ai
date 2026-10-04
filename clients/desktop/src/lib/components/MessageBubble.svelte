<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import AssistantMessage from "./AssistantMessage.svelte";
  import type { NextStepOffer, SearchAugmentation } from "../types";

  interface Props {
    role: string;
    content: string;
    messageId: string;
    conversationId: string;
    metadata?: Record<string, unknown>;
    isStreaming?: boolean;
    /** Forwarded to AssistantMessage — see its Props for the
     *  refining / searchAugmentation contract. */
    refining?: boolean;
    searchAugmentation?: SearchAugmentation;
    /** PR3 — callback fired when the user clicks a next-step chip
     *  rendered under the assistant message. ChatView supplies the
     *  orchestrator that routes via `resumeSession` or
     *  `sendMessageStream`. */
    onNextStep?: (offer: NextStepOffer) => void;
    /** Fired when the user clicks "Continue from here" on the cutoff
     *  chip. Forwarded to AssistantMessage; see its Props. */
    onContinue?: () => void;
    /** Navigate to the Library — forwarded to AssistantMessage for the
     *  EpistemicFooter abstention-panel route chips (I2-B). */
    onOpenLibrary?: () => void;
  }

  let {
    role,
    content,
    messageId,
    conversationId,
    metadata,
    isStreaming,
    refining,
    searchAugmentation,
    onNextStep,
    onContinue,
    onOpenLibrary,
  }: Props = $props();
</script>

{#if role === "user"}
  <div class="bubble user">
    <div class="role-label">You</div>
    <div class="content">{content}</div>
  </div>
{:else}
  <AssistantMessage
    {content}
    {messageId}
    {conversationId}
    {metadata}
    {isStreaming}
    {refining}
    {searchAugmentation}
    {onNextStep}
    {onContinue}
    {onOpenLibrary}
  />
{/if}

<style>
  .bubble {
    max-width: 82%;
    margin-bottom: 18px;
    word-wrap: break-word;
    white-space: pre-wrap;
  }

  .user {
    background: var(--user-bubble);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg) var(--radius-lg) var(--radius) var(--radius-lg);
    padding: 12px 16px;
    align-self: flex-end;
    margin-left: auto;
  }

  .user .role-label {
    text-align: right;
    color: var(--text-muted);
    font-size: 0.7rem;
    font-weight: 500;
    letter-spacing: 0.05em;
    margin-bottom: 5px;
    text-transform: uppercase;
  }

  .content {
    line-height: 1.65;
    color: var(--text-primary);
  }
</style>
