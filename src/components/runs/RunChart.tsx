import { useState } from "react";
import { formatDuration, formatTokens, type RunDay } from "../../lib/runs";

type Measure = "runs" | "tokens" | "waited";

const MEASURES: { id: Measure; label: string }[] = [
  { id: "runs", label: "Runs" },
  { id: "tokens", label: "Tokens" },
  { id: "waited", label: "Waiting on you" },
];

/** A bar a day, of runs (red: those that went wrong), tokens or time waited on you (RUN-4). */
export function RunChart({ days }: { days: RunDay[] }) {
  const [measure, setMeasure] = useState<Measure>("runs");
  const value = (d: RunDay) => (measure === "runs" ? d.runs : measure === "tokens" ? d.tokens : d.waitedSecs);
  const peak = Math.max(1, ...days.map(value));
  const say = (d: RunDay) =>
    measure === "runs"
      ? `${d.runs} ${d.runs === 1 ? "run" : "runs"}, ${d.wrong} went wrong`
      : measure === "tokens"
        ? `${formatTokens(d.tokens)} tokens`
        : `waited on you ${formatDuration(d.waitedSecs * 1000)}`;

  return (
    <div className="run-chart">
      <div className="tabs sub-tabs">
        {MEASURES.map((m) => (
          <button key={m.id} type="button" className={measure === m.id ? "active" : ""} onClick={() => setMeasure(m.id)}>
            {m.label}
          </button>
        ))}
      </div>
      <div className="run-days" role="img" aria-label={`${MEASURES.find((m) => m.id === measure)?.label} on each of the last ${days.length} days`}>
        {days.map((d) => (
          <div key={d.day} className="run-day" title={`${d.day}: ${say(d)}`}>
            <div className={`bar ${measure}`} style={{ height: `${(value(d) / peak) * 100}%` }}>
              {measure === "runs" && <div className="wrong" style={{ height: d.runs ? `${(d.wrong / d.runs) * 100}%` : 0 }} />}
            </div>
          </div>
        ))}
      </div>
      <div className="run-days-axis">
        <span>{days.length} days ago</span>
        <span>today</span>
      </div>
    </div>
  );
}
