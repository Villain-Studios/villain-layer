import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api";
import { useStore } from "../../store";
import type { JiraIssue, JiraIssueType, Project, Settings } from "../../lib/types";
import { Combo, Field, Modal, Switch } from "../ui";
import { RepoPicker } from "../RepoPicker";
import { read, write } from "../../lib/persist";
import { IssueTypeIcon, typeMap } from "../IssueType";
import { creatableTypes, preferredCreatable } from "../task-forms/creatable";
import { OptimizeDescription } from "../tickets/OptimizeDescription";

export function CreateTaskDialog({
  projects,
  settings,
  issues,
  issueTypes,
  onClose,
}: {
  projects: Project[];
  settings: Settings | null;
  issues: JiraIssue[];
  issueTypes: JiraIssueType[];
  onClose: () => void;
}) {
  const refreshIssues = useStore((s) => s.refreshIssues);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const select = useStore((s) => s.select);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [name, setName] = useState("");
  const [branch, setBranch] = useState("");
  // Filing the ticket and opening the worktrees are the same act often enough
  // that they belong in one dialog rather than two views.
  const [withJira, setWithJira] = useState(false);
  const [jiraType, setJiraType] = useState("");
  const [jiraProject, setJiraProject] = useState(() => read("jiraProject", ""));
  const [jiraParent, setJiraParent] = useState("");
  const [jiraDesc, setJiraDesc] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [reason, setReason] = useState<string | null>(null);
  const [base, setBase] = useState("");
  const [baseOptions, setBaseOptions] = useState<string[]>([]);
  /** The base this dialog last filled in itself, as opposed to one typed. */
  const autoBase = useRef("");

  const selected = projects.filter((p) => picked.includes(p.id));
  const tm = useMemo(() => typeMap(issueTypes), [issueTypes]);
  // Most sites never set a default project, so rather than disable the whole
  // feature the dialog asks — offering the keys already visible on the board.
  const boardKeys = useMemo(
    () => [...new Set(issues.map((i) => i.key.split("-")[0]).filter(Boolean))],
    [issues],
  );
  const defaultProject = settings?.jira?.project_key ?? boardKeys[0] ?? "";
  const creatable = useMemo(() => creatableTypes(issueTypes), [issueTypes]);
  // Epics from the board are what the dialog used to offer, which is only the
  // ones already carrying tickets. Jira is asked for the rest.
  const [projectEpics, setProjectEpics] = useState<JiraIssue[]>([]);
  const [epicsLoading, setEpicsLoading] = useState(false);
  const epicOptions = useMemo(
    () => projectEpics.map((e) => `${e.key} — ${e.summary}`),
    [projectEpics],
  );
  const epicLabel = jiraParent
    ? epicOptions.find((o) => o.startsWith(`${jiraParent} `)) ?? jiraParent
    : "";

  // Default to whatever the site calls a plain issue, without assuming it is
  // named "Task" — it often is not.
  useEffect(() => {
    if (jiraType && creatable.some((t) => t.name === jiraType)) return;
    setJiraType(preferredCreatable(creatable)?.name ?? "");
  }, [creatable, jiraType]);

  // Asked for when the dialog is open and a project is named, and again when
  // that changes. Quiet on failure: the field still takes a typed key.
  useEffect(() => {
    if (!withJira || !jiraProject.trim()) { setProjectEpics([]); return; }
    let stop = false;
    setEpicsLoading(true);
    api.jiraEpics(jiraProject.trim())
      .then((list) => { if (!stop) setProjectEpics(list); })
      .catch(() => { if (!stop) setProjectEpics([]); })
      .finally(() => { if (!stop) setEpicsLoading(false); });
    return () => { stop = true; };
  }, [withJira, jiraProject]);

  // Seeded when the dialog opens, and only then: refilling whenever the field
  // is empty would make it impossible to clear or retype.
  useEffect(() => {
    setJiraProject((current) => current || defaultProject);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    api.suggestRepos({})
      .then((s) => { setPicked(s.project_ids); setReason(s.reason); })
      .catch(() => setPicked([]));
  }, []);

  useEffect(() => {
    if (selected.length === 0) {
      setBase("");
      setBaseOptions([]);
      return;
    }
    const defaults = [...new Set(selected.map((p) => p.default_branch))];
    // Suggest, do not overwrite: a base typed by hand survives adding another
    // repository. Only a box still showing the last suggestion follows it.
    const suggested = defaults.length === 1 ? defaults[0] : "";
    setBase((cur) => (cur === "" || cur === autoBase.current ? suggested : cur));
    autoBase.current = suggested;
    let cancelled = false;
    Promise.all(selected.map((p) => api.projectBranches(p.id).catch(() => [] as string[])))
      .then((lists) => {
        if (cancelled) return;
        const seen = new Set<string>();
        const merged: string[] = [];
        for (const list of lists) {
          for (const b of list) {
            if (!seen.has(b)) {
              seen.add(b);
              merged.push(b);
            }
          }
        }
        for (const d of defaults) {
          if (!seen.has(d)) merged.unshift(d);
        }
        setBaseOptions(merged);
      });
    return () => { cancelled = true; };
  }, [picked.join(","), projects]);

  async function createTask() {
    if (!name.trim() || picked.length === 0) return;
    setBusy(true);
    try {
      const baseBranch = base.trim() || null;
      // With a ticket, the key it comes back with names the branch and the
      // folder, so the task cannot be built until Jira has answered.
      const task = withJira
        ? await api.jiraCreateTask({
            summary: name.trim(),
            description: jiraDesc.trim(),
            issue_type: jiraType,
            project_key: jiraProject.trim() || null,
            parent_key: jiraParent || null,
            project_ids: picked,
            branch_suffix: branch.trim() || null,
            base: baseBranch,
          })
        : await api.createTask({
            name: name.trim(),
            project_ids: picked,
            branch: branch.trim() || null,
            base: baseBranch,
          });
      await refreshTasks();
      if (withJira) {
        toast(
          "success",
          `Filed ${task.issue_key} and opened ${picked.length} worktree${
            picked.length === 1 ? "" : "s"
          }` + ("moved" in task && task.moved ? ` · ${task.moved}` : ""),
        );
        void refreshIssues();
      }
      select(task.id);
      onClose();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      title="New task"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button
            className="btn btn-primary"
            disabled={
              busy || !name.trim() || picked.length === 0 ||
              (withJira && (!jiraType || !jiraProject.trim()))
            }
            onClick={() => void createTask()}
          >
            {busy
              ? withJira ? "Filing…" : "Creating…"
              : withJira
                ? `File ticket + ${picked.length} worktree${picked.length === 1 ? "" : "s"}`
                : `Create ${picked.length} worktree${picked.length === 1 ? "" : "s"}`}
          </button>
        </>
      }
    >
      <Field label={withJira ? "Summary" : "Task name"}>
        <input
          autoFocus
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="fix checkout rounding"
        />
      </Field>

      <Switch
        label="File a Jira ticket for this"
        detail={
          settings?.jira_connected
            ? "Files the issue first, then names the branch and folder after its key."
            : "Connect Jira in Settings to file from here."
        }
        checked={withJira}
        disabled={!settings?.jira_connected}
        onChange={setWithJira}
      />

      {withJira && (
        <div className="jira-fields">
          <Field label="Project" hint="The key the ticket is filed under.">
            <Combo
              value={jiraProject}
              options={boardKeys}
              placeholder="ACME"
              width="100%"
              onChange={(raw) => {
                const v = raw.trim().toUpperCase();
                setJiraProject(v);
                write("jiraProject", v);
              }}
            />
          </Field>

          <Field label="Type">
            <div className="type-row">
              {creatable.map((t) => (
                <button
                  key={t.id}
                  className={`type-pick${jiraType === t.name ? " active" : ""}`}
                  onClick={() => setJiraType(t.name)}
                >
                  <IssueTypeIcon types={tm} name={t.name} size={15} />
                  {t.name}
                </button>
              ))}
              {creatable.length === 0 && (
                <span className="muted">Jira reported no issue types.</span>
              )}
            </div>
          </Field>

          <Field
            label="Epic"
            hint={
              epicsLoading
                ? "Asking Jira for this project's epics…"
                : "Optional. Every open epic on the project, whether or not it has work on it."
            }
          >
            <Combo
              value={epicLabel}
              options={epicOptions}
              placeholder="No epic"
              width="100%"
              empty="No open epics on this project"
              onChange={(v) => setJiraParent(v.trim().split(/\s/)[0] ?? "")}
            />
          </Field>

          <Field label="Description" hint="Optional. Becomes the ticket body and the agent's briefing.">
            <textarea
              rows={4}
              value={jiraDesc}
              onChange={(e) => setJiraDesc(e.target.value)}
              placeholder="What needs doing, and how you would know it is done."
            />
            <OptimizeDescription
              summary={name}
              description={jiraDesc}
              kind="ticket"
              onChange={setJiraDesc}
              disabled={busy}
            />
          </Field>
        </div>
      )}

      <Field
        label="Repositories"
        hint="One worktree per repo, all on the same branch, side by side in one task folder."
      >
        <RepoPicker
          projects={projects}
          picked={picked}
          onChange={setPicked}
          reason={reason}
        />
      </Field>

      <Field
        label={withJira ? "Branch suffix" : "Branch"}
        hint={
          withJira
            ? `Optional. The branch is the ticket key — ${jiraProject || "KEY"}-123, or ${jiraProject || "KEY"}-123-your-suffix.`
            : "Defaults to a slug of the task name. Used in every repo."
        }
      >
        <input
          value={branch}
          onChange={(e) => setBranch(e.target.value)}
          placeholder={withJira ? "(none)" : "(auto)"}
        />
      </Field>

      <Field
        label="Branch from"
        hint={
          selected.length === 0
            ? "Pick repositories first."
            : selected.length === 1
              ? `New branch starts at this tip in ${selected[0].name}.`
              : "Created from this tip in every repo you picked. Blank uses each repo's own default."
        }
      >
        <Combo
          value={base}
          options={baseOptions}
          placeholder={
            selected.length > 1 && !base
              ? "(each repo's default)"
              : selected[0]?.default_branch ?? "main"
          }
          empty="No branch matches"
          width="100%"
          onChange={setBase}
        />
      </Field>
    </Modal>
  );
}
