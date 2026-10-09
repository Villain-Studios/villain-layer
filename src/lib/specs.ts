import { create } from "zustand";
import { api } from "./api";
import type { AppliedAnswers, RepoSpec, SpecKind, SpecPart, TaskSpec } from "./types";

/**
 * Each task's specs as the Spec tab holds them (§19). Kept here rather than
 * in the tab, so an edit or a draft under way survives switching tabs or
 * tasks; and what the editor holds is kept as a draft in the task folder
 * too, so quitting loses nothing (SPEC-6).
 */
export interface SpecState {
  loaded: boolean;
  spec: TaskSpec | null;
  /** What each editor holds where it is not what is approved, by `editKey`. */
  edits: Record<string, string>;
  /** A draft being streamed in: which file, into which repositories. */
  drafting: { requestId: string; part: SpecPart } | null;
  /** Repositories whose check is running (SPEC-15). */
  checking: string[];
  /** What Apply answers last did, and what the editors held before it, for Undo (SPEC-20). */
  applied: { results: AppliedAnswers[]; before: Record<string, string> } | null;
}

interface Specs {
  byTask: Record<string, SpecState>;
  load: (taskId: string) => Promise<void>;
  edit: (taskId: string, checkoutId: string, part: SpecPart, text: string) => void;
  draft: (taskId: string, part: SpecPart, checkoutId: string | null, kind: SpecKind | null) => Promise<void>;
  /** Approve files of one repository's spec, as their editors hold them. */
  approve: (taskId: string, checkoutId: string, parts: SpecPart[], kind?: SpecKind | null) => Promise<void>;
  check: (taskId: string, checkoutId: string) => Promise<void>;
  /** Build the answered open questions into every repository's requirements (SPEC-20). */
  applyAnswers: (taskId: string) => Promise<void>;
  /** Put back what the requirements editors held before Apply answers. */
  undoAnswers: (taskId: string) => void;
  /** A piece of a draft under way (`spec:draft`): the repository's whole draft so far. */
  chunk: (requestId: string, checkoutId: string, text: string) => void;
  /** An agent ticked a step (`spec:changed`). */
  changed: (taskId: string) => void;
  /** The agent Start work picked, for the Spec tab to start once the spec is agreed (SPEC-18). */
  pending: { taskId: string; agentId: string | null } | null;
  setPending: (p: { taskId: string; agentId: string | null } | null) => void;
}

export const editKey = (checkoutId: string, part: SpecPart) => `${checkoutId}:${part}`;

/** What is approved of one file, or "". */
export function approvedText(repo: RepoSpec | undefined, part: SpecPart): string {
  return repo?.parts.find((p) => p.part === part)?.approved ?? "";
}

/** What an editor shows: the edit, else what is approved. */
export function editorText(state: SpecState | undefined, repo: RepoSpec | undefined, part: SpecPart): string {
  if (!repo) return "";
  return state?.edits[editKey(repo.checkout_id, part)] ?? approvedText(repo, part);
}

/** The tab's badge: steps done of all, or else how many requirements. */
export function specProgress(spec: TaskSpec | null): string | null {
  const repos = spec?.repos ?? [];
  const steps = repos.flatMap((r) => r.steps);
  if (steps.length > 0) return `${steps.filter((s) => s.done).length}/${steps.length}`;
  const reqs = repos.reduce((n, r) => n + r.requirements.length, 0);
  return reqs > 0 ? String(reqs) : null;
}

const EMPTY: SpecState = { loaded: false, spec: null, edits: {}, drafting: null, checking: [], applied: null };

/** Drafts are written a moment after typing stops, not at every key. */
const saving = new Map<string, { timer: ReturnType<typeof setTimeout>; write: () => Promise<void> }>();

function keep(taskId: string, checkoutId: string, part: SpecPart, text: string | null) {
  const key = `${taskId}:${editKey(checkoutId, part)}`;
  clearTimeout(saving.get(key)?.timer);
  const write = () => {
    saving.delete(key);
    return api.saveSpecDraft(taskId, checkoutId, part, text).catch(() => {});
  };
  saving.set(key, { timer: setTimeout(() => void write(), 600), write });
}

/**
 * Write now what is waiting to be written. A run that reads the drafts
 * (a redraft, Apply answers) would otherwise miss an answer picked a
 * moment before.
 */
async function flush(taskId: string) {
  const waiting = [...saving.entries()].filter(([key]) => key.startsWith(`${taskId}:`));
  await Promise.all(waiting.map(([, w]) => {
    clearTimeout(w.timer);
    return w.write();
  }));
}

