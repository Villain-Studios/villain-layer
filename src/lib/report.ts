import type { RepoResult } from "./types";

type Toast = (kind: "info" | "error" | "success", text: string) => void;

/**
 * Toast the outcome of a multi-repo operation.
 *
 * Each repo answers on its own, so a failure in one must not swallow successes
 * in the others — and an empty reply is worth saying out loud rather than
 * leaving the click looking like it did nothing.
 */
export function reportRepoResults(toast: Toast, results: RepoResult[], verb: string) {
  const ok = results.filter((r) => r.ok);
  const bad = results.filter((r) => !r.ok);
  if (bad.length) {
    toast("error", bad.map((r) => `${r.repo}: ${r.detail}`).join("\n"));
  }
  if (ok.length) {
    toast("success", `${verb} ${ok.map((r) => `${r.repo} (${r.detail})`).join(", ")}`);
  }
  if (!ok.length && !bad.length) {
    toast("info", "Nothing to do — no repository had changes.");
  }
}
