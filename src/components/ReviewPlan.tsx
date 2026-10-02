import { useMemo, useState } from "react";
import { useStore } from "../store";
import type { ChangedFile, PlanFile } from "../lib/types";
import { ChevronIcon } from "./icons";
import { Spinner } from "./ui";

const keyOf = (f: { checkout_id: string; path: string }) => `${f.checkout_id}:${f.path}`;
const NO_FILES: string[] = [];

interface Row {
  file: ChangedFile;
  why: string;
}

const GROUPS: { id: PlanFile["group"] | "rest"; label: string; shut?: boolean }[] = [
  { id: "start", label: "Start here" },
  { id: "tests", label: "Tests" },
  { id: "rest", label: "Everything else" },
  { id: "routine", label: "Low risk", shut: true },
];

/** The files of a review in the order Claude's plan gives, the rest after. */
function grouped(files: ChangedFile[], plan: PlanFile[] | null): { id: string; label: string; shut?: boolean; rows: Row[] }[] {
  const byKey = new Map(files.map((f) => [keyOf(f), f]));
  const placed = new Set<string>();
  const rows: Record<string, Row[]> = { start: [], tests: [], rest: [], routine: [] };
  for (const p of plan ?? []) {
    const file = byKey.get(keyOf(p));
    if (!file || placed.has(keyOf(p))) continue;
    placed.add(keyOf(p));
    rows[p.group].push({ file, why: p.why });
  }
  for (const f of [...files].sort((a, b) => a.path.localeCompare(b.path))) {
    if (!placed.has(keyOf(f))) rows.rest.push({ file: f, why: "" });
  }
  return GROUPS
    .map((g) => ({ ...g, label: !plan && g.id === "rest" ? "Files" : g.label, rows: rows[g.id] }))
    .filter((g) => g.rows.length > 0);
}

/** The files of a review in reading order, flat: what "next" means. */
export function readingOrder(files: ChangedFile[], plan: PlanFile[] | null): string[] {
  return grouped(files, plan).flatMap((g) => g.rows.map((r) => keyOf(r.file)));
}

/**
 * Mark `key` viewed and say which file to open next: the first after it in
 * reading order that is not viewed yet, wrapping round.
 */
export function viewAndNext(taskId: string, head: string, files: ChangedFile[], key: string): string | undefined {
  const st = useStore.getState();
  st.setViewed(taskId, head, key, true);
  const seen = new Set(st.viewed[taskId]?.head === head ? st.viewed[taskId].files : []);
  const plan = st.reviewer[taskId]?.last?.plan;
  const order = readingOrder(files, plan?.length ? plan : null);
  return order.slice(order.indexOf(key) + 1).concat(order).find((k) => !seen.has(k));
}

/**
 * Where a review starts (REV-12): Claude's summary of the change, its files
 * in the order to read them with why, and how many are viewed. It stands
 * where the folder tree stands for your own work: a folder tree says where
 * a file is, not which one matters.
 */
