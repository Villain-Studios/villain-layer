import { useEffect, useLayoutEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { api } from "../lib/api";
import { onPtyOutput } from "../lib/ptyOutput";
import { useStore } from "../store";
import type { PaneInfo } from "../lib/types";

const THEME = {
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

function decode(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/**
 * How many terminals may hold a WebGL context at once.
 *
 * WebKit allows only a handful per page and takes the oldest back without
 * asking once there are more. Terminals now outlive being on screen, so
 * rather than let the browser pick, the ones shown least recently go back to
 * xterm's own renderer and get a context again when they are next shown.
 */
const GPU_TERMINALS = 6;

const fontSize = () => useStore.getState().settings?.ui.terminal_font_size ?? 13;

/** Where a terminal waits while no view is showing it. */
let parking: HTMLDivElement | null = null;
function park(el: HTMLElement) {
  if (!parking) {
    parking = document.createElement("div");
    // Not laid out, so a parked terminal is never fitted to it — and never
    // tells its process that the window is some other size.
    parking.style.display = "none";
    document.body.appendChild(parking);
  }
  parking.appendChild(el);
}

/**
 * Whether showing a terminal may move the keyboard into it.
 *
 * Coming back to the window re-shows the pane on screen, and taking focus
 * then took it from whatever had it: switch away to copy a token for the
 * Settings field, switch back, paste — and the token went to the agent.
 */
function mayTakeFocus(): boolean {
  if (document.querySelector(".overlay")) return false;
  const a = document.activeElement;
  if (!(a instanceof HTMLElement) || a === document.body) return true;
  if (a.closest(".term-host")) return true;
  return !(
    a instanceof HTMLInputElement ||
    a instanceof HTMLTextAreaElement ||
    a instanceof HTMLSelectElement ||
    a.isContentEditable
  );
}

/** Terminals that hold a WebGL context, least recently shown first. */
const gpuOrder: PaneTerminal[] = [];

/**
 * One pane's terminal, for as long as the pane exists.
 *
 * It used to live and die with the React component that showed it, and that
 * component is unmounted by switching task or switching view. Every switch
 * back built a new xterm and a new WebGL context and replayed up to 256KB of
 * scrollback into it: the flash and the pause on every task change. Now the
 * terminal is kept, its element is moved into whichever view shows it, and
 * coming back asks only for what was printed while it was away.
 *
 * Nothing is fed to a terminal nobody can see. The backend holds a hidden
 * pane's output in scrollback, and `pty_attach` hands over whatever came
 * after the last byte this terminal drew.
 */
class PaneTerminal {
  private readonly term: Terminal;
  private readonly fit = new FitAddon();
  /** The element xterm draws into, moved between views rather than rebuilt. */
  private readonly el = document.createElement("div");
  private webgl: WebglAddon | null = null;
  /** How far into the pane's output this terminal has drawn. Null until first shown. */
  private have: number | null = null;
  private syncing = false;
  private again = false;
  /** Chunks that arrived while a catch-up was out, applied when it lands. */
  private held: [Uint8Array, number][] = [];
  private shown = false;
  private opened = false;
  private disposed = false;
  private readonly subs: { dispose(): void }[] = [];
  private readonly stopOutput: () => void;
  private readonly ro: ResizeObserver;

  constructor(private readonly id: string) {
    this.el.className = "term-host";
    this.term = new Terminal({
      fontFamily: '"SF Mono", "JetBrains Mono", Menlo, monospace',
      fontSize: fontSize(),
      lineHeight: 1.25,
      theme: THEME,
      cursorBlink: true,
      allowProposedApi: true,
      // Backend scrollback is the source of truth (~256KB). Keeping a huge
      // local buffer just burns memory across every terminal kept.
      scrollback: 5000,
      macOptionIsMeta: true,
    });
    this.term.loadAddon(this.fit);
    this.subs.push(
      this.term.onData((d) => { void api.ptyWrite(id, d).catch(() => {}); }),
      this.term.onResize(({ rows, cols }) => {
        void api.ptyResize(id, rows, cols).catch(() => {});
      }),
    );
    this.stopOutput = onPtyOutput(id, (data, end) => this.receive(data, end));
    this.ro = new ResizeObserver(() => {
      if (this.el.offsetParent !== null) this.fitAndTell();
    });
    this.ro.observe(this.el);
  }

  mount(slot: HTMLElement) {
    if (this.disposed) return;
    slot.appendChild(this.el);
  }

  unmount(slot: HTMLElement) {
    this.setShown(false);
    if (this.el.parentElement === slot) park(this.el);
  }

  setShown(shown: boolean) {
    if (this.disposed || this.shown === shown) return;
    this.shown = shown;
    if (!shown) {
      void api.ptyDetach(this.id).catch(() => {});
      return;
    }
    // Next frame: the slot has only just been made visible, and fitting
    // before it is laid out as such measures the size it had while hidden.
    requestAnimationFrame(() => {
      if (!this.shown || this.disposed) return;
      if (!this.opened) {
        // Opened on first show, not on creation: xterm measures its font
        // against the element, and one inside a hidden tab measures nothing.
        this.term.open(this.el);
        this.opened = true;
      }
      this.gpu();
      this.fitAndTell();
      void this.sync();
      if (mayTakeFocus()) this.term.focus();
    });
  }

  setFontSize(size: number) {
    if (this.disposed) return;
    this.term.options.fontSize = size;
    if (this.el.offsetParent !== null) this.fitAndTell();
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.ro.disconnect();
    this.stopOutput();
    for (const s of this.subs) s.dispose();
    this.dropGpu();
    this.term.dispose();
    this.el.remove();
  }

  /**
   * Fit, and say so.
   *
   * `onResize` fires only when xterm's own grid changes, so a process
   * spawned at the default size sits at that size forever if the fit
   * happens to agree with where xterm started. Pushing the dimensions after
   * every fit costs one call and means the child is never drawing to a
   * window that is not the one on screen.
   */
  private fitAndTell() {
    if (!this.opened) return;
    try { this.fit.fit(); } catch { return; /* not laid out yet */ }
    void api.ptyResize(this.id, this.term.rows, this.term.cols).catch(() => {});
  }

  private receive(data: string, end: number) {
    // Off screen, the backend has been told to stop sending; a chunk already
    // on its way is covered by the catch-up when this is shown again.
    if (this.disposed || !this.shown) return;
    const bytes = decode(data);
    if (this.syncing || this.have === null) {
      this.held.push([bytes, end]);
      return;
    }
    if (!this.apply(bytes, end)) void this.sync();
  }

  /**
   * Draw `bytes`, which end at `end` in the pane's output, keeping only the
   * part not already drawn. False means there is a gap before them.
   */
  private apply(bytes: Uint8Array, end: number): boolean {
    const have = this.have ?? 0;
    const start = end - bytes.length;
    if (end <= have) return true;
    if (start > have) return false;
    this.term.write(start < have ? bytes.subarray(have - start) : bytes);
    this.have = end;
    return true;
  }

  /** Ask for everything after what this terminal has, and draw it. */
  private async sync() {
    if (this.syncing) {
      this.again = true;
      return;
    }
    this.syncing = true;
    try {
      do {
        this.again = false;
        const c = await api.ptyAttach(this.id, this.have);
        if (this.disposed) return;
        // Too far behind to catch up: the scrollback has moved on past the
        // last byte drawn, so start again from what it still holds.
        if (c.reset) this.term.reset();
        if (c.data) this.term.write(decode(c.data));
        this.have = c.end;
        const held = this.held;
        this.held = [];
        for (const [bytes, end] of held) {
          if (!this.apply(bytes, end)) this.again = true;
        }
      } while (this.again && this.shown && !this.disposed);
    } catch {
      // The pane has gone; the next pane list drops this terminal.
      this.held = [];
    } finally {
      this.syncing = false;
    }
  }

  private gpu() {
    const at = gpuOrder.indexOf(this);
    if (at >= 0) gpuOrder.splice(at, 1);
    if (!this.webgl) {
      try {
        const webgl = new WebglAddon();
        // The GPU can take the context back — a sleep, a driver reset — and
        // a terminal drawn through a dead context is just blank. Dropping
        // the addon returns xterm to its own renderer.
        webgl.onContextLoss(() => this.dropGpu());
        this.term.loadAddon(webgl);
        this.webgl = webgl;
      } catch {
        // Software rendering is fine; some VMs have no WebGL context.
        return;
      }
    }
    gpuOrder.push(this);
    while (gpuOrder.length > GPU_TERMINALS) {
      const oldest = gpuOrder.find((t) => !t.shown);
      if (!oldest) break;
      oldest.dropGpu();
    }
  }

  private dropGpu() {
    const at = gpuOrder.indexOf(this);
    if (at >= 0) gpuOrder.splice(at, 1);
    this.webgl?.dispose();
    this.webgl = null;
  }
}

const pool = new Map<string, PaneTerminal>();

function terminalFor(id: string): PaneTerminal {
  let t = pool.get(id);
  if (!t) {
    t = new PaneTerminal(id);
    pool.set(id, t);
  }
  return t;
}

// A pane that has left the list has been closed, and its terminal goes with
// it. Font size applies to every terminal kept, not only the one on screen.
useStore.subscribe((s, prev) => {
  if (s.panes !== prev.panes) {
    const live = new Set(s.panes.map((p) => p.id));
    for (const [id, t] of pool) {
      if (!live.has(id)) {
        t.dispose();
        pool.delete(id);
      }
    }
  }
  const size = s.settings?.ui.terminal_font_size;
  if (size !== undefined && size !== prev.settings?.ui.terminal_font_size) {
    for (const t of pool.values()) t.setFontSize(size);
  }
});

// A hot reload brings a fresh pool; the old one's terminals would linger.
import.meta.hot?.dispose(() => {
  for (const t of pool.values()) t.dispose();
  pool.clear();
});

/**
 * Where a pane's terminal is shown. The terminal itself outlives this: see
 * `PaneTerminal`. Hidden slots stay laid out, so a pane that is merely behind
 * another one in the same task is fitted along with it.
 */
export function TerminalPane({ pane, visible }: { pane: PaneInfo; visible: boolean }) {
  const slotRef = useRef<HTMLDivElement>(null);
  const appActive = useStore((s) => s.appActive);

  useLayoutEffect(() => {
    const slot = slotRef.current;
    if (!slot) return;
    const t = terminalFor(pane.id);
    t.mount(slot);
    return () => t.unmount(slot);
  }, [pane.id]);

  // Shown means on screen with the window in front. Anything less and the
  // terminal detaches, and catches up from where it was when it is back.
  useEffect(() => {
    pool.get(pane.id)?.setShown(visible && appActive);
  }, [pane.id, visible, appActive]);

  return <div ref={slotRef} className={`term-slot${visible ? "" : " hidden"}`} />;
}
