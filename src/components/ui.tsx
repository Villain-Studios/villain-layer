import type { ReactNode, RefObject } from "react";
import { useEffect, useRef, useState } from "react";

import { useStore } from "../store";

export function Modal({
  title, children, footer, toolbar, onClose, wide, tall,
}: {
  title: string;
  children: ReactNode;
  footer?: ReactNode;
  /** Pinned under the header, outside the scrolling body. */
  toolbar?: ReactNode;
  onClose: () => void;
  wide?: boolean;
  /** Fixed height, so switching sections does not resize the window. */
  tall?: boolean;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="overlay" onMouseDown={onClose}>
      <div
        className={`modal${tall ? " tall" : ""}`}
        style={wide ? { maxWidth: 720 } : undefined}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="modal-head">
          {title}
          <div className="spacer" />
          <button className="btn-sm" onClick={onClose}>✕</button>
        </div>
        {toolbar && <div className="modal-toolbar">{toolbar}</div>}
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-foot">{footer}</div>}
      </div>
    </div>
  );
}

export function Field({
  label, hint, children,
}: {
  label: string;
  hint?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="field">
      <label>{label}</label>
      {children}
      {hint && <div className="hint">{hint}</div>}
    </div>
  );
}

export function Spinner() {
  return <div className="spin" />;
}

/** A gear, drawn rather than typed: the ⚙ glyph renders small and varies by font. */
export function GearIcon({ size = 18 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ display: "block" }}
      aria-hidden="true"
    >
      <circle cx="12" cy="12" r="3.2" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.6a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9v.09a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

/**
 * Show or hide the task list.
 *
 * It lives at the left edge of the main panel rather than in the title bar:
 * that is where the sidebar's own edge is, so the control sits against the
 * thing it moves, and it stays in the same place whether or not the list is
 * showing.
 */
export function SidebarToggle() {
  const hidden = useStore((s) => s.sidebarHidden);
  const toggle = useStore((s) => s.toggleSidebar);
  return (
    <button
      className="icon-btn flat"
      title={hidden ? "Show tasks" : "Hide tasks"}
      onClick={toggle}
    >
      <svg
        width="17"
        height="17"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinejoin="round"
        style={{ display: "block" }}
        aria-hidden="true"
      >
        <rect x="3" y="4" width="18" height="16" rx="2.5" />
        <line x1="9.5" y1="4" x2="9.5" y2="20" />
        {!hidden && (
          <rect x="3" y="4" width="6.5" height="16" fill="currentColor" opacity="0.35" stroke="none" />
        )}
      </svg>
    </button>
  );
}

/** A labelled toggle row, for settings that read better as switches. */
export function Switch({
  label, detail, checked, disabled, onChange,
}: {
  label: string;
  detail?: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void;
}) {
  return (
    <label className={`switch-row${disabled ? " disabled" : ""}`}>
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="switch-text">
        <span className="switch-label">{label}</span>
        {detail && <span className="switch-detail">{detail}</span>}
      </span>
    </label>
  );
}

export interface MenuItem {
  label: string;
  onSelect: () => void;
  danger?: boolean;
  disabled?: boolean;
  /** Renders a divider above this item. */
  separated?: boolean;
}

/**
 * A right-click menu anchored at the cursor.
 *
 * Shifts itself back on screen near an edge, since the sidebar is close to the
 * left and tasks near the bottom would otherwise open a menu below the window.
 */
export function ContextMenu({
  x, y, items, onClose, ignore,
}: {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
  /**
   * An element whose clicks must not dismiss the menu, so the button that
   * opened it can close it again instead of closing and reopening.
   */
  ignore?: RefObject<HTMLElement | null>;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    // The app chrome is scaled with CSS `zoom`, which scales fixed positioning
    // too: a rect comes back in viewport pixels while `left`/`top` are set in
    // the element's own units. Rather than assume the factor — engines differ
    // on what they report — measure it: move the menu a known distance and see
    // how far it actually went.
    el.style.left = "0px";
    el.style.top = "0px";
    const origin = el.getBoundingClientRect();
    el.style.left = "100px";
    el.style.top = "100px";
    const moved = el.getBoundingClientRect();
    const sx = (moved.left - origin.left) / 100 || 1;
    const sy = (moved.top - origin.top) / 100 || 1;

    const left = Math.max(8, Math.min(x, window.innerWidth - origin.width - 8));
    const top = Math.max(8, Math.min(y, window.innerHeight - origin.height - 8));
    setPos({ left: (left - origin.left) / sx, top: (top - origin.top) / sy });
    // Items can arrive after the menu opens — resumable agents are fetched —
    // and a taller menu needs placing again or it runs off the bottom.
  }, [x, y, items.length]);

  useEffect(() => {
    const close = (e: MouseEvent) => {
      // A press inside the menu must not dismiss it: closing on mousedown
      // detaches the item before the browser can deliver its click, and the
      // item then does nothing at all.
      if (ref.current?.contains(e.target as Node)) return;
      if (ignore?.current?.contains(e.target as Node)) return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    // Capture so a click anywhere dismisses before it does anything else.
    window.addEventListener("mousedown", close, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose, ignore]);

  return (
    <div
      ref={ref}
      className="ctx-menu"
      // Hidden until measured, so it never flashes at the unadjusted spot.
      style={{ ...(pos ?? {}), visibility: pos ? "visible" : "hidden" }}
      onMouseDown={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((item, i) => (
        <button
          key={i}
          className={`ctx-item${item.danger ? " danger" : ""}${
            item.separated ? " separated" : ""
          }`}
          disabled={item.disabled}
          onClick={() => { onClose(); item.onSelect(); }}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}

/**
 * An in-app confirmation.
 *
 * `window.confirm` cannot be used: the webview returns true without ever
 * drawing a dialog, so every guard written that way silently approves itself.
 */
export function Confirm({
  title, body, confirmLabel = "Delete", danger = true, onConfirm, onCancel,
}: {
  title: string;
  body: ReactNode;
  confirmLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <Modal
      title={title}
      onClose={onCancel}
      footer={
        <>
          <button className="btn" onClick={onCancel}>Cancel</button>
          <button
            className={`btn ${danger ? "btn-danger-solid" : "btn-primary"}`}
            autoFocus
            onClick={() => { onCancel(); onConfirm(); }}
          >
            {confirmLabel}
          </button>
        </>
      }
    >
      <div style={{ lineHeight: 1.6 }}>{body}</div>
    </Modal>
  );
}