export function ReviewPlan({
  taskId,
  head,
  files,
  selected,
  onSelect,
  notesPerFile,
}: {
  taskId: string;
  head: string;
  files: ChangedFile[];
  selected: string | null;
  onSelect: (key: string) => void;
  notesPerFile: Map<string, number>;
}) {
  const reviewer = useStore((s) => s.reviewer[taskId]);
  const runReviewer = useStore((s) => s.runReviewer);
  const viewedList = useStore((s) => (s.viewed[taskId]?.head === head ? s.viewed[taskId].files : NO_FILES));
  const setViewed = useStore((s) => s.setViewed);
  const plan = reviewer?.last?.plan.length ? reviewer.last.plan : null;
  const groups = useMemo(() => grouped(files, plan), [files, plan]);
  const viewed = useMemo(() => new Set(viewedList), [viewedList]);
  const [shut, setShut] = useState<Record<string, boolean>>({});
  const done = files.filter((f) => viewed.has(keyOf(f))).length;

  function mark(key: string, on: boolean) {
    if (!on) return setViewed(taskId, head, key, false);
    const next = viewAndNext(taskId, head, files, key);
    // Viewing the open file moves on to the next one not viewed yet.
    if (next && key === selected) onSelect(next);
  }

  return (
    <div className="plan">
      <div className="plan-progress" title={`${done} of ${files.length} files viewed`}>
        <div className="bar"><div style={{ width: `${files.length ? (100 * done) / files.length : 0}%` }} /></div>
        <span>{done} of {files.length} viewed</span>
      </div>

      <div className="plan-summary">
        {reviewer?.running ? (
          <span className="muted"><Spinner /> Claude is reading the change for a plan and findings…</span>
        ) : reviewer?.last?.summary ? (
          reviewer.last.summary
        ) : reviewer?.error ? (
          <span className="muted">
            Claude could not make a plan. <button className="link" onClick={() => void runReviewer(taskId)}>Try again</button>
          </span>
        ) : (
          <span className="muted">
            No plan yet. <button className="link" onClick={() => void runReviewer(taskId)}>Ask Claude</button>
          </span>
        )}
      </div>

      {groups.map((g) => {
        const closed = shut[g.id] ?? g.shut ?? false;
        const seen = g.rows.filter((r) => viewed.has(keyOf(r.file))).length;
        return (
          <div key={g.id} className="plan-group">
            <button className="plan-head" onClick={() => setShut((c) => ({ ...c, [g.id]: !closed }))}>
              <span className={`chev${closed ? "" : " open"}`}><ChevronIcon /></span>
              {g.label}
              <span className="muted">{seen}/{g.rows.length}</span>
            </button>
            {!closed && g.rows.map(({ file, why }) => {
              const key = keyOf(file);
              const slash = file.path.lastIndexOf("/");
              const notes = notesPerFile.get(key);
              return (
                <div
                  key={key}
                  className={`plan-file${key === selected ? " active" : ""}${viewed.has(key) ? " viewed" : ""}`}
                  title={why ? `${file.path}\n${why}` : file.path}
                  onClick={() => onSelect(key)}
                >
                  <input
                    type="checkbox"
                    checked={viewed.has(key)}
                    title="Viewed"
                    onClick={(e) => e.stopPropagation()}
                    onChange={(e) => mark(key, e.target.checked)}
                  />
                  <span className="name">
                    {file.path.slice(slash + 1)}
                    {why && <span className="why">{why}</span>}
                    {!why && slash > 0 && <span className="why">{file.path.slice(0, slash)}</span>}
                  </span>
                  {notes && <span className="badge" title="Notes on this file">●&nbsp;{notes}</span>}
                  <span className="n add">+{file.additions}</span>
                  <span className="n del">−{file.deletions}</span>
                </div>
              );
            })}
          </div>
        );
      })}
    </div>
  );
}

/** The open file of a review, and ticking it viewed, as GitHub heads each file. */
export function ReviewFileBar({
  taskId,
  head,
  file,
  files,
  onSelect,
}: {
  taskId: string;
  head: string;
  file: ChangedFile;
  files: ChangedFile[];
  onSelect: (key: string) => void;
}) {
  const viewed = useStore((s) => s.viewed[taskId]?.head === head && s.viewed[taskId].files.includes(keyOf(file)));
  return (
    <div className="diff-repo-banner review-file-bar">
      <span className="path">{file.path}</span>
      <div className="spacer" />
      <label title="Mark it viewed and go to the next file">
        <input type="checkbox" checked={viewed} onChange={(e) => {
            if (!e.target.checked) return useStore.getState().setViewed(taskId, head, keyOf(file), false);
            const next = viewAndNext(taskId, head, files, keyOf(file));
            if (next) onSelect(next);
          }}
        />
        Viewed
      </label>
    </div>
  );
}
