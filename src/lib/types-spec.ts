/** A task's specs (§19), mirrored from `commands/spec.rs`. */

/** One of a spec's files, in the order they are drafted and approved (SPEC-4). */
export type SpecPart = "requirements" | "design" | "tasks";
export type SpecKind = "feature" | "bugfix";

export interface SpecRequirement {
  id: string;
  text: string;
}

/** One step of `tasks.md`. `serves`: this repository's requirements it names. */
export interface SpecStep {
  number: number;
  text: string;
  done: boolean;
  serves: string[];
}

export interface SpecVerdict {
  id: string;
  verdict: "met" | "not met" | "unclear";
  evidence: string;
}

/** The last check against the spec (SPEC-15). `stale`: the branch moved on since. */
export interface SpecCheck {
  sha: string;
  at: number;
  results: SpecVerdict[];
  stale: boolean;
}

export interface SpecPartView {
  part: SpecPart;
  file: string;
  approved: string | null;
  draft: string | null;
  /** Approved, but an earlier file was approved since (SPEC-7). */
  stale: boolean;
}

/** One repository's spec in a task. */
export interface RepoSpec {
  checkout_id: string;
  repo: string;
  folder: string;
  /** Where it is kept: `api/specs/ACME-1-x` in the repository, or a path in the app's folder. */
  home: string;
  in_repo: boolean;
  kind: SpecKind;
  parts: SpecPartView[];
  requirements: SpecRequirement[];
  steps: SpecStep[];
  check: SpecCheck | null;
}

/** What Apply answers did to one repository's requirements, by id (SPEC-20). */
export interface AppliedAnswers {
  checkout_id: string;
  folder: string;
  changed: string[];
  added: string[];
  removed: string[];
}

export interface TaskSpec {
  repos: RepoSpec[];
  /** A `SPEC.md` from before, to start the requirements from (SPEC-17). */
  legacy: string | null;
}
