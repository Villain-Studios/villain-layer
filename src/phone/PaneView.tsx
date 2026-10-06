import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { decode, TERMINAL_FONT, TERMINAL_THEME } from "../lib/terminal";
import { follow, phoneApi } from "./client";
import { stateOf } from "./Overview";
import type { OutputLine, PhonePane } from "./types";

/** The key bar (PHONE-7): what an agent's questions and menus take. */
const KEYS: [string, string][] = [
  ["esc", "Esc"],
  ["up", "↑"],
  ["down", "↓"],
  ["enter", "⏎"],
  ["1", "1"],
  ["2", "2"],
  ["3", "3"],
  ["tab", "Tab"],
  ["shift_tab", "⇧Tab"],
  ["ctrl_c", "^C"],
];

/** A monospace cell is about this much of the font size wide. */
const CELL = 0.6;

/**
 * One pane, drawn at the size the window gave it and never resized from
 * here: the phone changing the terminal's size would scramble the Mac's
 * view of it (PHONE-6). It is scaled to the phone's width instead; "Zoom"
 * draws it larger, to be scrolled sideways.
 */
export function PaneView({
  id, pane, typing, onBack,
}: {
  id: string;
  pane: PhonePane | null;
  typing: boolean;
  onBack: () => void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const [size, setSize] = useState<[number, number]>([30, 100]);
  const [zoom, setZoom] = useState(false);
  const [width, setWidth] = useState(() => window.innerWidth);
  const [exited, setExited] = useState<number | null | undefined>(undefined);
  const [live, setLive] = useState(false);
  const [gone, setGone] = useState(false);

  useEffect(() => {
    const onResize = () => setWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    const t = new Terminal({
      fontFamily: TERMINAL_FONT,
      fontSize: 8,
      lineHeight: 1.15,
      theme: TERMINAL_THEME,
      disableStdin: true,
      cursorBlink: false,
      scrollback: 3000,
      rows: 30,
      cols: 100,
    });
    if (host.current) t.open(host.current);
    term.current = t;
    let have: number | null = null;
    const stop = follow(
      () => `/api/panes/${encodeURIComponent(id)}/output${have === null ? "" : `?from=${have}`}`,
      (raw) => {
        const line = raw as OutputLine;
        if ("size" in line) {
          const [rows, cols] = line.size;
          t.resize(cols, rows);
          setSize([rows, cols]);
        } else if ("data" in line) {
          if (line.reset) t.reset();
          t.write(decode(line.data));
          have = line.end;
        } else if ("exit" in line) {
          setExited(line.exit);
          return true;
        } else if ("gone" in line) {
          setGone(true);
          return true;
        }
      },
      setLive,
    );
    return () => {
      stop();
      t.dispose();
      term.current = null;
    };
  }, [id]);

  // Fit the terminal's columns to the screen, or twice that when zoomed.
  const fitted = Math.max(4, Math.min(14, Math.floor(((width - 8) / (size[1] * CELL)) * 10) / 10));
  const font = zoom ? Math.max(10, fitted * 2) : fitted;
  useEffect(() => {
    if (term.current) term.current.options.fontSize = font;
  }, [font]);

  const st = pane ? stateOf(pane) : null;
  const canType = typing && pane?.kind === "agent" && pane.running && exited === undefined && !gone;
  return (
    <div className="screen">
      <header className="bar">
        <button className="back" onClick={onBack}>‹</button>
        <div className="title">
          <div className="pane-name">{pane?.name ?? "Terminal"}</div>
          {st && (
            <div className="pane-state">
              <span className={`dot ${st.dot}`} /> {st.label}
            </div>
          )}
        </div>
        <button className={`chip-btn${zoom ? " on" : ""}`} onClick={() => setZoom((z) => !z)}>
          Zoom
        </button>
        <span className={`dot ${live ? "live" : "gone"}`} title={live ? "Connected" : "Not connected"} />
      </header>
      <div className={`term-wrap${zoom ? " zoomed" : ""}`}>
        <div ref={host} className="term" />
      </div>
      {exited !== undefined && <div className="note">Exited{exited !== null ? ` with ${exited}` : ""}.</div>}
      {gone && <div className="note">This terminal was closed on the Mac.</div>}
      {canType ? (
        <Typing id={id} />
      ) : pane?.kind === "shell" ? (
        <div className="note">A shell can only be watched from the phone.</div>
      ) : (
        pane?.running && !typing && (
          <div className="note">Watching only. Typing can be turned on in the app's Settings, under Phone.</div>
        )
      )}
    </div>
  );
}

/** A line to send, and the key bar. */
function Typing({ id }: { id: string }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function run(work: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="typing">
      {error && <div className="error">{error}</div>}
      <div className="keys">
        {KEYS.map(([key, label]) => (
          <button key={key} disabled={busy} onClick={() => void run(() => phoneApi.key(id, key))}>
            {label}
          </button>
        ))}
      </div>
      <form
        className="line"
        onSubmit={(e) => {
          e.preventDefault();
          const line = text.trim();
          if (!line) return;
          void run(async () => {
            await phoneApi.send(id, line);
            setText("");
          });
        }}
      >
        <input
          value={text}
          placeholder="Tell the agent…"
          enterKeyHint="send"
          autoCapitalize="sentences"
          onChange={(e) => setText(e.target.value)}
        />
        <button className="primary" disabled={busy || !text.trim()}>
          Send
        </button>
      </form>
    </div>
  );
}
