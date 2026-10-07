import { useEffect, useRef, useState } from "react";
import { Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errMessage } from "../lib/api";
import { key, mouse, pagePoint, wheel } from "../lib/browserInput";
import { copyText } from "../lib/clipboard";
import { ago } from "../lib/time";
import type { BrowserInput, BrowserView } from "../lib/types";
import { useStore } from "../store";
import { BrowserTabs } from "./BrowserTabs";
import { BackIcon, CloseIcon, ExternalIcon, ForwardIcon, ReloadIcon } from "./icons";
import { Spinner } from "./ui";

/**
 * A task's tab, as `browser_view` has it, kept current by `browser:changed`.
 * Null until the first answer.
 */
export function useBrowserView(taskId: string): BrowserView | null {
  const [view, setView] = useState<BrowserView | null>(null);
  useEffect(() => {
    let current = true;
    // Each answer replaces the one before only if it was asked later: two
    // changes in a row each ask, and the first answer can come second.
    let asked = 0;
    const load = () => {
      const n = ++asked;
      api.browserView(taskId)
        .then((v) => { if (current && n === asked) setView(v); })
        .catch(() => {});
    };
    load();
    const un = listen<string>("browser:changed", (e) => {
      if (e.payload === "" || e.payload === taskId) load();
    });
    return () => {
      current = false;
      void un.then((f) => f());
    };
  }, [taskId]);
  return view;
}

/** How long after its last browser call an agent still at work is taken to be using the tab. */
const DRIVING_LINGER = 60_000;

/** The page's size in CSS pixels, as the panel lays it out. */
type Size = { width: number; height: number };

/**
 * The task's browser tab beside its terminals (BRW-9): what it shows, live,
 * and the user's own mouse and keys sent to it, to sign in or to help an
 * agent that is stuck. What an agent last did is shown under it (BRW-10).
 */
