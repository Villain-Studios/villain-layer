/** What every terminal in the app draws with: the window's and the phone's. */
export const TERMINAL_THEME = {
  background: "#0c0c11",
  foreground: "#dcdce6",
  cursor: "#a78bfa",
  cursorAccent: "#0c0c11",
  selectionBackground: "#332f55",
  black: "#1c1c27", red: "#f87171", green: "#4ade80", yellow: "#fbbf24",
  blue: "#60a5fa", magenta: "#a78bfa", cyan: "#22d3ee", white: "#dcdce6",
  brightBlack: "#55555f", brightRed: "#fca5a5", brightGreen: "#86efac",
  brightYellow: "#fcd34d", brightBlue: "#93c5fd", brightMagenta: "#c4b5fd",
  brightCyan: "#67e8f9", brightWhite: "#f4f4f8",
};

export const TERMINAL_FONT = '"SF Mono", "JetBrains Mono", Menlo, monospace';

/** A pane's output as the backend sends it, base64, back to bytes. */
export function decode(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
