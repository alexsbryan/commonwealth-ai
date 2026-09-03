// SPDX-License-Identifier: AGPL-3.0-or-later
// covers: UI-10
//
// The two accessibility seams this package shares across surfaces, and the
// only tests in it — `packages/chat-ui/src` had no test file at all until
// now, which is why the desktop's vitest `include` reaches in here.
//
// Both behaviours are DYNAMIC: an automated scanner walking the rendered DOM
// sees a `role="status"` region and a dialog with tabbable children and
// reports both as fine. What it cannot see is *when* the region's text
// changes, or where focus goes when the dialog closes — and those are the two
// things that make the difference between usable and unusable with a screen
// reader. So they are asserted here by driving the seams, not by scanning.
import { describe, it, expect, vi } from "vitest";
import { completionAnnouncement, completionAnnouncer } from "./announce";
import { dialogFocus } from "./actions/dialog-focus";

describe("completionAnnouncement — the wording", () => {
  it("says something different when the turn errored", () => {
    // A renderer that collapsed these would tell a screen-reader user the
    // assistant "finished responding" on a turn that failed — the one case
    // where a sighted user sees a red error bubble and they hear nothing.
    expect(completionAnnouncement()).not.toBe(
      completionAnnouncement({ errored: true }),
    );
    expect(completionAnnouncement()).toMatch(/finished/i);
    expect(completionAnnouncement({ errored: true })).toMatch(/error/i);
  });
});

describe("completionAnnouncer — once per turn, never per token", () => {
  it("announces on the streaming to idle edge and on no other tick", () => {
    const emit = vi.fn();
    const a = completionAnnouncer(emit);

    // Idle before anything happens: nothing to announce.
    expect(a.observe(false)).toBe(false);

    // A turn streams for many ticks. Each tick is a MESSAGE_CHUNK arriving
    // and the component re-rendering. THIS is the failure the whole design
    // exists to prevent: an `aria-live` bound to the growing prose would
    // re-announce the entire answer here, eight times.
    for (let chunk = 0; chunk < 8; chunk++) {
      expect(a.observe(true)).toBe(false);
    }
    expect(emit).not.toHaveBeenCalled();

    // MESSAGE_COMPLETE — the falling edge. Exactly one announcement.
    expect(a.observe(false)).toBe(true);
    expect(emit).toHaveBeenCalledTimes(1);
    expect(emit).toHaveBeenCalledWith("svrnmesh finished responding.");

    // Idle keeps re-rendering (a hover, a store update). Still one.
    a.observe(false);
    a.observe(false);
    expect(emit).toHaveBeenCalledTimes(1);
  });

  it("announces again on the next turn, and words an errored turn as an error", () => {
    const emit = vi.fn();
    const a = completionAnnouncer(emit);

    a.observe(true);
    a.observe(false);
    a.observe(true);
    a.observe(true);
    expect(a.observe(false, { errored: true })).toBe(true);

    // Two turns, two announcements — an announcer that latched after the
    // first would pass every assertion in the test above.
    expect(emit).toHaveBeenCalledTimes(2);
    expect(emit.mock.calls[0][0]).toBe(completionAnnouncement());
    expect(emit.mock.calls[1][0]).toBe(completionAnnouncement({ errored: true }));
  });

  it("gives each surface its own edge — two announcers do not share state", () => {
    const a = vi.fn();
    const b = vi.fn();
    const one = completionAnnouncer(a);
    const two = completionAnnouncer(b);
    one.observe(true);
    one.observe(false);
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).not.toHaveBeenCalled();
  });
});

describe("dialogFocus — the trap and the restore", () => {
  function buildDialog() {
    const opener = document.createElement("button");
    opener.textContent = "Open";
    document.body.appendChild(opener);
    opener.focus();

    const dialog = document.createElement("div");
    dialog.tabIndex = -1;
    const first = document.createElement("button");
    first.textContent = "First";
    const middle = document.createElement("input");
    const last = document.createElement("button");
    last.textContent = "Last";
    dialog.append(first, middle, last);
    document.body.appendChild(dialog);
    // jsdom reports offsetParent as null for everything (no layout), and the
    // action's visibility filter keeps the active element regardless — so the
    // tabbable list is exercised through `document.activeElement` here rather
    // than through a layout jsdom does not have.
    for (const el of [first, middle, last]) {
      Object.defineProperty(el, "offsetParent", {
        get: () => document.body,
        configurable: true,
      });
    }
    return { opener, dialog, first, middle, last };
  }

  function tab(node: HTMLElement, shift = false) {
    const ev = new KeyboardEvent("keydown", {
      key: "Tab",
      shiftKey: shift,
      bubbles: true,
      cancelable: true,
    });
    node.dispatchEvent(ev);
    return ev;
  }

  it("cycles Tab inside the dialog instead of letting focus escape", async () => {
    const { dialog, first, last } = buildDialog();
    const handle = dialogFocus(dialog, undefined)!;
    // The action defers its initial focus a microtask so {#if} content can
    // render first.
    await Promise.resolve();
    expect(document.activeElement).toBe(first);

    // Tab from the LAST control wraps to the first rather than leaving the
    // dialog for the page behind the scrim.
    last.focus();
    const forward = tab(dialog);
    expect(forward.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(first);

    // And Shift-Tab from the first wraps to the last.
    const back = tab(dialog, true);
    expect(back.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(last);

    handle.destroy?.();
  });

  it("restores focus to the element that opened it", async () => {
    const { opener, dialog, first } = buildDialog();
    expect(document.activeElement).toBe(opener);

    const handle = dialogFocus(dialog, undefined)!;
    await Promise.resolve();
    expect(document.activeElement).toBe(first);

    // Closing the dialog. Without the restore, a keyboard or screen-reader
    // user is dumped at the top of the document and loses their place — the
    // gap every hand-rolled dialog had before this action existed.
    handle.destroy?.();
    expect(document.activeElement).toBe(opener);
  });

  it("calls onEscape and stops the event rather than letting a parent also close", async () => {
    const { dialog } = buildDialog();
    const onEscape = vi.fn();
    const parentSaw = vi.fn();
    document.body.addEventListener("keydown", parentSaw);

    const handle = dialogFocus(dialog, { onEscape })!;
    await Promise.resolve();
    dialog.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
    expect(onEscape).toHaveBeenCalledTimes(1);
    expect(parentSaw).not.toHaveBeenCalled();

    document.body.removeEventListener("keydown", parentSaw);
    handle.destroy?.();
  });
});
