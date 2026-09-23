import { useEffect, useState } from "react";
import { api, errMessage } from "../lib/api";
import type { Cleaned, CleanupItem } from "../lib/types";
import { useStore } from "../store";
import { Modal, Spinner } from "./ui";

const SECTIONS: { kind: CleanupItem["kind"]; title: string }[] = [
  { kind: "folder", title: "Task folders no task uses" },
  { kind: "worktree", title: "Worktrees in a task's folder that are none of its repos" },
  { kind: "clone_branch", title: "Task branches left in your clones" },
  { kind: "store_branch", title: "Branches in the app's copies that no task uses" },
  { kind: "records", title: "Git's records of deleted worktrees" },
  { kind: "store", title: "Copies no repository uses" },
];

/**
 * Clean up (REPO-8): what tasks left behind, each with why, before anything
 * goes. Only what loses nothing starts ticked, and the backend judges every
 * item again as it removes it, so a list left open for an hour cannot take
 * what changed in the meantime.
 */
export function CleanUp({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const fail = useStore((s) => s.fail);
  const [items, setItems] = useState<CleanupItem[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [running, setRunning] = useState(false);
  const [results, setResults] = useState<Record<string, Cleaned> | null>(null);
  // A look that failed is not an empty list: saying "nothing to clean up"
  // after an error is the all-clear nobody checked.
  const [broken, setBroken] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api.cleanupPlan().then(
      (found) => {
        if (!live) return;
        setItems(found);
        setPicked(new Set(found.filter((i) => i.verdict === "safe").map((i) => i.id)));
      },
      (e) => {
        if (live) setBroken(errMessage(e));
      },
    );
    return () => { live = false; };
  }, []);

  function toggle(id: string, on: boolean) {
    setPicked((s) => {
      const next = new Set(s);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  }

  async function run() {
    setRunning(true);
    try {
      const done = await api.cleanupApply([...picked]);
      setResults(Object.fromEntries(done.map((d) => [d.id, d])));
      onDone();
    } catch (e) {
      fail(e);
    } finally {
      setRunning(false);
    }
  }

  const removed = results ? Object.values(results).filter((r) => r.ok).length : 0;
  const footer = results ? (
    <>
      <span className="muted">
        Removed {removed} of {Object.keys(results).length}.
      </span>
      <div className="spacer" />
      <button className="btn btn-primary" onClick={onClose}>Done</button>
    </>
  ) : (
    <>
      <button className="btn" disabled={running} onClick={onClose}>Cancel</button>
      <button className="btn btn-danger-solid" disabled={running || picked.size === 0} onClick={() => void run()}>
        {running ? <span className="btn-busy"><Spinner />Removing…</span> : `Remove ${picked.size}`}
      </button>
    </>
  );

  return (
    <Modal
      title="Clean up"
      wide
      tall
      // Not dismissable mid-flight: closing would only hide that it runs.
      onClose={() => { if (!running) onClose(); }}
      footer={items && items.length > 0 ? footer : undefined}
    >
      {broken ? (
        <div className="repo-problem">Could not look through the task folders and copies: {broken}</div>
      ) : items === null ? (
        <div className="muted"><span className="btn-busy"><Spinner />Looking through task folders and copies…</span></div>
      ) : items.length === 0 ? (
        <div className="muted">Nothing to clean up: every folder, branch and copy is in use.</div>
      ) : (
        <>
          <div className="cleanup-intro">
            Nothing goes until you press Remove. Ticked: loses nothing. Amber: read
            why first. Greyed: not something the app can remove for you.
          </div>
          {SECTIONS.map(({ kind, title }) => {
            const rows = items.filter((i) => i.kind === kind);
            if (rows.length === 0) return null;
            return (
              <div key={kind} className="cleanup-section">
                <h4>{title}</h4>
                {rows.map((i) => {
                  const result = results?.[i.id];
                  return (
                    <label key={i.id} className={`cleanup-item ${i.verdict}`}>
                      <input
                        type="checkbox"
                        disabled={i.verdict === "blocked" || running || results !== null}
                        checked={picked.has(i.id)}
                        onChange={(e) => toggle(i.id, e.target.checked)}
                      />
                      <div className="cleanup-text">
                        <div className="cleanup-title">
                          {/* A name to recognise, except the records, which have none. */}
                          {i.kind === "records" ? <span>{i.title}</span> : <code>{i.title}</code>}
                          {i.repo && <span className="chip">{i.repo}</span>}
                          {result && (result.ok
                            ? <span className="chip add">removed</span>
                            : <span className="chip del">not removed</span>)}
                        </div>
                        <div className="cleanup-detail">{result && !result.ok ? result.detail : i.detail}</div>
                      </div>
                    </label>
                  );
                })}
              </div>
            );
          })}
        </>
      )}
    </Modal>
  );
}
