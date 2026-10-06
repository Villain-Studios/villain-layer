import { useState } from "react";
import type { Group, Overview as Data, PhonePane } from "./types";

const OPEN = "villain.phone.open";

/** The tasks opened on this phone, remembered across visits. */
function useOpen(): [Set<string>, (id: string) => void] {
  const [open, setOpen] = useState<Set<string>>(() => {
    try {
      return new Set(JSON.parse(localStorage.getItem(OPEN) ?? "[]") as string[]);
    } catch {
      return new Set();
    }
  });
  const toggle = (id: string) =>
    setOpen((was) => {
      const next = new Set(was);
      if (!next.delete(id)) next.add(id);
      try {
        localStorage.setItem(OPEN, JSON.stringify([...next]));
      } catch {
        // Remembered until the page closes, then.
      }
      return next;
    });
  return [open, toggle];
}

/** The dot and the words for a pane's state, as the window's `paneState` has them. */
export function stateOf(p: PhonePane): { label: string; dot: string } {
  if (!p.running) return { label: "exited", dot: "" };
  if (p.waiting) return { label: p.waiting.replace(/^is /, ""), dot: p.notice === "usage_limit" ? "gone" : "idle" };
  if (p.kind === "shell") return { label: "shell", dot: "live" };
  if (p.activity === "working") return { label: "working", dot: "live" };
  return { label: p.activity === "done" ? "your turn" : "idle", dot: "" };
}

/** A task's name without its ticket key, which is shown beside it already. */
function bare(g: Group): string {
  const name = g.key && g.name.startsWith(g.key) ? g.name.slice(g.key.length).replace(/^[\s:·-]+/, "") : g.name;
  return name || g.name;
}

function since(iso: string): string {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86_400)}d`;
}

function PaneRow({ pane, onOpen }: { pane: PhonePane; onOpen: (id: string) => void }) {
  const st = stateOf(pane);
  return (
    <button className={`pane-row${pane.waiting ? " waiting" : ""}`} onClick={() => onOpen(pane.id)}>
      <span className={`dot ${st.dot}`} />
      <span className="pane-text">
        <span className="pane-name">{pane.name}</span>
        <span className="pane-state">{st.label}</span>
      </span>
      <span className="pane-since">{since(pane.since)}</span>
    </button>
  );
}

/**
 * A task, closed until tapped (PHONE-5). Closed, it still says what its
 * agents are doing: a dot each, and whether one needs you.
 */
function GroupCard({
  group, open, onToggle, onOpen,
}: {
  group: Group;
  open: boolean;
  onToggle: () => void;
  onOpen: (id: string) => void;
}) {
  const waiting = group.panes.filter((p) => p.waiting).length;
  const working = group.panes.filter((p) => p.running && p.activity === "working").length;
  const summary = waiting ? `${waiting} need${waiting === 1 ? "s" : ""} you` : working ? `${working} working` : "";
  return (
    <section className={`group${waiting ? " waiting" : ""}`}>
      <button className="group-head" aria-expanded={open} onClick={onToggle}>
        <span className={`chev${open ? " open" : ""}`}>›</span>
        <span className="group-title">
          <span className="group-name">
            {group.key && <span className="key">{group.key}</span>} {bare(group)}
          </span>
          <span className="group-sum">
            {group.panes.map((p) => (
              <span key={p.id} className={`dot ${stateOf(p).dot}`} />
            ))}
            {summary && <span className={waiting ? "needs" : ""}>{summary}</span>}
          </span>
        </span>
      </button>
      {open && group.panes.map((p) => <PaneRow key={p.id} pane={p} onOpen={onOpen} />)}
    </section>
  );
}

/** Every task and its agents, the ones that need you first (PHONE-5). */
export function Overview({
  data, live, error, onOpen,
}: {
  data: Data | null;
  live: boolean;
  error: string | null;
  onOpen: (id: string) => void;
}) {
  const withPanes = data?.groups.filter((g) => g.panes.length > 0) ?? [];
  const quiet = data?.groups.filter((g) => g.panes.length === 0) ?? [];
  const waiting = withPanes.flatMap((g) => g.panes).filter((p) => p.waiting).length;
  const [open, toggle] = useOpen();
  return (
    <div className="screen">
      <header className="bar">
        <div className="brand">villain<span>·</span>layer</div>
        <div className="spacer" />
        <span className={`dot ${live ? "live" : "gone"}`} title={live ? "Connected to the Mac" : "Not connected"} />
      </header>
      <main className="scroll">
        {error && <div className="error">{error}</div>}
        {!data && !error && <div className="muted pad">Asking the Mac…</div>}
        {data && (
          <div className="summary">
            {waiting > 0 ? `${waiting} need${waiting === 1 ? "s" : ""} you` : "Nothing needs you"}
          </div>
        )}
        {withPanes.map((g) => (
          <GroupCard key={g.id} group={g} open={open.has(g.id)} onToggle={() => toggle(g.id)} onOpen={onOpen} />
        ))}
        {data && withPanes.length === 0 && <div className="muted pad">No agents or terminals are open on the Mac.</div>}
        {quiet.length > 0 && (
          <section className="group quiet">
            <h2>No terminals open</h2>
            {quiet.map((g) => (
              <div key={g.id} className="quiet-task">
                {g.key && <span className="key">{g.key}</span>} {bare(g)}
              </div>
            ))}
          </section>
        )}
      </main>
    </div>
  );
}
