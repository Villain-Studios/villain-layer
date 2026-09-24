#!/usr/bin/env bun
/**
 * The rules a compiler cannot check.
 *
 * Each of these has already cost something once: a window that froze while a
 * command waited on git, an app that quit with six agents in it over one
 * `unwrap`, a button calling a command that had been renamed. A failure names
 * the rule, the place, and what to do instead, so whoever meets it — a person
 * or an agent — can fix it without reading this file.
 *
 * An exception is written beside the code it excuses, on the same line or up
 * to three lines above:
 *
 *     // guard: allow <rule> — <why this one is fine>
 *
 * The reason is required. It is what the reviewer reads.
 *
 * Run by `bun run check`, by .githooks/pre-commit and by CI.
 */
import { existsSync, readFileSync } from "node:fs";

process.chdir(new URL("..", import.meta.url).pathname);

type Problem = { rule: string; file: string; line: number; message: string };
const problems: Problem[] = [];

function report(rule: string, file: string, line: number, message: string) {
  problems.push({ rule, file, line, message });
}

// ---------------------------------------------------------------------------
// Files

const listed = Bun.spawnSync(["git", "ls-files", "--cached", "--others", "--exclude-standard"]);
const files = new TextDecoder()
  .decode(listed.stdout)
  .split("\n")
  .filter((f) => f && existsSync(f));

const cache = new Map<string, string>();
function read(file: string): string {
  let text = cache.get(file);
  if (text === undefined) {
    text = readFileSync(file, "utf8");
    cache.set(file, text);
  }
  return text;
}

const rustFiles = files.filter((f) => f.startsWith("src-tauri/src/") && f.endsWith(".rs"));
/** The app's own frontend. The mock harness is a dev tool and may do what the app may not. */
const tsFiles = files.filter(
  (f) => f.startsWith("src/") && /\.(ts|tsx)$/.test(f) && !f.startsWith("src/__mock__/"),
);

function lineAt(text: string, index: number): number {
  let n = 1;
  for (let i = 0; i < index && i < text.length; i++) if (text.charCodeAt(i) === 10) n++;
  return n;
}

/**
 * Whether `line` (1-based) carries an exception for `rule`. A marker without
 * a reason is itself a problem: an exception nobody can judge is not one.
 */
function allowed(file: string, line: number, rule: string): boolean {
  const lines = read(file).split("\n");
  for (let i = Math.max(0, line - 4); i < line; i++) {
    const m = lines[i]?.match(new RegExp(`guard: allow ${rule}\\b(.*)$`));
    if (!m) continue;
    if (m[1].replace(/[\s—:-]/g, "").length < 4) {
      report(rule, file, i + 1, `This exception has no reason. Write why: // guard: allow ${rule} — <why>.`);
    }
    return true;
  }
  return false;
}

// ---------------------------------------------------------------------------
// Rust source, with string, char and comment contents blanked out, so that
// braces and words inside them are not mistaken for code. Same length and
// same line breaks as the original, so an index in one is an index in both.

function maskRust(src: string): string {
  const out = src.split("");
  const n = src.length;
  const blank = (from: number, to: number) => {
    for (let k = from; k < to && k < n; k++) if (out[k] !== "\n") out[k] = " ";
  };
  const ident = (c: string | undefined) => !!c && /[A-Za-z0-9_]/.test(c);
  let i = 0;
  while (i < n) {
    const c = src[i];
    const next = src[i + 1];
    if (c === "/" && next === "/") {
      const end = src.indexOf("\n", i);
      const stop = end === -1 ? n : end;
      blank(i, stop);
      i = stop;
    } else if (c === "/" && next === "*") {
      let depth = 1;
      let j = i + 2;
      while (j < n && depth > 0) {
        if (src[j] === "/" && src[j + 1] === "*") { depth++; j += 2; }
        else if (src[j] === "*" && src[j + 1] === "/") { depth--; j += 2; }
        else j++;
      }
      blank(i, j);
      i = j;
    } else if (
      c === "r" &&
      (next === '"' || next === "#") &&
      (!ident(src[i - 1]) || (src[i - 1] === "b" && !ident(src[i - 2])))
    ) {
      let j = i + 1;
      let hashes = 0;
      while (src[j] === "#") { hashes++; j++; }
      if (src[j] !== '"') { i++; continue; }
      const close = '"' + "#".repeat(hashes);
      const end = src.indexOf(close, j + 1);
      const stop = end === -1 ? n : end;
      blank(j + 1, stop);
      i = stop + close.length;
    } else if (c === '"') {
      let j = i + 1;
      while (j < n && src[j] !== '"') j += src[j] === "\\" ? 2 : 1;
      blank(i + 1, j);
      i = j + 1;
    } else if (c === "'") {
      // A char literal, or a lifetime ('a, 'static), which is left alone.
      if (next === "\\") {
        const end = src.indexOf("'", i + 3);
        blank(i + 1, end);
        i = end + 1;
      } else if (src[i + 2] === "'") {
        blank(i + 1, i + 2);
        i += 3;
      } else if (/[\uD800-\uDBFF]/.test(next ?? "") && src[i + 3] === "'") {
        blank(i + 1, i + 3);
        i += 4;
      } else i++;
    } else i++;
  }
  return out.join("");
}

