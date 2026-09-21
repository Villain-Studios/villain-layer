import { useEffect, useRef } from "react";
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
 * One xterm instance per pane, kept mounted across tab switches so scrollback
 * and cursor state survive. Hidden panes are visually hidden, never unmounted.
 *
 * Writes are skipped while the pane is off-screen or the window is in the
 * background — xterm still pays for every byte, and a long idle with agents
 * running is how coming back feels stuck. The backend keeps the real
 * scrollback; becoming visible again replays it.
 */
export function TerminalPane({ pane, visible }: { pane: PaneInfo; visible: boolean }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  // Terminal text scales on its own, not with the chrome: xterm measures a cell
  // grid, and a page zoom would leave the fit calculation half a pixel out.
  const fontSize = useStore((s) => s.settings?.ui.terminal_font_size ?? 13);
  const fontRef = useRef(fontSize);
  fontRef.current = fontSize;
  const appActive = useStore((s) => s.appActive);
  const liveRef = useRef(visible && appActive);
  const syncingRef = useRef(false);
  liveRef.current = visible && appActive && !syncingRef.current;
  const needsResync = useRef(false);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let disposed = false;

    const term = new Terminal({
      fontFamily: '"SF Mono", "JetBrains Mono", Menlo, monospace',
      fontSize: fontRef.current,
      lineHeight: 1.25,
      theme: THEME,
      cursorBlink: true,
      allowProposedApi: true,
      // Backend scrollback is the source of truth (~256KB). Keeping a huge
      // local buffer just burns memory across every mounted pane.
      scrollback: 5000,
      macOptionIsMeta: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    try {
      const webgl = new WebglAddon();
      // The GPU can take the context back — a sleep, a driver reset, too many
      // panes — and a terminal drawn through a dead context is just blank.
      // Dropping the addon returns xterm to its own renderer.
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      // Software rendering is fine; some VMs have no WebGL context.
    }

    termRef.current = term;
    fitRef.current = fit;

    /**
     * Fit, and say so.
     *
     * `onResize` fires only when xterm's own grid changes, so a process
     * spawned at the default size sits at that size forever if the fit
     * happens to agree with where xterm started. Pushing the dimensions
     * after every fit costs one call and means the child is never drawing
     * to a window that is not the one on screen.
     */
    const fitAndTell = () => {
      try { fit.fit(); } catch { return; /* not laid out yet */ }
      void api.ptyResize(pane.id, term.rows, term.cols).catch(() => {});
    };

    fitAndTell();

    // Replay what the process printed before this component existed.
    api.ptyScrollback(pane.id)
      .then((b64) => { if (!disposed && b64) term.write(decode(b64)); })
      .catch(() => {});

    const onData = term.onData((d) => { void api.ptyWrite(pane.id, d).catch(() => {}); });
    const onResize = term.onResize(({ rows, cols }) => {
      void api.ptyResize(pane.id, rows, cols).catch(() => {});
    });

    // `disposed` matters here as well as for the scrollback: output can still
    // arrive after term.dispose() if a chunk was already in flight.
    const stopOutput = onPtyOutput(pane.id, (data) => {
      if (disposed) return;
      if (!liveRef.current) {
        needsResync.current = true;
        return;
      }
      term.write(decode(data));
    });

    const ro = new ResizeObserver(() => {
      if (host.offsetParent !== null) fitAndTell();
    });
    ro.observe(host);

    return () => {
      disposed = true;
      ro.disconnect();
      onData.dispose();
      onResize.dispose();
      stopOutput();
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [pane.id]);

  // While the window sleeps the backend stops emitting output altogether, so
  // nothing arrives to raise the flag in the handler above — and the process
  // has kept printing into the scrollback all the same. Without this the
  // pane came back showing whatever was on screen when the window went into
  // the background, with everything since missing until the next reset.
  useEffect(() => {
    if (!appActive) needsResync.current = true;
  }, [appActive]);

  // Live-apply a font size change without tearing the session down.
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.fontSize = fontSize;
    try { fitRef.current?.fit(); } catch { /* not laid out */ }
  }, [fontSize]);

  // Re-fit, focus, and catch up scrollback when this pane is on screen again.
  useEffect(() => {
    if (!visible || !appActive) return;
    const term = termRef.current;
    if (!term) return;
    let cancelled = false;

    const show = async () => {
      if (needsResync.current) {
        // Hold live writes until the replay lands, or a chunk arriving mid-
        // reset would paint over (or under) the scrollback we just fetched.
        syncingRef.current = true;
        liveRef.current = false;
        needsResync.current = false;
        try {
          const b64 = await api.ptyScrollback(pane.id);
          if (cancelled || !termRef.current) return;
          term.reset();
          if (b64) term.write(decode(b64));
        } catch { /* leave whatever was there */ }
        finally {
          syncingRef.current = false;
          liveRef.current = visible && appActive;
        }
      }
      if (cancelled) return;
      try { fitRef.current?.fit(); } catch { /* not laid out */ }
      void api.ptyResize(pane.id, term.rows, term.cols).catch(() => {});
      term.focus();
    };

    const id = requestAnimationFrame(() => { void show(); });
    return () => {
      cancelled = true;
      cancelAnimationFrame(id);
    };
  }, [visible, appActive, pane.id]);

  return <div ref={hostRef} className={`term-host${visible ? "" : " hidden"}`} />;
}
