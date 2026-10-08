import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../lib/api";
import { paneName } from "../lib/derive";
import { read, write } from "../lib/persist";
import { useStore } from "../store";
import type { AcpSetting, LoopCheckRun, LoopView, PaneInfo, TaskView } from "../lib/types";
import { LoopIcon } from "./icons";
import { Field, Modal, Spinner } from "./ui";

/**
 * An agent on a loop (§21): the button in the pane bar that starts one, the
 * bar under it that shows how it goes (LOOP-9), and the dialog between.
 */

const ENDED = new Set(["passed", "gave_up", "stopped"]);

/** A pane's loop, kept current by `loop:changed`, never polled (LOOP-9). */
function useLoop(paneId: string | undefined): LoopView | null {
  const [view, setView] = useState<LoopView | null>(null);
  useEffect(() => {
    setView(null);
    if (!paneId) return;
    let current = true;
    const load = () =>
      api.loopView(paneId).then((v) => { if (current) setView(v); }).catch(() => {});
    void load();
    const p = listen<string>("loop:changed", (e) => {
      if (e.payload === paneId) void load();
    });
    return () => {
      current = false;
      void p.then((un) => un());
    };
  }, [paneId]);
  return view;
}

/** The repositories a pane works in (LOOP-2): its own, or every one of the task. */
function reposOf(pane: PaneInfo, task: TaskView) {
  return task.checkouts.filter((c) => !pane.checkout_id || c.id === pane.checkout_id);
}

/** Start a loop on the agent in front: offered while it runs and is on none. */
export function LoopButton({ pane, task }: { pane: PaneInfo | undefined; task: TaskView }) {
  const view = useLoop(pane?.id);
  const [asking, setAsking] = useState(false);
  if (!pane || pane.kind !== "agent" || !pane.running) return null;
  if (view && !ENDED.has(view.phase)) return null;
  return (
    <>
      <button
        className="btn btn-sm btn-add"
        title={`Put ${paneName(pane)} on a loop: its checks run each time it ends a turn`}
        onClick={() => setAsking(true)}
      >
        <LoopIcon />
      </button>
      {asking && <StartLoop pane={pane} task={task} onClose={() => setAsking(false)} />}
    </>
  );
}

function StartLoop({ pane, task, onClose }: { pane: PaneInfo; task: TaskView; onClose: () => void }) {
  const projects = useStore((s) => s.projects);
  const refreshRepos = useStore((s) => s.refreshRepos);
  const fail = useStore((s) => s.fail);
  const repos = reposOf(pane, task);
  const saved = (projectId: string) => projects.find((p) => p.id === projectId)?.check ?? "";
  const [commands, setCommands] = useState<Record<string, string>>(() =>
    Object.fromEntries(repos.map((c) => [c.project_id, saved(c.project_id)])),
  );
  const [rounds, setRounds] = useState(() => read<number>("loopRounds", 5));
  const [busy, setBusy] = useState(false);
  const once = useRef(false);
  const any = repos.some((c) => commands[c.project_id]?.trim());
  // An agent over ACP says what modes it has (ACP-12). One that asks
  // before it runs anything holds the loop at every command: Claude Code
  // in its default mode stopped at `ls`. Offered here, and changed only if
  // the user changes it.
  const [mode, setMode] = useState<AcpSetting | null>(null);
  const [modeValue, setModeValue] = useState("");
  useEffect(() => {
    if (!pane.acp) return;
    let current = true;
    api.acpView(pane.id, null)
      .then((v) => {
        const m = v.settings.find((s) => s.category === "mode") ?? null;
        if (!current || !m) return;
        setMode(m);
        setModeValue(m.current);
      })
      .catch(() => {});
    return () => { current = false; };
  }, [pane.id, pane.acp]);

  async function start() {
    if (once.current || !any) return;
    once.current = true;
    setBusy(true);
    try {
      // A command typed here is the repository's own from now on (LOOP-1).
      for (const c of repos) {
        const typed = commands[c.project_id]?.trim() ?? "";
        if (typed !== saved(c.project_id).trim()) await api.setProjectCheck(c.project_id, typed || null);
      }
      await refreshRepos();
      if (mode && modeValue !== mode.current) await api.acpSet(pane.id, mode.id, modeValue);
      const n = Math.min(20, Math.max(1, Math.round(rounds) || 5));
      write("loopRounds", n);
      await api.startLoop(pane.id, n);
      onClose();
    } catch (e) {
      fail(e);
    } finally {
      once.current = false;
      setBusy(false);
    }
  }

  return (
    <Modal
      title={`Put ${paneName(pane)} on a loop`}
      onClose={() => { if (!busy) onClose(); }}
      footer={
        <>
          <button className="btn" disabled={busy} onClick={onClose}>Cancel</button>
          <button className="btn btn-primary" disabled={busy || !any} onClick={() => void start()}>
            {busy ? "Starting…" : "Start loop"}
          </button>
        </>
      }
    >
      <p className="loop-intro">
        Each time the agent ends a turn, these run in its worktrees. When one fails, the agent is
        sent what failed and goes on. When they all pass, or the rounds are used, the loop stops
        and tells you. A turn that changes nothing waits for you.
      </p>
      <p className="loop-intro">
        An agent that stops to ask permission holds the loop until you answer. To leave it alone,
        give it a mode that does not ask first{mode ? ", below" : pane.acp ? "" : ", in the agent itself"}.
      </p>
      {repos.map((c) => (
        <Field
          key={c.id}
          label={`Check command · ${c.project_name}`}
          hint="Passes when it exits 0. Kept for the repository; also in Repos."
        >
          <input
            className="mono"
            value={commands[c.project_id] ?? ""}
            placeholder="not checked (cargo test, bun run check…)"
            spellCheck={false}
            onChange={(e) => setCommands((m) => ({ ...m, [c.project_id]: e.target.value }))}
          />
        </Field>
      ))}
      {mode && (
        <Field
          label={`The agent's ${mode.name.toLowerCase()}`}
          hint={mode.options.find((o) => o.value === modeValue)?.description ?? "The agent's own setting, as under its conversation."}
        >
          <select value={modeValue} onChange={(e) => setModeValue(e.target.value)}>
            {mode.options.map((o) => <option key={o.value} value={o.value}>{o.name}</option>)}
          </select>
        </Field>
      )}
      <Field label="Rounds" hint="The most times failures go back to the agent before the loop gives up (1–20).">
        <input
          type="number"
          min={1}
          max={20}
          value={rounds}
          onChange={(e) => setRounds(Number(e.target.value))}
          style={{ width: 80 }}
        />
      </Field>
    </Modal>
  );
}

