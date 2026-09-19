import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";

export function Modal({
  title, children, footer, onClose, wide,
}: {
  title: string;
  children: ReactNode;
  footer?: ReactNode;
  onClose: () => void;
  wide?: boolean;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="overlay" onMouseDown={onClose}>
      <div
        className="modal"
        style={wide ? { maxWidth: 720 } : undefined}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="modal-head">
          {title}
          <div className="spacer" />
          <button className="btn-sm" onClick={onClose}>✕</button>
        </div>
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
  x, y, items, onClose,
}: {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      left: Math.min(x, window.innerWidth - r.width - 8),
      top: Math.min(y, window.innerHeight - r.height - 8),
    });
  }, [x, y]);

  useEffect(() => {
    const close = () => onClose();
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    // Capture so a click anywhere dismisses before it does anything else.
    window.addEventListener("mousedown", close, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  return (
    <div
      ref={ref}
      className="ctx-menu"
      style={{ left: pos.left, top: pos.top }}
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
