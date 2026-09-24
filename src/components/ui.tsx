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
 * Something that floats over the window at a point: a menu, a panel.
 *
 * Shifts itself back on screen near an edge, since the sidebar is close to the
 * left and tasks near the bottom would otherwise open a menu below the window.
 * A press outside it or Escape closes it.
 */
export function Floating({
  x, y, onClose, ignore, measureKey, className, children,
}: {
  x: number;
  y: number;
  onClose: () => void;
  /**
   * An element whose clicks must not dismiss it, so the button that opened
   * it can close it again instead of closing and reopening.
   */
  ignore?: RefObject<HTMLElement | null>;
  /** Changes when the contents may have changed size, to place it again. */
  measureKey?: unknown;
  className: string;
  children: ReactNode;
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
  }, [x, y, measureKey]);

  useEffect(() => {
    const close = (e: MouseEvent) => {
      // A press inside must not dismiss it: closing on mousedown detaches
      // the item before the browser can deliver its click, and the item then
      // does nothing at all.
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
      className={className}
      // Hidden until measured, so it never flashes at the unadjusted spot.
      style={{ ...(pos ?? {}), visibility: pos ? "visible" : "hidden" }}
      onMouseDown={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.preventDefault()}
    >
      {children}
    </div>
  );
}

/** A right-click menu anchored at the cursor. */
export function ContextMenu({
  x, y, items, onClose, ignore,
}: {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
  ignore?: RefObject<HTMLElement | null>;
}) {
  return (
    <Floating
      x={x}
      y={y}
      onClose={onClose}
      ignore={ignore}
      className="ctx-menu"
      // Items can arrive after the menu opens — resumable agents are fetched —
      // and a taller menu needs placing again or it runs off the bottom.
      measureKey={items.length}
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
    </Floating>
  );
}

/**
 * An in-app confirmation.
 *
 * `window.confirm` cannot be used: the webview returns true without ever
 * drawing a dialog, so every guard written that way silently approves itself.
 *
 * If `onConfirm` returns a promise the dialog stays up and spins until it
 * settles, then closes itself. Closing first and acting after is how removing
 * a repository from a task looked like nothing had happened at all for the
 * seconds git spent on the worktree — and left the button there to be pressed
 * again while the first one was still running. An `onConfirm` that returns
 * nothing closes immediately, for callers that put their own progress on
 * screen or open a second dialog of their own.
 */
export function Confirm({
  title, body, confirmLabel = "Delete", busyLabel, danger = true, onConfirm, onCancel,
}: {
  title: string;
  body: ReactNode;
  confirmLabel?: string;
  /** Replaces the confirm label while the work runs. */
  busyLabel?: string;
  danger?: boolean;
  onConfirm: () => void | Promise<unknown>;
  onCancel: () => void;
}) {
  const [running, setRunning] = useState(false);

  function go() {
    const done = onConfirm();
    // Nothing to wait for: the caller has taken over, as the task delete does
    // with its own overlay.
    if (!(done && typeof (done as Promise<unknown>).then === "function")) {
      onCancel();
      return;
    }
    setRunning(true);
    void (done as Promise<unknown>).then(
      () => onCancel(),
      // The caller reports its own failures; this only has to stop spinning,
      // and it has to stop whether or not the dialog is still mounted.
      () => { setRunning(false); },
    );
  }

  return (
    <Modal
      title={title}
      // Not dismissable mid-flight: the work is already away, and closing the
      // dialog would only hide that it is still running.
      onClose={() => { if (!running) onCancel(); }}
      footer={
        <>
          <button className="btn" disabled={running} onClick={onCancel}>Cancel</button>
          <button
            className={`btn ${danger ? "btn-danger-solid" : "btn-primary"}`}
            autoFocus
            disabled={running}
            onClick={go}
          >
            {running ? (
              <span className="btn-busy"><Spinner />{busyLabel ?? "Working…"}</span>
            ) : (
              confirmLabel
            )}
          </button>
        </>
      }
    >
      <div style={{ lineHeight: 1.6 }}>{body}</div>
    </Modal>
  );
}

/**
 * A blocking "this is happening" card, for work that outlives the dialog that
 * started it and must not be interrupted.
 *
 * Deleting a task is the case it was written for: the sidebar row it belongs
 * to is still on screen and spinning, so the wait cannot live in a dialog
 * that has already closed.
 */
export function BusyOverlay({ title, detail }: { title: string; detail?: string }) {
  return (
    <div className="overlay deleting-overlay" aria-busy="true">
      <div className="deleting-card">
        <Spinner />
        <div className="deleting-copy">
          <div>{title}</div>
          {detail && <div className="muted">{detail}</div>}
        </div>
      </div>
    </div>
  );
}

/**
 * One input that filters a list as you type.
 *
 * The app had grown five of these, each a bare `<input list>` with its own
 * `<datalist>`: no way to tell there was anything to pick from until you
 * typed, no keyboard selection, and a native popup that looks like nothing
 * else here. One component instead, so they behave alike and look like the
 * rest of the app.
 *
 * Free text is allowed by default, because most of these name something that
 * may not exist yet — a group nobody has used, a branch pushed since the last
 * fetch. Pass `allowFree={false}` where only a listed value makes sense.
 */
export function Combo({
  value,
  onChange,
  options,
  placeholder,
  title,
  width,
  allowFree = true,
  empty,
}: {
  value: string;
  onChange: (v: string) => void;
  options: readonly string[];
  placeholder?: string;
  title?: string;
  width?: number | string;
  allowFree?: boolean;
  /** Shown in place of the list when nothing matches. */
  empty?: string;
}) {
  const [text, setText] = useState(value);
  const [open, setOpen] = useState(false);
  // Until something is typed the list shows everything: opening it is a
  // request to see the options, not to filter them by what is already there.
  const [typed, setTyped] = useState(false);
  const [cursor, setCursor] = useState(-1);

  /**
   * Follow the value when it changes underneath us — another pane, a refresh.
   *
   * Only on a genuine change: most of these commit through an async save, so
   * for a moment after picking, `value` is still the old one. Re-reading it
   * on every render would snap the box back to what was there before, until
   * the save lands and moves it forward again.
   */
  const seen = useRef(value);
  useEffect(() => {
    if (value === seen.current) return;
    seen.current = value;
    setText(value);
  }, [value]);

  const shown = typed
    ? options.filter((o) => o.toLowerCase().includes(text.trim().toLowerCase()))
    : options;

  /**
   * Set when a key has already decided the value, so the blur that follows
   * does not decide it again. Enter committed the highlighted option and then
   * blurred, and blur — still holding the text as typed — committed that
   * too: arrow to `release/2.1` from "rel", press Enter, and the base was
   * saved as "rel". Escape saved the typed text the same way.
   */
  const decided = useRef(false);

  function commit(v: string) {
    seen.current = v;
    setText(v);
    setOpen(false);
    setTyped(false);
    setCursor(-1);
    if (v !== value) onChange(v);
  }

  return (
    <div className="combo" style={width === undefined ? undefined : { width }}>
      <input
        value={text}
        placeholder={placeholder}
        title={title}
        onChange={(e) => { setText(e.target.value); setTyped(true); setOpen(true); setCursor(-1); }}
        onFocus={() => { setOpen(true); setTyped(false); setCursor(-1); }}
        onBlur={() => {
          setOpen(false);
          setTyped(false);
          if (decided.current) {
            decided.current = false;
            return;
          }
          if (allowFree) commit(text);
          else setText(value);
        }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setOpen(true);
            setCursor((c) => Math.min(c + 1, shown.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setCursor((c) => Math.max(c - 1, -1));
          } else if (e.key === "Enter") {
            e.preventDefault();
            decided.current = true;
            commit(cursor >= 0 && shown[cursor] ? shown[cursor] : text);
            e.currentTarget.blur();
          } else if (e.key === "Escape") {
            setText(value);
            setTyped(false);
            setCursor(-1);
            if (open) {
              // Dismissing the list is all this Escape means. Let through, it
              // reached the dialog's own Escape and closed the whole thing.
              e.stopPropagation();
              setOpen(false);
            } else {
              decided.current = true;
              e.currentTarget.blur();
            }
          }
        }}
      />
      {open && (
        <div className="combo-list">
          {shown.length === 0 && (
            <div className="combo-empty">{empty ?? "Nothing matches"}</div>
          )}
          {shown.map((o, i) => (
            <button
              key={o}
              type="button"
              className={i === cursor ? "on" : undefined}
              // Down, not click: a click fires after blur has already closed
              // the list out from under the pointer.
              onMouseDown={(e) => { e.preventDefault(); commit(o); }}
              onMouseEnter={() => setCursor(i)}
            >
              {o}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
