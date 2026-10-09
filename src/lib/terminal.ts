import type { ITheme } from "@xterm/xterm";
import { isLight, type Theme } from "./theme";

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
  // folke's tokyonight_night terminal colours, on the page's own --bg.
  "tokyo-night": {
    background: "#1a1b26",
    foreground: "#c0caf5",
    cursor: "#c0caf5",
    cursorAccent: "#1a1b26",
    selectionBackground: "#283457",
    black: "#15161e", red: "#f7768e", green: "#9ece6a", yellow: "#e0af68",
    blue: "#7aa2f7", magenta: "#bb9af7", cyan: "#7dcfff", white: "#a9b1d6",
    brightBlack: "#414868", brightRed: "#f7768e", brightGreen: "#9ece6a",
    brightYellow: "#e0af68", brightBlue: "#7aa2f7", brightMagenta: "#bb9af7",
    brightCyan: "#7dcfff", brightWhite: "#c0caf5",
  },
  // Cursor's own terminal colours (theme-cursor), on the page's --bg rather
  // than its panel's, and opaque where Cursor's are see-through.
  "cursor-dark": {
    background: "#181818",
    foreground: "#f0f0f0",
    cursor: "#f0f0f0",
    cursorAccent: "#181818",
    selectionBackground: "#3b3b3b",
    black: "#242424", red: "#fc6b83", green: "#3fa266", yellow: "#d2943e",
    blue: "#81a1c1", magenta: "#b48ead", cyan: "#88c0d0", white: "#f0f0f0",
    brightBlack: "#9a9a9a", brightRed: "#fc6b83", brightGreen: "#70b489",
    brightYellow: "#f1b467", brightBlue: "#87a6c4", brightMagenta: "#b48ead",
    brightCyan: "#88c0d0", brightWhite: "#ffffff",
  },
  "cursor-light": {
    background: "#fcfcfc",
    foreground: "#141414",
    cursor: "#141414",
    cursorAccent: "#fcfcfc",
    selectionBackground: "#dbdbdb",
    black: "#141414", red: "#be1744", green: "#007041", yellow: "#8b5700",
    blue: "#0064b0", magenta: "#92156a", cyan: "#176c74", white: "#6c6c6c",
    brightBlack: "#505050", brightRed: "#ce405b", brightGreen: "#00854c",
    brightYellow: "#a46700", brightBlue: "#2778c1", brightMagenta: "#b54e90",
    brightCyan: "#3b7e84", brightWhite: "#949494",
  },
  // Ayu's terminal colours as its extension publishes them, on the page's
  // --bg, with its accent for a cursor as its editor has.
  "ayu-dark": {
    background: "#10141c",
    foreground: "#bfbdb6",
    cursor: "#e6b450",
    cursorAccent: "#10141c",
    selectionBackground: "#193155",
    black: "#1b1f29", red: "#f06b73", green: "#70bf56", yellow: "#fdb04c",
    blue: "#4fbfff", magenta: "#d0a1ff", cyan: "#93e2c8", white: "#c7c7c7",
    brightBlack: "#686868", brightRed: "#f07178", brightGreen: "#aad94c",
    brightYellow: "#ffb454", brightBlue: "#59c2ff", brightMagenta: "#d2a6ff",
    brightCyan: "#95e6cb", brightWhite: "#ffffff",
  },
  "ayu-light": {
    background: "#fcfcfc",
    foreground: "#5c6166",
    cursor: "#f29718",
    cursorAccent: "#fcfcfc",
    selectionBackground: "#d7e4f6",
    black: "#000000", red: "#f06b6c", green: "#6cbf43", yellow: "#e7a100",
    blue: "#21a1e2", magenta: "#a176cb", cyan: "#4abc96", white: "#c7c7c7",
    brightBlack: "#686868", brightRed: "#f07171", brightGreen: "#86b300",
    brightYellow: "#eba400", brightBlue: "#22a4e6", brightMagenta: "#a37acc",
    brightCyan: "#4cbf99", brightWhite: "#d1d1d1",
  },
};

/**
 * A terminal's options for `theme`. An agent picks its own colours, often as
 * exact RGB for a dark background, and xterm darkens any too faint to read
 * on a light one. Nothing changes on a dark one, where agents expect to be.
 */
export function terminalTheme(theme: Theme): { theme: ITheme; minimumContrastRatio: number } {
  return { theme: PALETTES[theme], minimumContrastRatio: isLight(theme) ? 4.5 : 1 };
}

export const TERMINAL_FONT = '"SF Mono", "JetBrains Mono", Menlo, monospace';

/** A pane's output as the backend sends it, base64, back to bytes. */
export function decode(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
