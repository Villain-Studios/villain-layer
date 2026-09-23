import { useEffect, useMemo, useState } from "react";
import { api } from "../../lib/api";
import { read, write } from "../../lib/persist";
import { useStore } from "../../store";
import type { CreateField, JiraIssue, JiraIssueType } from "../../lib/types";
import { Combo, Field, Modal } from "../ui";
import { IssueTypeIcon, type TypeMap } from "../IssueType";
import { epicTypes, preferredEpic } from "../task-forms/creatable";
import { OptimizeDescription } from "./OptimizeDescription";

export function CreateEpicDialog({
  issueTypes,
  types,
  issues,
  onClose,
}: {
  issueTypes: JiraIssueType[];
  types: TypeMap;
  issues: JiraIssue[];
  onClose: () => void;
}) {
  const settings = useStore((s) => s.settings);
  const refreshIssues = useStore((s) => s.refreshIssues);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [summary, setSummary] = useState("");
  const [desc, setDesc] = useState("");
  const [epicType, setEpicType] = useState("");
  const [project, setProject] = useState(() => read("jiraProject", ""));
  const [busy, setBusy] = useState(false);
  const [needed, setNeeded] = useState<CreateField[]>([]);
  const [neededLoading, setNeededLoading] = useState(false);
  const [extra, setExtra] = useState<Record<string, string[]>>({});

  const epics = useMemo(() => epicTypes(issueTypes), [issueTypes]);
  const boardKeys = useMemo(
    () => [...new Set(issues.map((i) => i.key.split("-")[0]).filter(Boolean))],
    [issues],
  );
  const defaultProject = settings?.jira?.project_key ?? boardKeys[0] ?? "";

  useEffect(() => {
    setProject((current) => current || defaultProject);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (epicType && epics.some((t) => t.name === epicType)) return;
    setEpicType(preferredEpic(epics)?.name ?? "");
  }, [epics, epicType]);

  // Keyed on the id and guarded, as in FileIssueDialog: the type list is
  // rebuilt on every background refresh of the tickets, which re-ran this and
  // cleared the picks, and a slower earlier answer could land last.
  const typeId = epics.find((t) => t.name === epicType)?.id;
  const projectKey = project.trim();
  useEffect(() => {
    if (!projectKey || !typeId) { setNeeded([]); return; }
    let current = true;
    setNeededLoading(true);
    setExtra({});
    api.jiraCreateFields(projectKey, typeId)
      .then((f) => { if (current) setNeeded(f.filter((x) => x.required)); })
      // A site that will not describe its own form is no reason to block the
      // dialog: Jira still says what is missing if the create is refused.
      .catch(() => { if (current) setNeeded([]); })
      .finally(() => { if (current) setNeededLoading(false); });
    return () => { current = false; };
  }, [projectKey, typeId]);

  function extraFields(): Record<string, unknown> {
    const out: Record<string, unknown> = {};
    for (const field of needed) {
      const picked = extra[field.id] ?? [];
      if (picked.length === 0) continue;
      out[field.id] =
        field.kind === "array" ? picked.map((id) => ({ id })) : { id: picked[0] };
    }
    return out;
  }

  // Fields the dialog fills itself, whether or not Jira calls them required.
  const OWN = ["summary", "description", "issuetype", "project", "parent", "reporter"];
  const pickable = needed.filter((f) => !OWN.includes(f.id) && f.allowed.length > 0);
  const unsupported = needed.filter((f) => !OWN.includes(f.id) && f.allowed.length === 0);
  const descriptionRequired = needed.some((f) => f.id === "description");

  const missing = [
    ...pickable.filter((f) => (extra[f.id] ?? []).length === 0),
    ...(descriptionRequired && !desc.trim()
      ? [{ id: "description", name: "Description" }]
      : []),
  ];

  async function createEpic() {
    // The button's own conditions: Enter in the summary came straight here,
    // and a second Enter created a second epic.
    if (busy || !summary.trim() || !epicType || !project.trim() || missing.length > 0) return;
    setBusy(true);
    try {
      const issue = await api.jiraCreateIssue({
        summary: summary.trim(),
        description: desc.trim(),
        issue_type: epicType,
        project_key: project.trim(),
        parent_key: null,
        fields: extraFields(),
      });
      toast("success", `Created ${issue.key}`);
      onClose();
      await refreshIssues();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      title="New epic"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button
            className="btn btn-primary"
            disabled={
              busy ||
              !summary.trim() ||
              !epicType ||
              !project.trim() ||
              missing.length > 0
            }
            title={
              missing.length > 0
                ? `${missing.map((f) => f.name).join(", ")} required by this project`
                : undefined
            }
            onClick={() => void createEpic()}
          >
            {busy ? "Creating…" : "Create epic"}
          </button>
        </>
      }
    >
      <div className="muted" style={{ marginBottom: 12, lineHeight: 1.6 }}>
        Creates a top-level epic in the project you pick. Nothing is checked out;
        file tickets under it once it exists.
      </div>

      <Field label="Project" hint="The key the epic is filed under.">
        <Combo
          value={project}
          options={boardKeys}
          placeholder="ACME"
          width="100%"
          onChange={(raw) => {
            const v = raw.trim().toUpperCase();
            setProject(v);
            write("jiraProject", v);
          }}
        />
      </Field>

      <Field label="Summary">
        <input
          autoFocus
          value={summary}
          onChange={(e) => setSummary(e.target.value)}
          placeholder="What this epic covers"
          onKeyDown={(e) => { if (e.key === "Enter") void createEpic(); }}
        />
      </Field>

      <Field label="Type">
        <div className="type-row">
          {epics.map((t) => (
            <button
              key={t.id}
              className={`type-pick${epicType === t.name ? " active" : ""}`}
              onClick={() => setEpicType(t.name)}
            >
              <IssueTypeIcon types={types} name={t.name} size={15} />
              {t.name}
            </button>
          ))}
        </div>
      </Field>

      {neededLoading && (
        <div className="muted" style={{ marginBottom: 10 }}>
          Asking Jira what this project requires…
        </div>
      )}
      {unsupported.length > 0 && (
        <div className="confirm-detail" style={{ marginBottom: 12 }}>
          This project also requires {unsupported.map((f) => f.name).join(", ")},
          which this dialog cannot fill in. Jira will refuse the epic — create it
          in Jira instead, and it will show up here on the next refresh.
        </div>
      )}
      {pickable.map((field) => {
        const picked = extra[field.id] ?? [];
        const many = field.kind === "array";
        return (
          <Field
            key={field.id}
            label={field.name}
            hint={`Required by this project${many ? " — pick one or more" : ""}.`}
          >
            <div className="type-row">
              {field.allowed.map((v) => {
                const on = picked.includes(v.id);
                return (
                  <button
                    key={v.id}
                    className={`type-pick${on ? " active" : ""}`}
                    onClick={() =>
                      setExtra((c) => ({
                        ...c,
                        [field.id]: on
                          ? picked.filter((x) => x !== v.id)
                          : many
                            ? [...picked, v.id]
                            : [v.id],
                      }))
                    }
                  >
                    {v.name}
                  </button>
                );
              })}
            </div>
          </Field>
        );
      })}

      <Field
        label="Description"
        hint={
          descriptionRequired
            ? "Required by this project."
            : "Optional. Becomes the epic body."
        }
      >
        <textarea
          rows={5}
          value={desc}
          onChange={(e) => setDesc(e.target.value)}
          placeholder="Scope, outcomes, and anything an agent should know."
        />
        <OptimizeDescription
          summary={summary}
          description={desc}
          kind="epic"
          onChange={setDesc}
          disabled={busy}
        />
      </Field>
    </Modal>
  );
}
