import { useCallback, useRef, useState, type RefObject } from "react";
import { api } from "../lib/api";
import { goTo } from "../lib/goto";
import { ago } from "../lib/time";
import type { Message } from "../lib/types";
import { useNow, useStore } from "../store";
import { BellIcon, Confirm, Floating } from "./ui";

/**
 * The message center (MSG-5): the bell left of Settings, with the unread
 * count, and the list of what the app told you, newest first.
 */
export function MessageCenter() {
  const unread = useStore((s) => s.messages.reduce((n, m) => n + (m.read ? 0 : 1), 0));
  const fail = useStore((s) => s.fail);
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  // Asked from the panel, which closes for it: the panel floats over dialogs.
  const [clearing, setClearing] = useState(false);
  const bell = useRef<HTMLButtonElement>(null);
  const close = useCallback(() => setAt(null), []);

  function toggle() {
    if (at) return close();
    const r = bell.current?.getBoundingClientRect();
    if (r) setAt({ x: r.right, y: r.bottom + 6 });
  }

  return (
    <>
      <button
        ref={bell}
        className={`icon-btn bell${at ? " open" : ""}`}
        title={unread ? `Messages — ${unread} unread` : "Messages"}
        onClick={toggle}
      >
        <BellIcon />
        {unread > 0 && <span className="bell-count">{unread > 99 ? "99+" : unread}</span>}
      </button>
      {at && (
        <MessageList
          x={at.x}
          y={at.y}
          ignore={bell}
          onClose={close}
          onClear={() => { close(); setClearing(true); }}
        />
      )}
      {clearing && (
        <Confirm
          title="Clear every message?"
          body="The list empties for good. What comes next still lands here."
          confirmLabel="Clear"
          onConfirm={() => api.clearMessages().catch(fail)}
          onCancel={() => setClearing(false)}
        />
      )}
    </>
  );
}

function MessageList({
  x, y, ignore, onClose, onClear,
}: {
  x: number;
  y: number;
  ignore: RefObject<HTMLButtonElement | null>;
  onClose: () => void;
  onClear: () => void;
}) {
  const messages = useStore((s) => s.messages);
  const fail = useStore((s) => s.fail);
  const now = useNow(30_000);
  const unread = messages.some((m) => !m.read);

  function open(m: Message) {
    if (m.target) {
      // Marks this and every other unread one about the same thing.
      goTo(m.target);
      onClose();
    } else if (!m.read) {
      void api.markMessagesRead([m.id]).catch(fail);
    }
  }

  return (
    <Floating
      x={x}
      y={y}
      align="end"
      onClose={onClose}
      ignore={ignore}
      measureKey={messages.length}
      className="messages"
    >
      <div className="messages-head">
        <span className="title">Messages</span>
        <span className="spacer" />
        <button
          type="button"
          className="btn btn-sm"
          disabled={!unread}
          onClick={() => void api.markMessagesRead().catch(fail)}
        >
          Mark all read
        </button>
        <button
          type="button"
          className="btn btn-sm"
          disabled={messages.length === 0}
          onClick={onClear}
        >
          Clear
        </button>
      </div>
      <div className="messages-list">
        {messages.length === 0 && (
          <p className="messages-empty">
            Nothing yet. Agents that finish or need you, reviews, tickets and
            errors land here.
          </p>
        )}
        {messages.map((m) => (
          <button
            key={m.id}
            type="button"
            className={`message ${m.level}${m.read ? "" : " unread"}${m.target ? " linked" : ""}${m.body ? "" : " solo"}`}
            title={m.target ? "Open" : undefined}
            onClick={() => open(m)}
          >
            <span className="dot" />
            <span className="text">
              <span className="line">
                <span className="what">{m.title}</span>
                {m.count > 1 && <span className="times">×{m.count}</span>}
                <span className="when">{ago(m.at, now)}</span>
              </span>
              {m.body && <span className="body">{m.body}</span>}
            </span>
          </button>
        ))}
      </div>
    </Floating>
  );
}
