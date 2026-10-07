/**
 * The window's own mouse and key events, as the Browser panel sends them on
 * to the task's tab (BRW-9). Coordinates become the page's CSS pixels.
 */
import type { BrowserInput } from "./types";

/** Chrome's modifier bits. */
export function modifiers(e: { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }): number {
  return (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
}

const BUTTONS = ["left", "middle", "right"] as const;

/** Where on the page a point on the shown frame is. */
export function pagePoint(
  e: { clientX: number; clientY: number },
  rect: { left: number; top: number; width: number; height: number },
  page: { width: number; height: number },
): { x: number; y: number } {
  return {
    x: ((e.clientX - rect.left) * page.width) / Math.max(1, rect.width),
    y: ((e.clientY - rect.top) * page.height) / Math.max(1, rect.height),
  };
}

export function mouse(
  type: "mousePressed" | "mouseReleased" | "mouseMoved",
  e: MouseEvent,
  at: { x: number; y: number },
): BrowserInput {
  return {
    kind: "mouse",
    type,
    x: at.x,
    y: at.y,
    button: type === "mouseMoved" ? "none" : BUTTONS[e.button] ?? "left",
    buttons: e.buttons,
    click_count: type === "mouseMoved" ? 0 : Math.max(1, e.detail),
    modifiers: modifiers(e),
  };
}

/** A wheel's travel in pixels, whatever unit the event counted in. */
export function wheel(e: WheelEvent, at: { x: number; y: number }): BrowserInput {
  const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 600 : 1;
  return { kind: "wheel", x: at.x, y: at.y, dx: e.deltaX * unit, dy: e.deltaY * unit, modifiers: modifiers(e) };
}

/** What a key types, if anything: a modified key types nothing. */
export function keyText(e: KeyboardEvent): string | null {
  if (e.metaKey || e.ctrlKey) return null;
  if (e.key === "Enter") return "\r";
  return e.key.length === 1 ? e.key : null;
}

export function key(type: "keyDown" | "keyUp", e: KeyboardEvent): BrowserInput {
  return {
    kind: "key",
    type,
    key: e.key,
    code: e.code,
    key_code: e.keyCode,
    text: type === "keyDown" ? keyText(e) : null,
    modifiers: modifiers(e),
    location: e.location,
  };
}
