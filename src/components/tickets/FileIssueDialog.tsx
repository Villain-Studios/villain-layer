import { useEffect, useMemo, useState } from "react";
import { api } from "../../lib/api";
import { useStore } from "../../store";
import type { CreateField, JiraIssue, JiraIssueType } from "../../lib/types";
import { Field, Modal } from "../ui";
import { IssueTypeIcon, type TypeMap } from "../IssueType";
import { creatableTypes, preferredCreatable } from "../task-forms/creatable";

export function FileIssueDialog({
  epic,
  issueTypes,
  types,
  onClose,
  onFiled,
}: {
  epic: { key: string; summary: string };
  issueTypes: JiraIssueType[];
  types: TypeMap;
  onClose: () => void;
  onFiled: (issue: JiraIssue) => void;
}) {
  const refreshIssues = useStore((s) => s.refreshIssues);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [newSummary, setNewSummary] = useState("");
  const [newDesc, setNewDesc] = useState("");
  const [newType, setNewType] = useState("");
  const [filingBusy, setFilingBusy] = useState(false);
  // What this project insists on, asked of Jira rather than assumed.
  const [needed, setNeeded] = useState<CreateField[]>([]);
  const [neededLoading, setNeededLoading] = useState(false);
  const [extra, setExtra] = useState<Record<string, string[]>>({});

  const creatable = useMemo(() => creatableTypes(issueTypes), [issueTypes]);

  useEffect(() => {
    if (newType && creatable.some((t) => t.name === newType)) return;
    setNewType(preferredCreatable(creatable)?.name ?? "");
  }, [creatable, newType]);

  useEffect(() => {
    const project = epic.key.split("-")[0];
    const typeId = creatable.find((t) => t.name === newType)?.id;
    if (!project || !typeId) { setNeeded([]); return; }
    setNeededLoading(true);
    setExtra({});
    api.jiraCreateFields(project, typeId)
      .then((f) => setNeeded(f.filter((x) => x.required)))
      // A site that will not describe its own form is no reason to block the
      // dialog: Jira still says what is missing if the create is refused.
      .catch(() => setNeeded([]))
      .finally(() => setNeededLoading(false));
  }, [epic.key, newType, creatable]);

  /// Shaped the way Jira wants each field, from the metadata it gave us.
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
  // Required, and a closed set of values, so it can be offered as a choice.
  const pickable = needed.filter((f) => !OWN.includes(f.id) && f.allowed.length > 0);
  // Required, free-form, and not something this dialog asks for. Nothing
  // sensible can be invented for these, so say so rather than failing at Jira.
  const unsupported = needed.filter((f) => !OWN.includes(f.id) && f.allowed.length === 0);
  const descriptionRequired = needed.some((f) => f.id === "description");

  const missing = [
    ...pickable.filter((f) => (extra[f.id] ?? []).length === 0),
    ...(descriptionRequired && !newDesc.trim()
      ? [{ id: "description", name: "Description" }]
      : []),
  ];

  async function fileIssue() {
    if (!newSummary.trim() || !newType) return;
    setFilingBusy(true);
    try {
      const issue = await api.jiraCreateIssue({
        summary: newSummary.trim(),
        description: newDesc.trim(),
        issue_type: newType,
        project_key: null,
        parent_key: epic.key,
        fields: extraFields(),
      });
      toast("success", `Filed ${issue.key} under ${epic.key}`);
      onClose();
      await refreshIssues();
      // Straight into the start-work dialog: filing it is usually the first
      // half of picking it up.
      onFiled(issue);
    } catch (e) {
      fail(e);
    } finally {
      setFilingBusy(false);
    }
  }

  return (
    <Modal
      title={`New ticket in ${epic.key}`}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button
            className="btn btn-primary"
            disabled={filingBusy || !newSummary.trim() || !newType || missing.length > 0}
            title={
              missing.length > 0
                ? `${missing.map((f) => f.name).join(", ")} required by this project`
                : undefined
            }
            onClick={() => void fileIssue()}
          >
            {filingBusy ? "Filing…" : "File ticket"}
          </button>
        </>
      }
    >
      <div className="muted" style={{ marginBottom: 12, lineHeight: 1.6 }}>
        Filed under <b>{epic.key}</b>
        {epic.summary ? ` — ${epic.summary}` : ""}, in that epic's own project.
        Nothing is checked out; the start-work dialog opens once it exists.
      </div>

      <Field label="Summary">
        <input
          autoFocus
          value={newSummary}
          onChange={(e) => setNewSummary(e.target.value)}
          placeholder="What needs doing"
          onKeyDown={(e) => { if (e.key === "Enter") void fileIssue(); }}
        />
      </Field>

      <Field label="Type">
        <div className="type-row">
          {creatable.map((t) => (
            <button
              key={t.id}
              className={`type-pick${newType === t.name ? " active" : ""}`}
              onClick={() => setNewType(t.name)}
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
          which this dialog cannot fill in. Jira will refuse the ticket — file it
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
            ? "Required by this project. Becomes the ticket body and an agent's briefing."
            : "Optional. Becomes the ticket body and an agent's briefing."
        }
      >
        <textarea
          rows={5}
          value={newDesc}
          onChange={(e) => setNewDesc(e.target.value)}
          placeholder="What needs doing, and how you would know it is done."
        />
      </Field>
    </Modal>
  );
}
