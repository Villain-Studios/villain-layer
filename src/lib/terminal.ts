import type { ITheme } from "@xterm/xterm";
import type { Theme } from "./theme";

/** What every terminal in the app draws with, per theme: the window's and the phone's (SET-5). */
const PALETTES: Record<Theme, ITheme> = {
  dark: {
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
  },
  // "White" is a grey: programs print it meaning "readable", assuming a dark
  // background, as every light terminal's palette does.
  light: {
    background: "#ffffff",
    foreground: "#1c1c24",
    cursor: "#6d28d9",
    cursorAccent: "#ffffff",
    selectionBackground: "#ddd6fe",
    black: "#1c1c24", red: "#dc2626", green: "#15803d", yellow: "#a16207",
    blue: "#2563eb", magenta: "#7c3aed", cyan: "#0e7490", white: "#6a6a77",
    brightBlack: "#8c8c99", brightRed: "#b91c1c", brightGreen: "#166534",
    brightYellow: "#854d0e", brightBlue: "#1d4ed8", brightMagenta: "#6d28d9",
    brightCyan: "#155e75", brightWhite: "#9a9aa6",
  },
};

/**
 * A terminal's options for `theme`. An agent picks its own colours, often as
 * exact RGB for a dark background, and xterm darkens any too faint to read
 * on a light one. Nothing changes in the dark, where agents expect to be.
 */
export function terminalTheme(theme: Theme): { theme: ITheme; minimumContrastRatio: number } {
  return { theme: PALETTES[theme], minimumContrastRatio: theme === "light" ? 4.5 : 1 };
}

export const TERMINAL_FONT = '"SF Mono", "JetBrains Mono", Menlo, monospace';

/** A pane's output as the backend sends it, base64, back to bytes. */
export function decode(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
