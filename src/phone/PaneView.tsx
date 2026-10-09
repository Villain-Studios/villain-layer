import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { decode, TERMINAL_FONT, terminalTheme } from "../lib/terminal";
import { currentTheme, type Theme } from "../lib/theme";
import { follow, phoneApi } from "./client";
import { stateOf } from "./Overview";
import { readable, type Row } from "./readable";
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
/** The most text lines kept on screen: the end of the conversation. */
const TEXT_LINES = 600;

type Mode = "text" | "screen";
const MODE = "villain.phone.mode";

function savedMode(): Mode {
  try {
    return localStorage.getItem(MODE) === "screen" ? "screen" : "text";
  } catch {
    return "text";
  }
}

/**
 * One pane (PHONE-6). Its output goes through a real terminal at the size
 * the window gave it, never resized from here: the phone changing the
 * terminal's size would scramble the Mac's view of it. By default that
 * terminal's rows are shown as text, wrapped to the phone (`readable.ts`);
 * "Screen" shows the terminal itself, scaled to fit, and "Zoom" larger, to
 * be scrolled sideways.
 */
export function PaneView({
  id, pane, typing, theme, onBack,
}: {
  id: string;
  pane: PhonePane | null;
  typing: boolean;
  theme: Theme;
  onBack: () => void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const [size, setSize] = useState<[number, number]>([30, 100]);
  const [zoom, setZoom] = useState(false);
  const [mode, setMode] = useState<Mode>(savedMode);
  const [lines, setLines] = useState<string[]>([]);
  const textBox = useRef<HTMLDivElement>(null);
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
      ...terminalTheme(currentTheme()),
      disableStdin: true,
      cursorBlink: false,
      scrollback: 3000,
      rows: 30,
      cols: 100,
    });
    if (host.current) t.open(host.current);
    term.current = t;
    let have: number | null = null;
    // The text is read back from the terminal at most once a frame.
    let reading = 0;
    const read = () => {
      if (reading) return;
      reading = requestAnimationFrame(() => {
        reading = 0;
        const b = t.buffer.active;
        const rows: Row[] = [];
        for (let i = 0; i < b.length; i++) {
          const l = b.getLine(i);
          if (l) rows.push({ text: l.translateToString(true), wrapped: l.isWrapped });
        }
        setLines(readable(rows).slice(-TEXT_LINES));
      });
    };
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
          t.write(decode(line.data), read);
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
      cancelAnimationFrame(reading);
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
  useEffect(() => {
    if (term.current) Object.assign(term.current.options, terminalTheme(theme));
  }, [theme]);

  // Follow the end, unless you have scrolled up to read something.
  const pinned = useRef(true);
  useEffect(() => {
    const el = textBox.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  }, [lines, mode]);

  function choose(m: Mode) {
    setMode(m);
    try {
      localStorage.setItem(MODE, m);
    } catch {
      // Remembered until the page closes, then.
    }
  }

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
        <button className="chip-btn" onClick={() => choose(mode === "text" ? "screen" : "text")}>
          {mode === "text" ? "Screen" : "Text"}
        </button>
        {mode === "screen" && (
          <button className={`chip-btn${zoom ? " on" : ""}`} onClick={() => setZoom((z) => !z)}>
            Zoom
          </button>
        )}
        <span className={`dot ${live ? "live" : "gone"}`} title={live ? "Connected" : "Not connected"} />
      </header>
      {mode === "text" && (
        <div
          ref={textBox}
          className="term-text"
          onScroll={(e) => {
            const el = e.currentTarget;
            pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
          }}
        >
          {lines.length ? lines.join("\n") : <span className="muted">Nothing printed yet.</span>}
        </div>
      )}
      {/* Always there: it is what turns the output into rows, shown or not. */}
      <div className={`term-wrap${zoom ? " zoomed" : ""}${mode === "text" ? " hidden" : ""}`}>
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
