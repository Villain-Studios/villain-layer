import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { listen } from "@tauri-apps/api/event";
import { api } from "../lib/api";
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
      scrollback: 20000,
      macOptionIsMeta: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    try {
      term.loadAddon(new WebglAddon());
    } catch {
      // Software rendering is fine; some VMs have no WebGL context.
    }

    termRef.current = term;
    fitRef.current = fit;

    try { fit.fit(); } catch { /* host not laid out yet */ }

    // Replay what the process printed before this component existed.
    api.ptyScrollback(pane.id)
      .then((b64) => { if (!disposed && b64) term.write(decode(b64)); })
      .catch(() => {});

    const onData = term.onData((d) => { void api.ptyWrite(pane.id, d).catch(() => {}); });
    const onResize = term.onResize(({ rows, cols }) => {
      void api.ptyResize(pane.id, rows, cols).catch(() => {});
    });

    // `disposed` matters here as well as for the scrollback: unlisten resolves
    // asynchronously, so an event can still arrive after term.dispose().
    const unlistenPromise = listen<{ pane_id: string; data: string }>(
      "pty:output",
      (e) => {
        if (disposed || e.payload.pane_id !== pane.id) return;
        term.write(decode(e.payload.data));
      },
    );

    const ro = new ResizeObserver(() => {
      if (host.offsetParent !== null) {
        try { fit.fit(); } catch { /* mid-layout */ }
      }
    });
    ro.observe(host);

    return () => {
      disposed = true;
      ro.disconnect();
      onData.dispose();
      onResize.dispose();
      void unlistenPromise.then((un) => un());
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [pane.id]);

  // Live-apply a font size change without tearing the session down.
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.fontSize = fontSize;
    try { fitRef.current?.fit(); } catch { /* not laid out */ }
  }, [fontSize]);

  // Re-fit and focus when this pane comes to the front.
  useEffect(() => {
    if (!visible) return;
    const id = requestAnimationFrame(() => {
      try { fitRef.current?.fit(); } catch { /* not laid out */ }
      termRef.current?.focus();
    });
    return () => cancelAnimationFrame(id);
  }, [visible]);

  return <div ref={hostRef} className={`term-host${visible ? "" : " hidden"}`} />;
}
