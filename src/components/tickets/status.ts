export function statusClass(category: string) {
  if (category === "done") return "done";
  if (category === "indeterminate") return "indeterminate";
  return "new";
}
