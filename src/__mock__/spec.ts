/**
 * Specs (§19) for the mock harness: each task's repositories, their three
 * files approved or drafted, drafts streamed in a line at a time, and a
 * check that answers from the requirements. Parsing is rough, as far as the
 * seeded texts need; `spec.rs` is the real thing.
 */
import { emit } from "@tauri-apps/api/event";
import { questions } from "../lib/questions";
import type { AppliedAnswers, RepoSpec, SpecKind, SpecPart, SpecVerdict, TaskSpec } from "../lib/types";
import type { World } from "./world";

const PARTS: SpecPart[] = ["requirements", "design", "tasks"];

interface Kept {
  kind: SpecKind;
  approved: Partial<Record<SpecPart, string>>;
  drafts: Partial<Record<SpecPart, string>>;
  stale: SpecPart[];
  check: { sha: string; at: number; results: SpecVerdict[]; stale: boolean } | null;
}

const API_REQUIREMENTS = `## Goal
Two logins at once no longer leave one of them signed out.

## Requirements
- R-1: WHEN two sign-ins for one account arrive within a second THE SYSTEM SHALL keep both sessions valid.
- R-2: WHEN a session refresh loses the race THE SYSTEM SHALL retry once, then answer 409 with a reason.

## Out of scope
- Rate-limiting sign-ins.

## Open questions
None.
`;

const API_DESIGN = `## Approach
Refresh under a per-account lock in Redis, so the second refresh waits for the first (R-1).

## Changes
- \`src/auth/session.ts\`: take \`lock:session:<account>\` around refresh.
- \`src/auth/login.ts\`: retry once on a lost race, then 409 (R-2).

## Interfaces
The 409 body is \`{ reason: "session_race" }\`, which \`web R-1\` shows.

## Error handling
A lock not taken in 2s is a lost race.

## Testing
Two parallel sign-ins in \`session.test.ts\` (R-1); a forced lost race (R-2).
`;

const API_TASKS = `## Tasks
- [x] 1. Take a per-account lock around session refresh (R-1)
- [x] 2. Test two parallel sign-ins (R-1)
- [ ] 3. Retry a lost race once, then answer 409 (R-2)
- [ ] 4. Test a forced lost race (R-2)
`;

const WEB_REQUIREMENTS = `## Goal
The sign-in page says so when a sign-in lost a race, instead of looping.

## Requirements
- R-1: WHEN the API answers 409 with reason session_race THE SYSTEM SHALL show "Signed in elsewhere at the same moment. Try again."
- R-2: WHEN a sign-in is in flight THE SYSTEM SHALL disable the submit button.

## Out of scope
- The API's retry (\`api R-2\`).

## Open questions
None.
`;

/** What a draft streams in, by file. */
const DRAFT: Record<SpecPart, (folder: string) => string> = {
  requirements: () => `## Goal
Password resets are rate-limited, so one address cannot be flooded with reset mails.

## Requirements
- R-1: WHEN a sixth reset is asked for one address within an hour THE SYSTEM SHALL refuse it with a clear message.
- R-2: WHEN resets are asked for different addresses THE SYSTEM SHALL count them separately.

## Out of scope
- Rate-limiting sign-ins.

## Open questions
- Q-1: Should support staff be able to lift the limit for an address?
  - Yes, from the admin page
  - No
- Q-2: Should the refusal say when to try again?
  - Yes, the minutes left
  - No, only that it was refused
`,
  design: (folder) => `## Approach
Count reset requests per address in Redis with a one-hour expiry, in \`${folder}\`.

## Changes
- \`src/auth/reset.ts\`: check the count before sending (R-1, R-2).

## Interfaces
None.

## Error handling
Redis down: let the reset through, and log it.

## Testing
Six requests for one address (R-1); two addresses (R-2).
`,
  tasks: () => `## Tasks
- [ ] 1. Count resets per address with an hour's expiry (R-1, R-2)
- [ ] 2. Refuse the sixth with a clear message (R-1)
- [ ] 3. Test one address and two (R-1, R-2)
`,
};

/** ACME-130's requirements, drafted and half answered (SPEC-19). */
const AUDIT_DRAFT = `## Goal
Admins can see who changed what in the workspace, and when.

## Requirements
- R-1: WHEN an admin opens the audit log THE SYSTEM SHALL list changes newest first, with who, what and when.
- R-2: WHEN the list is filtered by person THE SYSTEM SHALL show only that person's changes.
- R-3: WHEN an entry is older than the kept period THE SYSTEM SHALL leave it out.

## Out of scope
- Exporting the log.

## Open questions
- Q-1: How long are entries kept?
  - 90 days
  - A year
  - For good
  Answer: A year
- Q-2: Can admins see changes made by other admins?
  - Yes
  - Only their own
- Q-3: Should the page load more as it scrolls, or in pages of 50?
`;

