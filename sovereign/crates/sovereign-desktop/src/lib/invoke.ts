// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The one place the frontend reaches the Tauri bridge.
//
// Tauri's own `invoke` takes `cmd: string`, so `invoke("send_mesage")`
// compiles, ships, and fails in front of a user as a rejected promise
// from the bridge. A misspelled argument key fails the same way, with
// "invalid args", and the control simply does nothing.
//
// This wrapper types both against `commands.generated.ts`, which is
// rendered from the Rust `#[tauri::command]` surface by
// `src-tauri/tests/command_surface.rs`. A name or key that does not
// exist is now a `svelte-check` error — a gate that already blocks CI —
// instead of a runtime one.
//
// `command_surface.rs::the_invoke_bridge_is_the_only_tauri_entry_point`
// holds this as the sole importer of `@tauri-apps/api/core`, so a new
// file cannot quietly go around it.
//
// NOT typed here: the RESOLVED value. `T` is still the caller's claim,
// exactly as before, because checking it needs every Rust DTO mirrored
// in TypeScript. That is a separate job; this one closes the failure
// that actually reaches users.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";

import type { CommandArgs, CommandName } from "./commands.generated";

export type { CommandArgs, CommandName };

/**
 * Call a backend command.
 *
 * `K` is inferred from `cmd`, so the argument object is checked against
 * that command's own keys. The four call sites that pass `T` explicitly
 * fall back to `K = CommandName`, which still rejects an unknown name.
 */
export async function invoke<T, K extends CommandName = CommandName>(
  cmd: K,
  args?: CommandArgs[K],
): Promise<T> {
  return tauriInvoke<T>(cmd, args as Record<string, unknown> | undefined);
}

/**
 * A Tauri PLUGIN command, addressed as `plugin:<name>|<method>`.
 *
 * These are not `#[tauri::command]` functions in this crate — they come
 * from a plugin — so the generated map cannot know them and their
 * arguments cannot be checked. Kept as a separate, greppable door
 * rather than widening `invoke`: folding them in would loosen the
 * argument type for all 260 of our own commands to buy nothing.
 */
export type PluginCommand = `plugin:${string}|${string}`;

export async function invokePlugin<T>(
  cmd: PluginCommand,
  args?: Record<string, unknown>,
): Promise<T> {
  return tauriInvoke<T>(cmd, args);
}
