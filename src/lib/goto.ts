/**
 * Going where a banner, a toast or a message points (NOTE-4): the one place
 * that turns a target into a view, a task, a tab and a selection.
 */
import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useStore } from "../store";
import { api } from "./api";
import { route } from "./target";
import type { PaneInfo, Target } from "./types";

/**
 * Open what `target` is about. `raise` brings the window forward too, for a
 * banner clicked while the app was behind something else.
 */
export function goTo(target: Target | string, opts: { raise?: boolean } = {}) {
  const s = useStore.getState();
  const step = route(target, { tasks: s.tasks.map((t) => t.id), panes: s.panes });
  if (step) {
    // Settings is a sheet over every view: left open, it hides where this went.
    if (s.settingsOpen) s.toggleSettings(false);
    switch (step.view) {
      case "work":
        s.select(step.task);
        // After select, which always lands on the terminals.
        if (step.tab) s.setTab(step.tab);
        if (step.pane) s.showPane(step.pane);
        break;
      case "chat":
        s.setView("chat");
        if (step.pane) s.showPane(step.pane);
        break;
      case "reviews":
        if (step.review) s.showReview(step.review);
        else s.setView("reviews");
        break;
      case "tickets":
        if (step.issue) s.showIssue(step.issue);
        else s.setView("tickets");
        break;
    }
  }
  // Whatever else was said about the same thing has been seen now too.
  const seen = s.messages.filter((m) => !m.read && m.target === target).map((m) => m.id);
  if (seen.length) void api.markMessagesRead(seen).catch(() => {});
  if (opts.raise) {
    const win = getCurrentWindow();
    void win.unminimize().catch(() => {});
    void win.show().catch(() => {});
    void win.setFocus().catch(() => {});
  }
}

/**
 * Select the pane a click asked for, once it is among `panes`. Declared after
 * a view's own "keep a sensible pane selected" effect, so on mount this one's
 * choice is the one that stands.
 */
export function useFocusedPane(panes: PaneInfo[], select: (id: string) => void) {
  const focus = useStore((s) => s.focusPane);
  const clear = useStore((s) => s.clearFocusPane);
  useEffect(() => {
    if (!focus) return;
    if (panes.some((p) => p.id === focus)) {
      select(focus);
      clear();
    } else if (!useStore.getState().panes.some((p) => p.id === focus)) {
      // Gone: nothing will ever claim it.
      clear();
    }
  }, [focus, panes, select, clear]);
}