/**
 * Blank out test-only code: a `#[cfg(test)]` item runs to the next `}` at
 * the attribute's own indentation, which is where rustfmt closes it — column
 * 0 for a module, deeper for a method inside an `impl`.
 */
function withoutTests(masked: string): string {
  const lines = masked.split("\n");
  // Blanked, not emptied: an index in this is read back out of the raw
  // source. Emptied, everything after a test-only method mid-file (a
  // `for_tests` constructor) moved, and an event emitted below one was read
  // as whatever text sat that many characters earlier.
  const drop = (k: number) => { lines[k] = " ".repeat(lines[k].length); };
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].trim() !== "#[cfg(test)]") continue;
    const close = lines[i].slice(0, lines[i].indexOf("#")) + "}";
    let j = i + 1;
    while (j < lines.length && lines[j].trim().startsWith("#[")) j++;
    if (lines[j]?.trimEnd().endsWith(";")) {
      for (let k = i; k <= j; k++) drop(k);
      i = j;
      continue;
    }
    while (j < lines.length && !lines[j].startsWith(close)) j++;
    for (let k = i; k <= j && k < lines.length; k++) drop(k);
    i = j;
  }
  return lines.join("\n");
}

/** Index of the bracket closing the one at `open`, in masked text. */
function closing(masked: string, open: number): number {
  const pairs: Record<string, string> = { "{": "}", "(": ")", "[": "]" };
  const want = pairs[masked[open]];
  let depth = 0;
  for (let i = open; i < masked.length; i++) {
    const c = masked[i];
    if (c === masked[open]) depth++;
    else if (c === want && --depth === 0) return i;
  }
  return masked.length;
}

const rust = new Map<string, { raw: string; masked: string; code: string }>();
for (const f of rustFiles) {
  const raw = read(f);
  const masked = maskRust(raw);
  rust.set(f, { raw, masked, code: withoutTests(masked) });
}

// ---------------------------------------------------------------------------
// Tauri commands: the Rust side

type Param = { name: string; optional: boolean };
type Command = {
  name: string;
  file: string;
  line: number;
  async: boolean;
  params: Param[];
  body: [number, number];
};

/** Parameters Tauri fills in itself, which the frontend never sends. */
const INJECTED = /\b(State|AppHandle|Window|WebviewWindow|Webview)\b/;

function camel(snake: string): string {
  return snake.replace(/_([a-z0-9])/g, (_, c: string) => c.toUpperCase());
}

