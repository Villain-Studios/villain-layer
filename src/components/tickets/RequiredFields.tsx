import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import type { CreateField } from "../../lib/types";
import { Field } from "../ui";

/**
 * What a Jira project insists on before it will take a new issue of a type,
 * asked of Jira rather than assumed, and what has been picked for it.
 *
 * `own` are the fields the form fills itself. Asked after `projectKey` stops
 * changing, since it is typed a letter at a time.
 */
export function useRequiredFields(projectKey: string, typeId: string | undefined, own: string[]) {
  const [needed, setNeeded] = useState<CreateField[]>([]);
  const [loading, setLoading] = useState(false);
  const [extra, setExtra] = useState<Record<string, string[]>>({});

  useEffect(() => {
    if (!projectKey || !typeId) { setNeeded([]); return; }
    // Only the latest answer lands: switching type quickly otherwise left
    // the previous type's fields on screen, or cleared the spinner early.
    let current = true;
    setLoading(true);
    setExtra({});
    const t = window.setTimeout(() => {
      api.jiraCreateFields(projectKey, typeId)
        .then((f) => { if (current) setNeeded(f.filter((x) => x.required)); })
        // A site that will not describe its own form is no reason to block
        // the dialog: Jira still says what is missing if the create is refused.
        .catch(() => { if (current) setNeeded([]); })
        .finally(() => { if (current) setLoading(false); });
    }, 300);
    return () => { current = false; window.clearTimeout(t); };
  }, [projectKey, typeId]);

  // Required, and a closed set of values, so it can be offered as a choice.
  const pickable = needed.filter((f) => !own.includes(f.id) && f.allowed.length > 0);
  // Required, free-form, and not something the form asks for. Nothing
  // sensible can be invented for these, so say so rather than failing at Jira.
  const unsupported = needed.filter((f) => !own.includes(f.id) && f.allowed.length === 0);
  const missing = pickable.filter((f) => (extra[f.id] ?? []).length === 0);

  /** Shaped the way Jira wants each field, from the metadata it gave us. */
  function fields(): Record<string, unknown> {
    const out: Record<string, unknown> = {};
    for (const field of needed) {
      const picked = extra[field.id] ?? [];
      if (picked.length === 0) continue;
      out[field.id] = field.kind === "array" ? picked.map((id) => ({ id })) : { id: picked[0] };
    }
    return out;
  }

  return {
    loading,
    pickable,
    unsupported,
    missing,
    extra,
    setExtra,
    fields,
    descriptionRequired: needed.some((f) => f.id === "description"),
  };
}

/** A choice for each required field the form does not fill itself. */
export function RequiredFieldPickers({ rf }: { rf: ReturnType<typeof useRequiredFields> }) {
  return (
    <>
      {rf.unsupported.length > 0 && (
        <div className="confirm-detail" style={{ marginBottom: 12 }}>
          This project also requires {rf.unsupported.map((f) => f.name).join(", ")}, which this
          dialog cannot fill in. Jira will refuse the ticket — file it in Jira instead.
        </div>
      )}
      {rf.pickable.map((field) => {
        const picked = rf.extra[field.id] ?? [];
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
                      rf.setExtra((c) => ({
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
    </>
  );
}