/**
 * What Apply answers does here (SPEC-20), roughly: each answered question
 * becomes a requirement and leaves the list. The real run rewrites the ones
 * it decides instead.
 */
function applied(text: string): string {
  const qs = questions(text);
  const head = text.split(/^## Open questions/m)[0].trimEnd();
  const ids = requirements(text).map((r) => Number(r.id.slice(2)));
  let next = Math.max(0, ...ids);
  const added = qs.filter((q) => q.answer).map((q) =>
    `- R-${++next}: WHEN ${q.text.replace(/\?$/, "").replace(/^\w/, (c) => c.toLowerCase())} THE SYSTEM SHALL do as decided: ${q.answer}.`);
  const withReqs = head.replace(/(^- R-\d+[^\n]*\n)(?![\s\S]*^- R-\d+)/m, (m) => m + added.map((a) => a + "\n").join(""));
  const left = qs.filter((q) => !q.answer).map((q) => [`- ${q.id}: ${q.text}`, ...q.options.map((o) => `  - ${o}`)].join("\n"));
  return `${withReqs}\n\n## Open questions\n${left.length > 0 ? left.join("\n") : "None."}\n`;
}

function requirements(text: string) {
  const at = text.search(/^## (Requirements|Expected behaviour)/m);
  if (at < 0) return [];
  const part = text.slice(at).split(/^## (?!Unchanged)/m)[1] ?? "";
  return [...part.matchAll(/^- (?:\[[ xX]\] )?(R-\d+)[:.]? (.+)$/gm)].map((m) => ({ id: m[1], text: m[2].trim() }));
}

function steps(text: string) {
  return [...text.matchAll(/^- \[([ xX])\] (\d+)\. (.+)$/gm)].map((m) => ({
    number: Number(m[2]),
    text: m[3].trim(),
    done: m[1] !== " ",
    serves: [...(m[3].match(/\(([^)]*)\)\s*$/)?.[1] ?? "").matchAll(/(?:^|, )(R-\d+)/g)].map((x) => x[1]),
  }));
}

export function specAnswers(world: World): Record<string, (a: Record<string, unknown>) => unknown> {
  const kept = new Map<string, Kept>();
  const key = (task: string, checkout: string) => `${task}/${checkout}`;
  const get = (task: string, checkout: string): Kept => {
    let k = kept.get(key(task, checkout));
    if (!k) {
      k = { kind: "feature", approved: {}, drafts: {}, stale: [], check: null };
      kept.set(key(task, checkout), k);
    }
    return k;
  };
  const task = (id: string) => world.tasks.find((t) => t.id === id);

  // ACME-123, half way: the api's spec approved and two steps done, the
  // web's requirements approved and its design still a draft.
  if (task("t-login")) {
    Object.assign(get("t-login", "c-login-api"), {
      approved: { requirements: API_REQUIREMENTS, design: API_DESIGN, tasks: API_TASKS },
      check: { sha: "a1b2c3d4", at: Date.now() - 600_000, stale: true, results: [
        { id: "R-1", verdict: "met", evidence: "src/auth/session.ts:41, session.test.ts" },
        { id: "R-2", verdict: "not met", evidence: "no retry in login.ts yet" },
      ] },
    });
    Object.assign(get("t-login", "c-login-web"), {
      approved: { requirements: WEB_REQUIREMENTS },
      drafts: { design: DRAFT.design("web") },
    });
  }

  // ACME-130: requirements drafted, with questions, one of them answered.
  if (task("t-audit")) get("t-audit", "c-audit-web").drafts.requirements = AUDIT_DRAFT;

  const view = (taskId: string): TaskSpec => {
    const t = task(taskId);
    const repos: RepoSpec[] = (t?.checkouts ?? []).map((c) => {
      const k = get(taskId, c.id);
      const project = world.projects.find((p) => p.id === c.project_id);
      const inRepo = !project?.specs_in_app;
      return {
        checkout_id: c.id,
        repo: c.project_name,
        folder: c.project_name,
        home: inRepo ? `${c.project_name}/${project?.spec_folder ?? "specs"}/${t!.branch}` : `~/Library/Application Support/…/specs/${c.project_name}/${t!.branch}`,
        in_repo: inRepo,
        kind: k.kind,
        parts: PARTS.map((part) => ({
          part,
          file: part === "requirements" && k.kind === "bugfix" ? "bugfix.md" : `${part}.md`,
          approved: k.approved[part] ?? null,
          draft: k.drafts[part] ?? null,
          stale: k.stale.includes(part),
        })),
        requirements: requirements(k.approved.requirements ?? ""),
        steps: steps(k.approved.tasks ?? ""),
        check: k.check,
      };
    });
    return structuredClone({ repos, legacy: null });
  };

  return {
    read_spec: (a) => view(a.taskId as string),
    save_spec_draft: (a) => {
      const k = get(a.taskId as string, a.checkoutId as string);
      const part = a.part as SpecPart;
      if (a.text) k.drafts[part] = a.text as string;
      else delete k.drafts[part];
      return null;
    },
    approve_spec: (a) => {
      const k = get(a.taskId as string, a.checkoutId as string);
      if (a.kind) k.kind = a.kind as SpecKind;
      const texts = a.texts as { part: SpecPart; text: string }[];
      const first = Math.min(...texts.map((t) => PARTS.indexOf(t.part)));
      for (const { part, text } of texts) {
        if (text.trim()) k.approved[part] = text.trim() + "\n";
        else delete k.approved[part];
        delete k.drafts[part];
      }
      const done = texts.map((t) => t.part);
      k.stale = k.stale.filter((p) => !done.includes(p));
      for (const p of PARTS.slice(first + 1)) if (k.approved[p] && !done.includes(p) && !k.stale.includes(p)) k.stale.push(p);
      return view(a.taskId as string);
    },
    draft_spec: async (a) => {
      const t = task(a.taskId as string);
      const part = a.part as SpecPart;
      const targets = (t?.checkouts ?? []).filter((c) => part === "requirements" || !a.checkoutId || c.id === a.checkoutId);
      for (const c of targets) {
        const k = get(a.taskId as string, c.id);
        if (a.kind) k.kind = a.kind as SpecKind;
        let sent = "";
        for (const piece of DRAFT[part](c.project_name).match(/[^\n]*\n?/g) ?? []) {
          await new Promise((r) => setTimeout(r, 40));
          sent += piece;
          void emit("spec:draft", { request_id: a.requestId, checkout_id: c.id, text: sent });
        }
        k.drafts[part] = sent;
      }
      return null;
    },
    apply_spec_answers: async (a) => {
      const results: AppliedAnswers[] = [];
      for (const c of task(a.taskId as string)?.checkouts ?? []) {
        const k = get(a.taskId as string, c.id);
        const before = k.drafts.requirements ?? k.approved.requirements;
        if (!before || !questions(before).some((q) => q.answer)) continue;
        const after = applied(before);
        let sent = "";
        for (const piece of after.match(/[^\n]*\n?/g) ?? []) {
          await new Promise((r) => setTimeout(r, 30));
          sent += piece;
          void emit("spec:draft", { request_id: a.requestId, checkout_id: c.id, text: sent });
        }
        k.drafts.requirements = after;
        const old = requirements(before).map((r) => r.id);
        results.push({
          checkout_id: c.id, folder: c.project_name, changed: [],
          added: requirements(after).map((r) => r.id).filter((id) => !old.includes(id)), removed: [],
        });
      }
      if (results.length === 0) throw "no open question has an answer yet";
      return results;
    },
    check_spec: async (a) => {
      await new Promise((r) => setTimeout(r, 900));
      const k = get(a.taskId as string, a.checkoutId as string);
      const results: SpecVerdict[] = requirements(k.approved.requirements ?? "").map((r, i) => ({
        id: r.id, verdict: i === 0 ? "met" : "unclear", evidence: i === 0 ? "src/auth/session.ts:41" : "needs a run to tell",
      }));
      k.check = { sha: "e5f6a7b8", at: Date.now(), results, stale: false };
      return k.check;
    },
    spec_work_prompt: (a) => `Work through the steps in the approved tasks.md, and call spec_task with task id \`${a.taskId}\` as each is done.`,
    tell_spec_change: (a) => world.panes.filter((p) => p.task_id === a.taskId && p.running && p.kind === "agent").length,
    set_project_specs: (a) => {
      const p = world.projects.find((x) => x.id === a.projectId);
      if (p) {
        p.specs_in_app = a.inApp as boolean;
        p.spec_folder = (a.folder as string) === "specs" ? null : (a.folder as string);
      }
      return null;
    },
  };
}
