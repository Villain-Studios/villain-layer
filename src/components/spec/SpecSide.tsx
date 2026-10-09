import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { useSpecs } from "../../lib/specs";
import { useStore } from "../../store";
import type { RepoSpec, TaskView } from "../../lib/types";
import { Spinner } from "../ui";

const VERDICT_MARK: Record<string, string> = { met: "✓", "not met": "✗", unclear: "?" };

/**
 * Beside the editor, for one repository: its approved requirements with the
 * last check's answers (SPEC-15), how far its tasks are (SPEC-8), and Start
 * agent, which sets an agent to work through them (SPEC-12).
 */
export function SpecSide({ task, repo, repos, agentId, setAgentId }: {
  task: TaskView;
  repo: RepoSpec;
  repos: RepoSpec[];
  agentId: string | null;
  setAgentId: (id: string) => void;
}) {
  const checking = useSpecs((s) => s.byTask[task.id]?.checking.includes(repo.checkout_id) ?? false);
  const check = useSpecs((s) => s.check);
  const agents = useStore((s) => s.agents);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const showPane = useStore((s) => s.showPane);
  const setTab = useStore((s) => s.setTab);
  const fail = useStore((s) => s.fail);

  const installed = agents.filter((a) => a.installed);
  const firstInstalled = installed[0]?.id;
  useEffect(() => {
    if (!agentId && firstInstalled) setAgentId(firstInstalled);
  }, [agentId, firstInstalled, setAgentId]);

  /** Every repository from the task folder, or this one in its own. */
  const [scope, setScope] = useState<"all" | "this">("all");
  const inScope = scope === "all" || repos.length === 1 ? repos : [repo];
  const ready = inScope.some((r) => r.steps.length > 0);
  const [starting, setStarting] = useState(false);

  async function start() {
    if (!agentId) return;
    setStarting(true);
    try {
      const checkoutId = scope === "this" && repos.length > 1 ? repo.checkout_id : null;
      const [prompt, work] = await Promise.all([api.taskPrompt(task.id, checkoutId), api.specWorkPrompt(task.id, checkoutId)]);
      const pane = await api.spawnAgent(task.id, agentId, checkoutId, [prompt, work].filter(Boolean).join("\n\n"));
      await refreshPanes();
      showPane(pane.id);
      setTab("terminals");
    } catch (e) {
      fail(e);
    } finally {
      setStarting(false);
    }
  }

  const verdict = (id: string) => repo.check?.results.find((v) => v.id === id);
  const done = repo.steps.filter((s) => s.done).length;

  return (
    <aside className="spec-side">
      <section>
        <h3>{repo.kind === "bugfix" ? "Expected and unchanged" : "Requirements"}</h3>
        {repo.requirements.length === 0 && repo.parts[0].approved ? (
          <p className="spec-stale">
            No requirements found in what was approved. List them under a Requirements heading,
            each starting R-1, R-2: they are what done means.
          </p>
        ) : repo.requirements.length === 0 ? (
          <p className="muted">
            None approved yet. They are what done means: each one a WHEN … THE SYSTEM SHALL … under a
            Requirements heading.
          </p>
        ) : (
          <ol className="spec-reqs">
            {repo.requirements.map((r) => {
              const v = verdict(r.id);
              return (
                <li key={r.id} title={v?.evidence || undefined}>
                  <b>{r.id}</b>
                  {v && <span className={`spec-verdict ${v.verdict.replace(" ", "-")}`}>{VERDICT_MARK[v.verdict]} {v.verdict}</span>}
                  <span>{r.text}</span>
                </li>
              );
            })}
          </ol>
        )}
        <div className="row spec-check">
          <button
            className="btn btn-sm"
            disabled={repo.requirements.length === 0 || checking}
            title="Read this repository's changes on the branch against its requirements"
            onClick={() => void check(task.id, repo.checkout_id).catch(fail)}
          >
            Check against spec
          </button>
          {checking && <Spinner />}
          {repo.check && !checking && (
            <span className={`muted${repo.check.stale ? " spec-stale" : ""}`}>
              {repo.check.stale ? "Checked before the latest commits" : `Checked at ${repo.check.sha.slice(0, 7)}`}
            </span>
          )}
        </div>
      </section>

      <section>
        <h3>Tasks{repo.steps.length > 0 && <span className="muted"> · {done} of {repo.steps.length} done</span>}</h3>
        {repo.steps.length === 0 ? (
          <p className="muted">None approved yet.</p>
        ) : (
          <ul className="spec-tasks">
            {repo.steps.map((s) => (
              <li key={s.number} className={s.done ? "done" : ""}>
                <span className="mark">{s.done ? "✓" : "○"}</span>
                <span>{s.number}. {s.text}</span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="spec-startbox">
        <h3>Start an agent on it</h3>
        <select value={agentId ?? ""} disabled={installed.length === 0} onChange={(e) => setAgentId(e.target.value)}>
          {installed.length === 0 && <option value="">No agent CLI installed</option>}
          {installed.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
        </select>
        {repos.length > 1 && (
          <select value={scope} onChange={(e) => setScope(e.target.value as "all" | "this")}>
            <option value="all">Every repository, from the task folder</option>
            <option value="this">Only {repo.folder}</option>
          </select>
        )}
        <button
          className="btn btn-sm btn-primary"
          disabled={!agentId || !ready || starting}
          title={ready ? "It works through the approved tasks in order, ticking each off" : "Approve the tasks first"}
          onClick={() => void start()}
        >
          {starting ? "Starting…" : "Start agent"}
        </button>
      </section>
    </aside>
  );
}