function what(view: LoopView, agent: string): string {
  const used = `${view.round} of ${view.rounds} rounds used`;
  switch (view.phase) {
    case "waiting":
      return `Checks run when ${agent} ends its turn · ${used}`;
    case "queued":
      return `Waiting for other loops' checks to finish · ${used}`;
    case "checking":
      return `Checking ${view.checking ?? ""}… · ${used}`;
    default:
      return view.note ?? "";
  }
}

/** The pane's loop, under the pane bar (LOOP-9). */
export function LoopBar({ pane }: { pane: PaneInfo | undefined }) {
  const view = useLoop(pane?.id);
  const fail = useStore((s) => s.fail);
  const [open, setOpen] = useState(false);
  /** The loop whose ended bar was put away, by when it started. */
  const [dismissed, setDismissed] = useState<string | null>(null);
  if (!pane || !view || dismissed === view.started_at) return null;
  const ended = ENDED.has(view.phase);
  const tone = view.phase === "passed" ? "good" : view.phase === "gave_up" ? "bad" : view.phase === "held" ? "held" : "";
  return (
    <div className={`loop-bar ${tone}`}>
      <div className="loop-line">
        <LoopIcon size={14} />
        <b>Loop</b>
        {view.phase === "checking" && <Spinner />}
        <span className="loop-what" title={what(view, paneName(pane))}>{what(view, paneName(pane))}</span>
        <div className="spacer" />
        {view.runs.length > 0 && (
          <button className="btn btn-sm" onClick={() => setOpen((o) => !o)}>
            {open ? "Hide checks" : `Checks (${view.runs.length})`}
          </button>
        )}
        {!ended && (
          <button className="btn btn-sm" onClick={() => void api.stopLoop(pane.id).catch(fail)}>Stop</button>
        )}
        {ended && pane.running && (
          <button className="btn btn-sm" onClick={() => void api.startLoop(pane.id, view.rounds).catch(fail)}>
            Start again
          </button>
        )}
        {ended && (
          <button className="btn btn-sm" title="Put this away" onClick={() => setDismissed(view.started_at)}>
            Done
          </button>
        )}
      </div>
      {open && (
        <div className="loop-runs">
          {view.runs.map((run, i) => <Run key={run.at} run={run} n={view.runs.length - i} />)}
        </div>
      )}
    </div>
  );
}

function Run({ run, n }: { run: LoopCheckRun; n: number }) {
  const [shown, setShown] = useState<string | null>(null);
  const at = new Date(run.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  return (
    <div className="loop-run">
      <div className="loop-run-head">
        Check {n} · {at} · {run.round === 0 ? "before any round" : `after round ${run.round}`}
      </div>
      {run.results.map((r) => (
        <div key={r.repo}>
          <div
            className={`loop-result ${r.passed ? "pass" : "fail"}`}
            onClick={() => setShown((s) => (s === r.repo ? null : r.repo))}
          >
            <span className="mark">{r.passed ? "✓" : "✗"}</span>
            <span>{r.repo}/</span>
            <code>{r.command}</code>
            <span className="dim">
              {r.passed ? "passed" : r.code === null ? "stopped" : `exited ${r.code}`} · {r.secs.toFixed(1)}s
            </span>
          </div>
          {shown === r.repo && <pre className="loop-output">{r.output || "(no output)"}</pre>}
        </div>
      ))}
    </div>
  );
}
