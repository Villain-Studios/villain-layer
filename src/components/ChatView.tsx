import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/api";
import { useFocusedPane } from "../lib/goto";
import { read, write } from "../lib/persist";
import { useStore } from "../store";
import { CHAT_TASK_ID, paneName, paneState } from "../lib/derive";
import { TerminalPane } from "./Terminal";
import { PlusIcon } from "./icons";
import { ContextMenu, Confirm } from "./ui";
import type { MenuItem } from "./ui";

function started(unix: string): string {
  const d = new Date(unix);
  return Number.isNaN(d.getTime())
    ? ""
    : d.toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

/**
 * A standing agent with no worktree: for questions, drafting tickets, and
 * pulling context from whatever MCP servers the user's own CLI is configured
 * with. It runs in a scratch folder so it can keep notes between sessions.
 *
 * Conversations live down the left rather than in a tab strip: they are long
 * lived and there is no obvious limit on how many you might keep, which is the
 * case tabs handle worst.
 */
export function ChatView() {
  const allPanes = useStore((s) => s.panes);
  const panes = useMemo(
    () => allPanes.filter((p) => p.task_id === CHAT_TASK_ID),
    [allPanes],
  );
  const agents = useStore((s) => s.agents);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const fail = useStore((s) => s.fail);

  const [active, setActive] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [closing, setClosing] = useState<string | null>(null);
  const addRef = useRef<HTMLButtonElement>(null);
  const installed = agents.filter((a) => a.installed);

  // Chats are given fresh ids every launch, so what survives a restart is the
  // position in the list, not the identity of the pane.
  useEffect(() => {
    if (panes.length === 0) { setActive(null); return; }
    if (!active || !panes.some((p) => p.id === active)) {
      const want = read<number>("activeChat", panes.length - 1);
      const i = Number.isInteger(want) && want >= 0 && want < panes.length
        ? want
        : panes.length - 1;
      setActive(panes[i].id);
    }
  }, [panes, active]);

  useFocusedPane(panes, setActive);

  useEffect(() => {
    const i = panes.findIndex((p) => p.id === active);
    if (i >= 0) write("activeChat", i);
  }, [active, panes]);

  // Ctrl+Tab moves between chats, as it does between terminals.
  useEffect(() => {
    if (panes.length < 2) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab" || !e.ctrlKey || e.metaKey || e.altKey) return;
      e.preventDefault();
      e.stopPropagation();
      const at = panes.findIndex((p) => p.id === active);
      const from = at < 0 ? 0 : at;
      setActive(panes[(from + (e.shiftKey ? -1 : 1) + panes.length) % panes.length].id);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [panes, active]);

  // One start at a time: a second click while the first was spawning
  // opened a second chat.
  const starting = useRef(false);
  async function start(agentId: string) {
    if (starting.current) return;
    starting.current = true;
    try {
      const pane = await api.spawnChat(agentId);
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    } finally {
      starting.current = false;
    }
  }

  async function close(id: string) {
    try {
      await api.closePane(id);
      await refreshPanes();
    } catch (e) {
      fail(e);
    }
  }

  const addItems: MenuItem[] = installed.length
    ? installed.map((a) => ({ label: a.name, onSelect: () => void start(a.id) }))
    : [{ label: "No agent CLIs on your PATH", onSelect: () => {}, disabled: true }];

  const doomed = panes.find((p) => p.id === closing);

  return (
    <div className="chat">
      <div className="chat-list">
        <div className="chat-list-head">
          Chats
          <div className="spacer" />
          <button
            ref={addRef}
            className="btn btn-sm btn-add"
            title="New chat"
            onClick={() =>
              setMenu((open) => {
                if (open) return null;
                const r = addRef.current?.getBoundingClientRect();
                return r ? { x: r.right, y: r.bottom + 4 } : null;
              })
            }
          >
            <PlusIcon />
          </button>
        </div>

        <div className="chat-list-body">
          {panes.map((p) => (
            <div
              key={p.id}
              className={`chat-item${p.id === active ? " active" : ""}`}
              onClick={() => setActive(p.id)}
            >
              <span className={`dot ${paneState(p).dot}`} title={paneState(p).label} />
              <span className="chat-item-text">
                <span className="chat-item-title" title={paneName(p)}>{paneName(p)}</span>
                <span className="chat-item-sub">
                  {p.topic && `${agents.find((a) => a.id === p.agent_id)?.name ?? p.title} · `}
                  {p.running ? started(p.started_at) : "ended"}
                </span>
              </span>
              <span
                className="x"
                title="Close this chat"
                onClick={(e) => { e.stopPropagation(); setClosing(p.id); }}
              >
                ✕
              </span>
            </div>
          ))}
          {panes.length === 0 && (
            <div className="chat-list-empty">No chats open.</div>
          )}
        </div>
      </div>

      <div className="pane-stack">
        {panes.map((p) => (
          <TerminalPane key={p.id} pane={p} visible={p.id === active} />
        ))}
        {panes.length === 0 && (
          <div className="empty">
            <h2>Chat</h2>
            <p>
              An agent with no worktree attached — for asking questions, drafting a
              ticket before it exists, or pulling context together from elsewhere.
            </p>
            <p style={{ color: "var(--dimmer)" }}>
              It starts with a <code>CLAUDE.md</code> describing your repos, the tasks
              in flight and which integrations the app is connected to — so it knows
              what you are working on without being told.
            </p>
            <p style={{ color: "var(--dimmer)" }}>
              It also reaches Jira, Slack and GitHub through this app's own
              connections, so it can search tickets, file one, or start work on it
              without you configuring anything separately.
            </p>
            <div className="row">
              {installed.length === 0 && <span>No agent CLIs found on your PATH.</span>}
              {installed.map((a) => (
                <button key={a.id} className="btn btn-primary" onClick={() => void start(a.id)}>
                  Start {a.name}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          ignore={addRef}
          items={addItems}
          onClose={() => setMenu(null)}
        />
      )}

      {doomed && (
        <Confirm
          title="Close this chat?"
          confirmLabel="Close"
          body={
            <>
              <b>{doomed.title}</b> will be stopped and its scrollback discarded.
              {doomed.running && " Anything it is part-way through is lost."}
            </>
          }
          onCancel={() => setClosing(null)}
          // Returned, not fired and forgotten: stopping a chat agent gives it
          // five seconds to save, and the dialog now waits that out instead of
          // vanishing while the tab stays put.
          busyLabel="Closing…"
          onConfirm={() => close(doomed.id)}
        />
      )}
    </div>
  );
}