/** The edits a freshly read spec brings: its drafts. */
function draftsOf(spec: TaskSpec): Record<string, string> {
  const edits: Record<string, string> = {};
  for (const repo of spec.repos) {
    for (const p of repo.parts) if (p.draft !== null) edits[editKey(repo.checkout_id, p.part)] = p.draft;
  }
  return edits;
}

export const useSpecs = create<Specs>((set, get) => {
  const put = (taskId: string, part: Partial<SpecState>) =>
    set((s) => ({ byTask: { ...s.byTask, [taskId]: { ...(s.byTask[taskId] ?? EMPTY), ...part } } }));
  const state = (taskId: string) => get().byTask[taskId] ?? EMPTY;

  return {
    byTask: {},
    load: async (taskId) => {
      const spec = await api.readSpec(taskId);
      // An edit under way here wins over the draft kept on disk, which is
      // at most a moment behind it.
      put(taskId, { loaded: true, spec, edits: { ...draftsOf(spec), ...state(taskId).edits } });
    },
    edit: (taskId, checkoutId, part, text) => {
      const repo = state(taskId).spec?.repos.find((r) => r.checkout_id === checkoutId);
      const key = editKey(checkoutId, part);
      const edits = { ...state(taskId).edits };
      const same = text.trim() === approvedText(repo, part).trim();
      if (same) delete edits[key];
      else edits[key] = text;
      // Undo after a change of the user's own would throw that away too.
      put(taskId, part === "requirements" ? { edits, applied: null } : { edits });
      keep(taskId, checkoutId, part, same ? null : text);
    },
    draft: async (taskId, part, checkoutId, kind) => {
      await flush(taskId);
      const before = state(taskId).edits;
      const requestId = `spec-${crypto.randomUUID()}`;
      put(taskId, { drafting: { requestId, part }, applied: null });
      try {
        await api.draftSpec(taskId, part, checkoutId, kind, requestId);
        put(taskId, { drafting: null });
        await get().load(taskId);
      } catch (e) {
        // A failed draft puts back what the editors held (SPEC-5).
        put(taskId, { drafting: null, edits: before });
        throw e;
      }
    },
    approve: async (taskId, checkoutId, parts, kind = null) => {
      const s = state(taskId);
      const repo = s.spec?.repos.find((r) => r.checkout_id === checkoutId);
      const texts = parts.map((part) => ({ part, text: editorText(s, repo, part) }));
      for (const part of parts) clearTimeout(saving.get(`${taskId}:${editKey(checkoutId, part)}`)?.timer);
      const spec = await api.approveSpec(taskId, checkoutId, texts, kind);
      const edits = { ...state(taskId).edits };
      for (const part of parts) delete edits[editKey(checkoutId, part)];
      put(taskId, parts.includes("requirements") ? { spec, edits, applied: null } : { spec, edits });
    },
    check: async (taskId, checkoutId) => {
      put(taskId, { checking: [...state(taskId).checking, checkoutId] });
      try {
        await api.checkSpec(taskId, checkoutId);
        await get().load(taskId);
      } finally {
        put(taskId, { checking: state(taskId).checking.filter((c) => c !== checkoutId) });
      }
    },
    applyAnswers: async (taskId) => {
      await flush(taskId);
      const before = state(taskId).edits;
      const requestId = `spec-${crypto.randomUUID()}`;
      put(taskId, { drafting: { requestId, part: "requirements" }, applied: null });
      try {
        const results = await api.applySpecAnswers(taskId, requestId);
        put(taskId, { drafting: null, applied: { results, before } });
        await get().load(taskId);
      } catch (e) {
        // As a failed draft does (SPEC-5).
        put(taskId, { drafting: null, edits: before });
        throw e;
      }
    },
    undoAnswers: (taskId) => {
      const s = state(taskId);
      if (!s.applied) return;
      for (const repo of s.spec?.repos ?? []) {
        const was = s.applied.before[editKey(repo.checkout_id, "requirements")] ?? approvedText(repo, "requirements");
        get().edit(taskId, repo.checkout_id, "requirements", was);
      }
      put(taskId, { applied: null });
    },
    chunk: (requestId, checkoutId, text) => {
      const entry = Object.entries(get().byTask).find(([, s]) => s.drafting?.requestId === requestId);
      if (!entry) return;
      const [taskId, s] = entry;
      put(taskId, { edits: { ...s.edits, [editKey(checkoutId, s.drafting!.part)]: text } });
    },
    changed: (taskId) => {
      if (state(taskId).loaded) void get().load(taskId).catch(() => {});
    },
    pending: null,
    setPending: (pending) => set({ pending }),
  };
});
