import { listen } from "@tauri-apps/api/event";

/**
 * One Tauri listener for all terminal panes.
 *
 * Each TerminalPane used to subscribe to `pty:output` on its own, so N open
 * panes meant every chunk was decoded N times just to discard N−1 of them.
 * One subscription fans out by pane id instead.
 */

/** `data` is base64; `end` is where it finishes in the pane's output. */
type Handler = (data: string, end: number) => void;

const handlers = new Map<string, Handler>();
let started = false;

function ensureListening() {
  if (started) return;
  started = true;
  void listen<{ pane_id: string; data: string; end: number }>("pty:output", (e) => {
    handlers.get(e.payload.pane_id)?.(e.payload.data, e.payload.end);
  });
}

export function onPtyOutput(paneId: string, handler: Handler): () => void {
  handlers.set(paneId, handler);
  ensureListening();
  return () => {
    if (handlers.get(paneId) === handler) handlers.delete(paneId);
  };
}
