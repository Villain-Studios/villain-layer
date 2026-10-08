import { listen } from "@tauri-apps/api/event";

/**
 * One Tauri listener for every conversation pane (§20), fanned out by pane
 * id, as `ptyOutput.ts` does for terminals. The event says only that a
 * conversation changed; the pane asks for what (ACP-7).
 */

const handlers = new Map<string, () => void>();
let started = false;

function ensureListening() {
  if (started) return;
  started = true;
  void listen<string>("acp:update", (e) => {
    handlers.get(e.payload)?.();
  });
}

export function onAcpUpdate(paneId: string, handler: () => void): () => void {
  handlers.set(paneId, handler);
  ensureListening();
  return () => {
    if (handlers.get(paneId) === handler) handlers.delete(paneId);
  };
}
