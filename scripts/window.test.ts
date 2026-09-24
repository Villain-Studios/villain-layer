import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

// The window has no native title bar, so the top bar is the only thing it
// can be moved by (WIN-1). None of this runs in the mock harness, which is a
// browser, so it is held here: what the app ships with.
const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

describe("the window", () => {
  test("is moved by dragging the top bar, which Tauri is told about", () => {
    expect(read("src/App.tsx")).toMatch(/className="topbar" data-tauri-drag-region="deep"/);
    const caps = JSON.parse(read("src-tauri/capabilities/default.json"));
    expect(caps.permissions).toContain("core:window:allow-start-dragging");
  });

  test("is not left relying on CSS that macOS's WebKit ignores", () => {
    expect(read("src/styles.css")).not.toContain("-webkit-app-region");
  });
});
