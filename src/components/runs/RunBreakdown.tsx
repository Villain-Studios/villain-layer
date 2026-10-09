import { formatDuration, formatTokens, totalTokens, type AgentRow, type Spend } from "../../lib/runs";

/** Each agent side by side, and where the time went (RUN-8). */
export function RunBreakdown({ agents, tasks, repos, agentName }: {
  agents: AgentRow[];
  tasks: Spend[];
  repos: Spend[];
  agentName: (id: string) => string;
}) {
  return (
    <div className="run-breakdown">
      <section>
        <h3>By agent</h3>
        <table className="run-table">
          <thead>
            <tr>
              <th>Agent</th>
              <th>Runs</th>
              <th title="Failed, or a loop gave up">Went wrong</th>
              <th title="The median run">Typical run</th>
              <th>Tokens a run</th>
              <th title="Time spent waiting on you, a run">Waited a run</th>
              <th>Failed tools</th>
            </tr>
          </thead>
          <tbody>
            {agents.map(({ agent, stats: s }) => (
              <tr key={agent}>
                <td>{agentName(agent)}</td>
                <td>{s.runs}</td>
                <td className={s.wrong ? "bad" : ""}>{Math.round(s.wrongRate * 100)}%</td>
                <td>{formatDuration(s.medianMs)}</td>
                <td>{s.tokens ? formatTokens(Math.round(totalTokens(s.tokens) / s.withTokens)) : "—"}</td>
                <td>{formatDuration((s.waitedSecs / s.runs) * 1000)}</td>
                <td>{s.tools?.calls ? `${Math.round((s.tools.failed / s.tools.calls) * 100)}%` : "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
      <Spent title="Most time, by task" rows={tasks} />
      <Spent title="Most time, by repository" rows={repos} />
    </div>
  );
}

function Spent({ title, rows }: { title: string; rows: Spend[] }) {
  const peak = Math.max(1, ...rows.map((r) => r.ms));
  return (
    <section>
      <h3>{title}</h3>
      <ul className="run-spend">
        {rows.map((r) => (
          <li key={r.id} title={`${r.runs} ${r.runs === 1 ? "run" : "runs"}${r.tokens ? `, ${formatTokens(r.tokens)} tokens` : ""}`}>
            <span className="name">{r.name}</span>
            <span className="took">{formatDuration(r.ms)}</span>
            <span className="meter" style={{ width: `${(r.ms / peak) * 100}%` }} />
          </li>
        ))}
      </ul>
    </section>
  );
}
