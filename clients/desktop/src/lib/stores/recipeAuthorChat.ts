// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Bridge from dashboard cards → the live recipe-author chat.
//
// Cards (validation errors, test issues) are pure presentation and don't own the
// conversation. To offer a conversational-recovery action ("Ask agent to fix")
// they request a turn by TEXT through this singleton; `RecipeChatSurface` — which
// owns the conversation id + transcript rendering — registers a dispatcher on
// mount and runs the request through its normal send flow. So a button click
// appears in the chat exactly like a typed message (user turn + streamed reply),
// keeping the recovery glassbox rather than firing a hidden side-channel call.

class RecipeAuthorChatBridge {
  #handler: ((text: string) => void) | null = null;

  /**
   * RecipeChatSurface registers its dispatcher on mount; the returned fn
   * unregisters it (call from onDestroy). One workspace is open at a time, so
   * last registration wins.
   */
  register(handler: (text: string) => void): () => void {
    this.#handler = handler;
    return () => {
      if (this.#handler === handler) this.#handler = null;
    };
  }

  /** True when a chat surface is mounted to receive turns (cards gate on this). */
  get active(): boolean {
    return this.#handler !== null;
  }

  /**
   * A card asks the agent to handle `text` (e.g. "Fix these errors: …").
   * Returns false if no chat surface is mounted to receive it.
   */
  requestTurn(text: string): boolean {
    if (!this.#handler) return false;
    this.#handler(text);
    return true;
  }
}

/** Process-wide singleton bridging dashboard cards to the recipe-author chat. */
export const recipeAuthorChat = new RecipeAuthorChatBridge();
