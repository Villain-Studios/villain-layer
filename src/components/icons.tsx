import type { ReactNode } from "react";

/*
 * Icons are drawn rather than typed. A glyph (⚙, ✕, ▶, ⇄, +) takes its size
 * and weight from whatever font the fallback lands on: the ▶ chevrons were
 * 8px of ink, the ✕ on a pane tab thinner than the text beside it. A drawn
 * icon is the same size and stroke everywhere. It stays inline, so a button
 * holding only an icon is as tall as a labelled one beside it.
 */

function Svg({ size, stroke = 1.8, children }: { size: number; stroke?: number; children: ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={stroke}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ verticalAlign: "middle", flex: "none" }}
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

export function GearIcon({ size = 20 }: { size?: number }) {
  return (
    <Svg size={size}>
      <circle cx="12" cy="12" r="3.2" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.6a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9v.09a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </Svg>
  );
}

/** A bell, for the message center. */
export function BellIcon({ size = 20 }: { size?: number }) {
  return (
    <Svg size={size}>
      <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
      <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
    </Svg>
  );
}

/** Points right; `.chev.open` turns it down. */
export function ChevronIcon({ size = 12 }: { size?: number }) {
  return (
    <Svg size={size} stroke={2.6}>
      <path d="M9 5l7 7-7 7" />
    </Svg>
  );
}

export function CloseIcon({ size = 14 }: { size?: number }) {
  return (
    <Svg size={size} stroke={2.2}>
      <path d="M6 6l12 12M18 6L6 18" />
    </Svg>
  );
}

export function PlusIcon({ size = 16 }: { size?: number }) {
  return (
    <Svg size={size} stroke={2.2}>
      <path d="M12 5v14M5 12h14" />
    </Svg>
  );
}

/** Hand a pane's work to another agent. */
export function SwapIcon({ size = 14 }: { size?: number }) {
  return (
    <Svg size={size} stroke={2}>
      <path d="M4 8h15l-4-4M20 16H5l4 4" />
    </Svg>
  );
}

export function ExternalIcon({ size = 14 }: { size?: number }) {
  return (
    <Svg size={size} stroke={2}>
      <path d="M8 16L17 7M9 7h8v8" />
    </Svg>
  );
}

export function CopyIcon({ size = 14 }: { size?: number }) {
  return (
    <Svg size={size} stroke={2}>
      <rect x="8" y="8" width="12" height="12" rx="2" />
      <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
    </Svg>
  );
}