export function BrowserPanel({ taskId, visible, onClose }: { taskId: string; visible: boolean; onClose: () => void }) {
  const view = useBrowserView(taskId);
  const fail = useStore((s) => s.fail);
  const refreshSettings = useStore((s) => s.refreshSettings);
  const allowed = useStore((s) => s.settings?.browser_sites);
  const [address, setAddress] = useState("");
  const [editing, setEditing] = useState(false);
  const [size, setSize] = useState<Size | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [focused, setFocused] = useState(false);
  const [marker, setMarker] = useState<{ x: number; y: number; at: number } | null>(null);
  const [promptText, setPromptText] = useState("");
  const dialogShown = view?.dialog ? `${view.dialog.kind}:${view.dialog.message}` : "";
  useEffect(() => { setPromptText(view?.dialog?.default_prompt ?? ""); }, [dialogShown]); // eslint-disable-line react-hooks/exhaustive-deps -- a new dialog, not every refresh
  const screenRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const keysRef = useRef<HTMLTextAreaElement>(null);
  const addressRef = useRef<HTMLInputElement>(null);

  // The address bar follows the page, except while it is being typed in.
  const url = view?.url ?? "";
  useEffect(() => {
    if (!editing) setAddress(url === "about:blank" ? "" : url);
  }, [url, editing]);

  // The page is laid out at the panel's size, so what the user sees and what
  // an agent reads are the same page.
  useEffect(() => {
    const el = screenRef.current;
    if (!el) return;
    let timer: number | undefined;
    const measure = () => {
      const r = el.getBoundingClientRect();
      if (r.width < 50 || r.height < 50) return;
      // Settled before it is sent: a dragged divider resizes it every frame,
      // and each size is a relayout of the page.
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        setSize((s) => {
          const next = { width: Math.round(r.width), height: Math.round(r.height) };
          return s && s.width === next.width && s.height === next.height ? s : next;
        });
      }, 150);
    };
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    measure();
    return () => { ro.disconnect(); window.clearTimeout(timer); };
  }, [view?.chrome]);

  // Frames only while on screen (BRW-9). Watching makes the tab, starting
  // the browser, the first time; a new tab (the browser restarted) is
  // watched afresh.
  const tab = view?.tab ?? null;
  const chrome = view?.chrome ?? false;
  useEffect(() => {
    if (!visible || !chrome || !size) return;
    let current = true;
    let pending: ArrayBuffer | null = null;
    let drawing = false;
    // Only the newest frame is drawn: one that arrives while another is
    // being decoded replaces it, so a slow decode never builds a queue.
    const draw = async () => {
      drawing = true;
      while (pending && current) {
        const jpeg = pending;
        pending = null;
        try {
          const bitmap = await createImageBitmap(new Blob([jpeg], { type: "image/jpeg" }));
          const c = canvasRef.current;
          const ctx = c?.getContext("2d");
          if (c && ctx) ctx.drawImage(bitmap, 0, 0, c.width, c.height);
          bitmap.close();
        } catch {
          // A frame that will not decode is skipped; the next one replaces it.
        }
      }
      drawing = false;
    };
    const frames = new Channel<ArrayBuffer>();
    frames.onmessage = (jpeg) => {
      pending = jpeg;
      if (!drawing) void draw();
    };
    setError(null);
    api.browserWatch(taskId, size.width, size.height, window.devicePixelRatio || 1, frames)
      .catch((e) => { if (current) setError(errMessage(e)); });
    return () => {
      current = false;
      void api.browserUnwatch(taskId).catch(() => {});
    };
  }, [visible, chrome, size, tab, taskId, attempt]);

  // Where an agent clicked, for a moment (BRW-10).
  const last = view?.last_action ?? null;
  const [lastX, lastY, lastAt] = [last?.x ?? null, last?.y ?? null, last?.at ?? 0];
  useEffect(() => {
    if (lastX === null || lastY === null || Date.now() - lastAt > 3000) return;
    setMarker({ x: lastX, y: lastY, at: lastAt });
    const t = window.setTimeout(() => setMarker(null), 1200);
    return () => window.clearTimeout(t);
  }, [lastX, lastY, lastAt]);

  // Whether an agent is using the tab (BRW-15): while a browser call of its
  // runs, and between calls for as long as it is still working on its turn,
  // up to a minute after its last one. An agent thinks between calls, and
  // an overlay that came and went with each call said nothing.
  const driver = view?.driver ?? null;
  const driverWorking = useStore((s) =>
    driver ? s.panes.find((p) => p.id === driver.pane)?.activity === "working" : false,
  );
  const [, setTick] = useState(0);
  const lingering = !!driver && driverWorking && Date.now() - driver.last_at < DRIVING_LINGER;
  const driving = !!driver && !view?.held && (driver.calls > 0 || lingering);
  // Looked at again when the minute runs out, which no event says.
  useEffect(() => {
    if (!driver || driver.calls > 0 || !driverWorking) return;
    const left = driver.last_at + DRIVING_LINGER - Date.now();
    if (left <= 0) return;
    const t = window.setTimeout(() => setTick((n) => n + 1), left + 50);
    return () => window.clearTimeout(t);
  }, [driver, driverWorking]);
  const drivingRef = useRef(driving);
  drivingRef.current = driving;

  // The wheel is a native listener: React's is passive, and the panel's own
  // scrolling must not happen as well as the page's.
  const sizeRef = useRef(size);
  sizeRef.current = size;
  const showing = chrome && !error;
  useEffect(() => {
    const el = canvasRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      if (drivingRef.current) return;
      const page = sizeRef.current;
      if (page) send(wheel(e, pagePoint(e, el.getBoundingClientRect(), page)));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [showing, taskId]); // eslint-disable-line react-hooks/exhaustive-deps -- `send` reads only taskId

  function send(input: BrowserInput) {
    void api.browserInput(taskId, input).catch(() => {});
  }

  // Moves coalesce to one per frame: a mouse reports far more often than
  // a page needs to hear it.
  const move = useRef<{ e: MouseEvent; frame: number } | null>(null);
  function onMouse(type: "mousePressed" | "mouseReleased" | "mouseMoved", e: React.MouseEvent<HTMLCanvasElement>) {
    if (!size) return;
    const el = e.currentTarget;
    const native = e.nativeEvent;
    if (type === "mousePressed") {
      e.preventDefault();
      keysRef.current?.focus();
    }
    if (type !== "mouseMoved") {
      send(mouse(type, native, pagePoint(native, el.getBoundingClientRect(), size)));
      return;
    }
    if (move.current) {
      move.current.e = native;
      return;
    }
    move.current = {
      e: native,
      frame: requestAnimationFrame(() => {
        const m = move.current;
        move.current = null;
        if (m) send(mouse("mouseMoved", m.e, pagePoint(m.e, el.getBoundingClientRect(), size)));
      }),
    };
  }
  useEffect(() => () => { if (move.current) cancelAnimationFrame(move.current.frame); }, []);

  async function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    const k = e.nativeEvent;
    // The agent has the page; the user takes it over first (BRW-15).
    if (driving) {
      e.preventDefault();
      return;
    }
    // An input method is composing: its text comes whole, at the end.
    if (k.isComposing || k.keyCode === 229) return;
    const cmd = k.metaKey && !k.ctrlKey && !k.altKey;
    const letter = k.key.toLowerCase();
    // ⌘V is left to the window, whose paste event carries the text.
    if (cmd && letter === "v") return;
    e.preventDefault();
    if (cmd && letter === "l") return addressRef.current?.select();
    if (cmd && letter === "r") return void go("reload");
    if (cmd && k.key === "[") return void go("back");
    if (cmd && k.key === "]") return void go("forward");
    // The page's selection, onto the Mac's clipboard: a headless Chrome
    // copies to one of its own.
    if (cmd && (letter === "c" || letter === "x")) {
      try {
        const text = await api.browserCopy(taskId);
        if (text) await copyText(text);
      } catch (err) {
        fail(err);
      }
      if (letter === "c") return;
    }
    send(key("keyDown", k));
  }

  async function hold(held: boolean) {
    try {
      await api.browserHold(taskId, held);
      if (held) keysRef.current?.focus();
    } catch (e) {
      fail(e);
    }
  }

  /** The page's dialog answered (BRW-13). */
  async function answerDialog(accept: boolean) {
    try {
      await api.browserDialog(taskId, accept, view?.dialog?.kind === "prompt" ? promptText : undefined);
      keysRef.current?.focus();
    } catch (e) {
      fail(e);
    }
  }

  /** An agent's request answered (BRW-11). */
  async function answer(id: number, allow: boolean) {
    try {
      await api.browserAnswerSite(id, allow);
      if (allow) await refreshSettings();
    } catch (e) {
      fail(e);
    }
  }

  /** The page's site, allowed for agents from the panel itself (BRW-3). */
  async function allowHere(site: string) {
    try {
      const kept = await api.setBrowserSites([...(allowed ?? []), site]);
      await refreshSettings();
      // Said, not left silent: a click that kept nothing looked like a dead button.
      if (!kept.includes(site)) fail(`${site} is not a site agents can be allowed to use.`);
    } catch (e) {
      fail(e);
    }
  }

  async function go(to: "back" | "forward" | "reload") {
    if (drivingRef.current) return;
    try {
      await api.browserGo(taskId, to);
    } catch (e) {
      fail(e);
    }
  }

  async function open(e: React.FormEvent) {
    e.preventDefault();
    if (drivingRef.current) return;
    const url = address.trim();
    if (!url) return;
    addressRef.current?.blur();
    try {
      await api.browserOpen(taskId, url);
      keysRef.current?.focus();
    } catch (err) {
      fail(err);
    }
  }

  // Only a web page has a site to allow: Chrome's own pages have none.
  const [host, hostname] = (() => {
    try {
      const u = new URL(view?.url ?? "");
      return u.protocol === "http:" || u.protocol === "https:" ? [u.host, u.hostname] : ["", ""];
    } catch {
      return ["", ""];
    }
  })();

  return (
    <div className="browser">
      {/* The whole panel is the agent's while it uses the browser (BRW-15):
          tabs, address and navigation too, not only the page. */}
      <BrowserTabs
        tabs={view?.tabs ?? []}
        locked={driving}
        onSwitch={(id) => void api.browserSwitchTab(taskId, id).catch(fail)}
        onClose={(id) => void api.browserCloseTab(taskId, id).catch(fail)}
        onNew={() => {
          void api.browserNewTab(taskId).then(() => addressRef.current?.focus()).catch(fail);
        }}
      />
      <div className="browser-bar">
        <button className="btn btn-sm btn-icon" title="Back (⌘[)" onClick={() => void go("back")} disabled={!tab || driving}>
          <BackIcon />
        </button>
        <button className="btn btn-sm btn-icon" title="Forward (⌘])" onClick={() => void go("forward")} disabled={!tab || driving}>
          <ForwardIcon />
        </button>
        <button className="btn btn-sm btn-icon" title="Reload (⌘R)" onClick={() => void go("reload")} disabled={!tab || driving}>
          {view?.loading ? <Spinner /> : <ReloadIcon />}
        </button>
        <form className="browser-address" onSubmit={(e) => void open(e)}>
          <input
            disabled={driving}
            ref={addressRef}
            value={address}
            placeholder="localhost:3000, an address, or words to search for"
            spellCheck={false}
            onChange={(e) => setAddress(e.target.value)}
            onFocus={(e) => { setEditing(true); e.target.select(); }}
            onBlur={() => setEditing(false)}
            onKeyDown={(e) => { if (e.key === "Escape") { setEditing(false); keysRef.current?.focus(); } }}
          />
        </form>
        {view?.held && (
          <button
            className="btn btn-sm btn-primary"
            title="Let this task's agents use the browser again"
            onClick={() => void hold(false)}
          >
            Hand back
          </button>
        )}
        <button
          className="btn btn-sm btn-icon"
          title="Open this page in your own browser"
          disabled={!view?.url || view.url === "about:blank"}
          onClick={() => view && void openUrl(view.url)}
        >
          <ExternalIcon />
        </button>
        <button className="btn btn-sm btn-icon" title="Hide the browser" onClick={onClose}>
          <CloseIcon />
        </button>
      </div>

      {view?.requests.map((r) => (
        <div key={r.id} className="browser-ask">
          <span>
            <b>An agent asks to use {r.site}</b>
            {r.reason && <> — {r.reason}</>}
          </span>
          <div className="spacer" />
          <button className="btn btn-sm" onClick={() => void answer(r.id, false)}>No</button>
          <button className="btn btn-sm btn-primary" onClick={() => void answer(r.id, true)}>
            Allow {r.site}
          </button>
        </div>
      ))}

      {view?.dialog && (
        <form
          className="browser-ask browser-dialog"
          onSubmit={(e) => { e.preventDefault(); void answerDialog(true); }}
        >
          <span>
            <b>The page {view.dialog.kind === "alert" ? "says" : "asks"}:</b> {view.dialog.message}
          </span>
          {view.dialog.kind === "prompt" && (
            <input value={promptText} onChange={(e) => setPromptText(e.target.value)} autoFocus />
          )}
          <div className="spacer" />
          {view.dialog.kind !== "alert" && (
            <button type="button" className="btn btn-sm" onClick={() => void answerDialog(false)}>
              {view.dialog.kind === "beforeunload" ? "Stay" : "Cancel"}
            </button>
          )}
          <button className="btn btn-sm btn-primary">
            {view.dialog.kind === "beforeunload" ? "Leave" : "OK"}
          </button>
        </form>
      )}

      <div className={`browser-screen${focused ? " focused" : ""}`} ref={screenRef}>
        {view && !view.chrome ? (
          <div className="empty">
            <h2>No Chrome to run</h2>
            <p>
              The browser runs on Google Chrome (or Chromium), with a profile of its own:
              your Chrome's sign-ins and extensions stay out of it. Install Chrome, and it
              starts the next time it is needed.
            </p>
            <div className="row">
              <button className="btn" onClick={() => void openUrl("https://www.google.com/chrome/")}>
                Get Chrome <ExternalIcon />
              </button>
            </div>
          </div>
        ) : error ? (
          <div className="empty">
            <h2>The browser could not start</h2>
            <p>{error}</p>
            <div className="row">
              <button className="btn" onClick={() => setAttempt((n) => n + 1)}>Try again</button>
            </div>
          </div>
        ) : (
          <>
            <canvas
              ref={canvasRef}
              width={size ? Math.round(size.width * (window.devicePixelRatio || 1)) : 0}
              height={size ? Math.round(size.height * (window.devicePixelRatio || 1)) : 0}
              style={size ? { width: size.width, height: size.height } : undefined}
              onMouseDown={(e) => onMouse("mousePressed", e)}
              onMouseUp={(e) => onMouse("mouseReleased", e)}
              onMouseMove={(e) => onMouse("mouseMoved", e)}
              onContextMenu={(e) => e.preventDefault()}
            />
            {!tab && (
              <div className="browser-starting"><Spinner /> Starting the browser…</div>
            )}
            {driving && driver && (
              // Over the page and taking its clicks: the user's input would
              // land in the middle of the agent's.
              <div className="browser-driving" onMouseDown={(e) => e.preventDefault()}>
                <div className="browser-driving-card">
                  <span className="dot live" />
                  <span>
                    <b>{driver.agent}</b> is using this browser
                    {last && <span className="muted"> · {last.text}</span>}
                  </span>
                  <button
                    className="btn btn-sm btn-primary"
                    title="Keep agents out of this tab until you hand it back: to sign in, or to look at something"
                    onClick={() => void hold(true)}
                  >
                    Take over
                  </button>
                </div>
              </div>
            )}
            {marker && size && (
              <span
                key={marker.at}
                className="browser-marker"
                style={{ left: `${(marker.x / size.width) * 100}%`, top: `${(marker.y / size.height) * 100}%` }}
              />
            )}
          </>
        )}
        {/* Where the keys go while the page has them: an editable element, so
            the window's paste reaches it, out of sight. */}
        <textarea
          ref={keysRef}
          className="browser-keys"
          aria-label="Keys for the page"
          onKeyDown={(e) => void onKeyDown(e)}
          onKeyUp={(e) => {
            const k = e.nativeEvent;
            if (k.isComposing || (k.metaKey && k.key.toLowerCase() === "v")) return;
            e.preventDefault();
            send(key("keyUp", k));
          }}
          onPaste={(e) => {
            e.preventDefault();
            const text = e.clipboardData.getData("text/plain");
            if (text) send({ kind: "text", text });
          }}
          onCompositionEnd={(e) => {
            if (e.data) send({ kind: "text", text: e.data });
            e.currentTarget.value = "";
          }}
          onInput={(e) => {
            // Keys are sent as keys and never reach here; what does is text
            // that came another way: dictation, the emoji picker.
            const text = e.currentTarget.value;
            if ((e.nativeEvent as InputEvent).isComposing || !text) return;
            send({ kind: "text", text });
            e.currentTarget.value = "";
          }}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
        />
      </div>

      <div className="browser-status">
        {view?.held ? (
          <span className="browser-held">
            <b>You have the browser.</b> Agents wait until you hand it back.
          </span>
        ) : last ? (
          <span title={new Date(last.at).toLocaleString()}>
            <b>Agent</b> · {last.text} · {ago(last.at)}
          </span>
        ) : (
          <span className="muted">No agent has used this tab yet.</span>
        )}
        <div className="spacer" />
        {tab && host && !view?.agents_may && (
          <>
            <span className="browser-off" title="Agents use only this machine's pages and the sites you allowed (BRW-3)">
              Agents cannot use {host}
            </span>
            <button
              className="btn btn-sm"
              title={`Let agents use ${hostname} and its subdomains. Settings → Browser lists the sites allowed.`}
              onClick={() => void allowHere(hostname)}
            >
              Allow
            </button>
          </>
        )}
      </div>
    </div>
  );
}