function splitTopLevel(s: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if ("<([{".includes(c)) depth++;
    else if (">)]}".includes(c)) depth--;
    else if (c === "," && depth === 0) {
      parts.push(s.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(s.slice(start));
  return parts.map((p) => p.trim()).filter(Boolean);
}

const commands: Command[] = [];
for (const [file, { code }] of rust) {
  const re = /#\[tauri::command[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?(async\s+)?fn\s+(\w+)\s*(?:<[^>]*>)?\s*\(/g;
  for (let m; (m = re.exec(code)); ) {
    const open = m.index + m[0].length - 1;
    const close = closing(code, open);
    const params = splitTopLevel(code.slice(open + 1, close))
      .map((p) => {
        const colon = p.indexOf(":");
        return { name: p.slice(0, colon).replace(/^mut\s+/, "").trim(), type: p.slice(colon + 1).trim() };
      })
      .filter((p) => !INJECTED.test(p.type))
      .map((p) => ({ name: p.name, optional: p.type.startsWith("Option<") }));
    const bodyOpen = code.indexOf("{", close);
    commands.push({
      name: m[2],
      file,
      line: lineAt(code, m.index),
      async: !!m[1],
      params,
      body: [bodyOpen, closing(code, bodyOpen)],
    });
  }
}

const libRs = "src-tauri/src/lib.rs";
const handler = rust.get(libRs)!.code.match(/generate_handler!\s*\[([\s\S]*?)\]/);
const registered = new Set([...(handler?.[1] ?? "").matchAll(/(?:\w+::)*(\w+)/g)].map((m) => m[1]));

for (const c of commands) {
  if (!registered.has(c.name)) {
    report("command-registered", c.file, c.line,
      `\`${c.name}\` is a #[tauri::command] that is not in generate_handler! in ${libRs}, so nothing can call it. Register it, or delete it.`);
  }
}

// ---------------------------------------------------------------------------
// Tauri commands: the frontend side. Everything goes through src/lib/api.ts.

const apiTs = "src/lib/api.ts";
const api = read(apiTs);

for (const f of tsFiles) {
  if (f === apiTs) continue;
  const text = read(f);
  const m = text.match(/import\s*\{[^}]*\binvoke\b[^}]*\}\s*from\s*["']@tauri-apps\/api\/core["']/);
  if (m) {
    report("api-only", f, lineAt(text, m.index!),
      `Calls \`invoke\` directly. Every command goes through ${apiTs}, which is the one place the frontend's view of the backend is written down — add a wrapper there and call that.`);
  }
}

/** Skip over a JS string or template starting at `i`; returns the index after it. */
function skipJsString(s: string, i: number): number {
  const q = s[i];
  let j = i + 1;
  while (j < s.length && s[j] !== q) j += s[j] === "\\" ? 2 : 1;
  return j + 1;
}

/** Top-level keys of the object literal opening at `open`, or null when it spreads. */
function objectKeys(s: string, open: number): string[] | null {
  const keys: string[] = [];
  let depth = 0;
  let expectKey = true;
  for (let i = open; i < s.length; i++) {
    const c = s[i];
    if (c === '"' || c === "'" || c === "`") { i = skipJsString(s, i) - 1; continue; }
    if ("{([".includes(c)) { depth++; if (depth === 1) expectKey = true; continue; }
    if ("})]".includes(c)) { if (--depth === 0) return keys; continue; }
    if (depth !== 1) continue;
    if (c === ",") { expectKey = true; continue; }
    if (expectKey && /[A-Za-z_$.]/.test(c)) {
      if (s.startsWith("...", i)) return null;
      const m = s.slice(i).match(/^[A-Za-z_$][\w$]*/);
      if (m) { keys.push(m[0]); i += m[0].length - 1; }
      expectKey = false;
    }
  }
  return keys;
}

const invoked = new Map<string, number>();
const byName = new Map(commands.map((c) => [c.name, c]));
for (const m of api.matchAll(/\binvoke\s*(?:<[\s\S]*?>)?\s*\(\s*"(\w+)"\s*(,\s*\{)?/g)) {
  const name = m[1];
  const line = lineAt(api, m.index!);
  invoked.set(name, line);
  const cmd = byName.get(name);
  if (!cmd) {
    report("api-command-exists", apiTs, line,
      `Invokes \`${name}\`, which is not a #[tauri::command] in src-tauri/src. It fails at runtime with "command not found". Fix the name, or remove the wrapper.`);
    continue;
  }
  if (!m[2]) {
    const needed = cmd.params.filter((p) => !p.optional);
    if (needed.length) {
      report("api-args", apiTs, line,
        `\`${name}\` needs ${needed.map((p) => camel(p.name)).join(", ")} (${cmd.file}:${cmd.line}), and is invoked with no arguments.`);
    }
    continue;
  }
  const keys = objectKeys(api, m.index! + m[0].length - 1);
  if (!keys) continue;
  const expected = new Map(cmd.params.map((p) => [camel(p.name), p]));
  for (const k of keys) {
    if (!expected.has(k)) {
      const hint = k.includes("_") ? ` Tauri renames Rust's snake_case parameters to camelCase — send \`${camel(k)}\`.` : "";
      report("api-args", apiTs, line,
        `\`${name}\` is sent \`${k}\`, which is not one of its parameters (${[...expected.keys()].join(", ") || "none"}; ${cmd.file}:${cmd.line}).${hint}`);
    }
  }
  for (const [k, p] of expected) {
    if (!p.optional && !keys.includes(k)) {
      report("api-args", apiTs, line,
        `\`${name}\` needs \`${k}\` (${cmd.file}:${cmd.line}); without it the call fails at runtime.`);
    }
  }
}

for (const c of commands) {
  if (registered.has(c.name) && !invoked.has(c.name)) {
    report("dead-command", c.file, c.line,
      `\`${c.name}\` is registered but nothing in ${apiTs} invokes it. Dead code goes: remove it from its module, from generate_handler! and from the types on both sides.`);
  }
}

// Wrappers nobody calls are dead too.
const apiBody = api.slice(api.indexOf("export const api = {"));
const wrappers = [...apiBody.matchAll(/^ {2}(\w+):/gm)].map((m) => m[1]);
const everythingElse = tsFiles.filter((f) => f !== apiTs).map(read).join("\n");
for (const w of wrappers) {
  if (!new RegExp(`\\bapi\\.${w}\\b`).test(everythingElse)) {
    report("dead-wrapper", apiTs, lineAt(api, api.indexOf(`  ${w}:`)),
      `\`api.${w}\` is never called. Dead code goes — remove it, and its command if nothing else uses that.`);
  }
}

// ---------------------------------------------------------------------------
// Events: what the backend emits and what the frontend listens for

const emitted = new Map<string, { file: string; line: number }>();
for (const [file, { raw, code }] of rust) {
  for (const m of code.matchAll(/\.emit\(\s*"/g)) {
    const start = m.index! + m[0].length;
    const name = raw.slice(start, raw.indexOf('"', start));
    if (!emitted.has(name)) emitted.set(name, { file, line: lineAt(raw, m.index!) });
  }
}
const listened = new Map<string, { file: string; line: number }>();
for (const f of tsFiles) {
  const text = read(f);
  for (const m of text.matchAll(/\blisten\s*(?:<[\s\S]*?>)?\s*\(\s*"([^"]+)"/g)) {
    if (!listened.has(m[1])) listened.set(m[1], { file: f, line: lineAt(text, m.index!) });
  }
}
for (const [name, at] of emitted) {
  if (!listened.has(name)) {
    report("event-sync", at.file, at.line,
      `Emits "${name}", which nothing in src/ listens for. Either the listener was removed and this should go too, or the name is wrong.`);
  }
}
for (const [name, at] of listened) {
  if (!emitted.has(name)) {
    report("event-sync", at.file, at.line,
      `Listens for "${name}", which the backend never emits. The UI will wait for it forever.`);
  }
}

// ---------------------------------------------------------------------------
// Responsiveness

/**
 * Things that wait: on git, on a child, on the disk, on the network. A plain
 * `#[tauri::command] fn` runs on the main thread, where waiting freezes the
 * window; an async one runs on the runtime's workers, which the MCP server
 * and the HTTP clients share.
 */
const WAITS = /\bgit::\w+|Command::new|thread::sleep|\bblock_on\b|\bread_dir\b|\.wait\(\)|\.wait_with_output\(\)|\bstd::process::/g;
/** Calls that move a closure onto a thread of its own. */
const OFFLOADS = /\b(blocking|off_runtime|spawn_blocking|thread::spawn|thread::scope|std::thread::Builder)\b/g;

for (const c of commands) {
  const { code } = rust.get(c.file)!;
  const [from, to] = c.body;
  const body = code.slice(from, to);
  const spans: [number, number][] = [];
  for (const m of body.matchAll(OFFLOADS)) {
    const open = body.indexOf("(", m.index!);
    if (open !== -1) spans.push([open, closing(body, open)]);
  }
  for (const m of body.matchAll(WAITS)) {
    if (spans.some(([a, b]) => m.index! > a && m.index! < b)) continue;
    const line = lineAt(code, from + m.index!);
    const rule = c.async ? "runtime-blocking" : "main-thread";
    if (allowed(c.file, line, rule)) continue;
    report(rule, c.file, line, c.async
      ? `\`${m[0]}\` directly in async command \`${c.name}\`. Async commands run on the tokio workers the MCP server and the Jira/GitHub/Slack clients share; waiting on one stalls them. Wrap the work: commands::blocking(app, move |state| …) or commands::off_runtime(move || …).`
      : `\`${m[0]}\` in \`${c.name}\`, a plain #[tauri::command] fn — those run on the main thread, and while it waits no event reaches the webview: terminals stop printing and the window stops answering. Make the command async and move the work into commands::blocking.`);
  }
}

for (const f of tsFiles) {
  const lines = read(f).split("\n");
  lines.forEach((l, i) => {
    if (!/\bsetInterval\s*\(/.test(l) || /^\s*(\/\/|\*)/.test(l)) return;
    if (allowed(f, i + 1, "poll")) return;
    report("poll", f, i + 1,
      `A new setInterval. macOS throttles a hidden webview's timers, and every poll is work repeated whether or not anything changed. Prefer an event from the backend (see docs/architecture.md, "Events"). If polling really is the answer, poll only while \`appActive\`, and say why: // guard: allow poll — <why an event cannot do this>.`);
  });
}

// ---------------------------------------------------------------------------
// Robustness and hygiene

const PANICS = /\.unwrap\(\)|\.expect\(|\bpanic!\(|\btodo!\(|\bunimplemented!\(|\bunreachable!\(|\bdbg!\(/g;
for (const [file, { code }] of rust) {
  for (const m of code.matchAll(PANICS)) {
    const line = lineAt(code, m.index!);
    if (allowed(file, line, "panic")) continue;
    report("panic", file, line,
      `\`${m[0]}\` outside tests. Release builds abort on a panic (panic = "abort"), and an abort takes every running agent down with the app. Return an error (\`?\`, ok_or, unwrap_or_else) instead.`);
  }
}

for (const [file, { code }] of rust) {
  if (file === "src-tauri/src/git.rs") continue;
  for (const m of code.matchAll(/Command::new\(\s*"git"/g)) {
    report("git-only-in-git-rs", file, lineAt(code, m.index!),
      `Runs git outside git.rs. git.rs is the only place that shells out to git, because it is where the flags that defend against the user's own git config live (see docs/architecture.md, "Git"). Add a function there.`);
  }
}

for (const [file, { masked, code }] of rust) {
  const at = masked.search(/^#\[cfg\(test\)\]\s*\n\s*mod\s+\w+/m);
  if (at === -1) continue;
  const open = masked.indexOf("{", at);
  const rest = masked.slice(closing(masked, open) + 1);
  if (rest.trim()) {
    report("tests-last", file, lineAt(masked, at),
      "The test module is not the last thing in the file. Keep it last: the size ceiling and the panic rule count everything above it as shipped code.");
  }
  void code;
}

for (const f of tsFiles) {
  const lines = read(f).split("\n");
  lines.forEach((l, i) => {
    if (/^\s*(\/\/|\*)/.test(l)) return;
    const m = l.match(/\bconsole\.(log|debug|info)\s*\(|\bdebugger\b/);
    if (m && !allowed(f, i + 1, "console")) {
      report("console", f, i + 1, `\`${m[0]}\` left in. Remove it; for something the user should see, use the store's \`toast\`.`);
    }
  });
}

// ---------------------------------------------------------------------------
// Size. Big files are where the next change lands whether or not it belongs
// there, so a file stops growing at its ceiling and is split instead. Rust
// counts only the code above its test module.

const DEFAULT_CEILING = 600;
/**
 * Files already past the default, each with a little headroom. Raising one
 * is allowed; say in the pull request why the growth belongs in that file
 * rather than in a new one.
 */
const CEILINGS: Record<string, number> = {
  "src/styles.css": 1800,
  "src/components/DiffView.tsx": 1100,
  "src/components/PrPanel.tsx": 800,
  "src/store.ts": 800,
  "src/components/Settings.tsx": 700,
  "src/components/Terminals.tsx": 700,
  "src-tauri/src/commands/jira.rs": 1400,
  "src-tauri/src/pty.rs": 1350,
  "src-tauri/src/commands/github.rs": 1100,
  "src-tauri/src/integrations/github.rs": 1050,
  "src-tauri/src/commands/tasks.rs": 1050,
  "src-tauri/src/mcp.rs": 1000,
  "src-tauri/src/commands/panes.rs": 1000,
  "src-tauri/src/integrations/jira.rs": 900,
  "src-tauri/src/agents.rs": 850,
  "src-tauri/src/git.rs": 1150,
  "src-tauri/src/commands/diff.rs": 700,
};

for (const f of files) {
  if (!/^(src|src-tauri\/src)\/.*\.(ts|tsx|css|rs)$/.test(f) || f.startsWith("src/__mock__/")) continue;
  let count: number;
  if (f.endsWith(".rs")) {
    const { masked } = rust.get(f)!;
    const at = masked.search(/^#\[cfg\(test\)\]\s*\n\s*mod\s+\w+/m);
    count = lineAt(masked, at === -1 ? masked.length : at);
  } else {
    count = read(f).split("\n").length;
  }
  const ceiling = CEILINGS[f] ?? DEFAULT_CEILING;
  if (count > ceiling) {
    report("file-size", f, count,
      `${count} lines, past its ceiling of ${ceiling}. Split it along a seam instead of growing it (docs/recipes.md, "Splitting a file"). If the growth really belongs here, raise its entry in scripts/guard.ts and say why in the pull request.`);
  }
}
for (const f of Object.keys(CEILINGS)) {
  if (!files.includes(f)) report("file-size", "scripts/guard.ts", 1, `CEILINGS lists ${f}, which no longer exists. Remove the entry.`);
}

// ---------------------------------------------------------------------------
// Docs that describe a list in the code have to keep up with it.

const features = existsSync("docs/features.md") ? read("docs/features.md") : "";
const architecture = existsSync("docs/architecture.md") ? read("docs/architecture.md") : "";

const mcp = rust.get("src-tauri/src/mcp.rs");
if (mcp) {
  for (const m of mcp.code.matchAll(/\btool\(\s*"/g)) {
    const start = m.index! + m[0].length;
    const name = mcp.raw.slice(start, mcp.raw.indexOf('"', start));
    if (!features.includes(`\`${name}\``)) {
      report("docs-sync", "src-tauri/src/mcp.rs", lineAt(mcp.raw, m.index!),
        `MCP tool \`${name}\` is not in the tool table in docs/features.md. Agents' users read that table to know what an agent can do on their behalf.`);
    }
  }
}

const agentsRs = rust.get("src-tauri/src/agents.rs");
if (agentsRs) {
  for (const m of agentsRs.code.matchAll(/\bid:\s*"/g)) {
    const start = m.index! + m[0].length;
    const id = agentsRs.raw.slice(start, agentsRs.raw.indexOf('"', start));
    if (!features.includes(`\`${id}\``)) {
      report("docs-sync", "src-tauri/src/agents.rs", lineAt(agentsRs.raw, m.index!),
        `Agent CLI \`${id}\` is not in the agent table in docs/features.md.`);
    }
  }
}

for (const [name, at] of emitted) {
  if (!architecture.includes(`\`${name}\``)) {
    report("docs-sync", at.file, at.line, `Event \`${name}\` is not in the events table in docs/architecture.md.`);
  }
}

// ---------------------------------------------------------------------------

if (problems.length === 0) {
  console.log(`guard: ok — ${commands.length} commands, ${emitted.size} events, ${files.length} files checked`);
  process.exit(0);
}

problems.sort((a, b) => a.file.localeCompare(b.file) || a.line - b.line);
for (const p of problems) console.error(`${p.file}:${p.line}  [${p.rule}]\n  ${p.message}\n`);
console.error(`guard: ${problems.length} problem${problems.length === 1 ? "" : "s"}. Each rule is explained in AGENTS.md, "Guard rules".`);
process.exit(1);
