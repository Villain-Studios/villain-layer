import { useMemo, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { FoundRepo } from "../lib/types";
import { Combo, Field, Modal, Spinner } from "./ui";

/**
 * Adding repos one folder-picker at a time does not scale past a handful, so
 * this offers two routes: pick several folders, or point at a parent folder and
 * register every git repo found underneath.
 */
export function AddRepos({ onClose }: { onClose: () => void }) {
  const refreshAll = useStore((s) => s.refreshAll);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [found, setFound] = useState<FoundRepo[] | null>(null);
  const [scanned, setScanned] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const [scanning, setScanning] = useState(false);
  const [busy, setBusy] = useState(false);
  const [group, setGroup] = useState("");

  const projects = useStore((s) => s.projects);
  const existingGroups = useMemo(
    () => [...new Set(projects.map((p) => p.group).filter(Boolean))] as string[],
    [projects],
  );

  async function pickFolders() {
    const chosen = await openDialog({
      directory: true,
      multiple: true,
      title: "Choose one or more git repositories",
    });
    const paths = Array.isArray(chosen) ? chosen : typeof chosen === "string" ? [chosen] : [];
    if (paths.length === 0) return;
    await register(paths);
  }

  async function scan() {
    const root = await openDialog({
      directory: true,
      title: "Choose a folder that contains your repositories",
    });
    if (typeof root !== "string") return;

    setScanning(true);
    setFound(null);
    try {
      const repos = await api.scanRepos(root, 3);
      setScanned(root);
      setFound(repos);
      setPicked(repos.filter((r) => !r.registered).map((r) => r.path));
      // A folder of repos is usually already a group: ~/code/backend -> backend.
      const leaf = root.split("/").filter(Boolean).pop() ?? "";
      setGroup(leaf.toLowerCase());
      if (repos.length === 0) toast("info", `No git repositories found under ${root}`);
    } catch (e) {
      fail(e);
    } finally {
      setScanning(false);
    }
  }

  async function register(paths: string[]) {
    setBusy(true);
    try {
      const added = await api.addProjects(paths, group.trim() || null);
      await refreshAll();
      toast("success", `Added ${added.length} repositor${added.length === 1 ? "y" : "ies"}`);
      onClose();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  const selectable = found?.filter((r) => !r.registered) ?? [];

  return (
    <Modal
      title="Add repositories"
      wide
      onClose={onClose}
      footer={
        found ? (
          <>
            <button className="btn" onClick={onClose}>Cancel</button>
            <button
              className="btn btn-primary"
              disabled={busy || picked.length === 0}
              onClick={() => void register(picked)}
            >
              {busy ? "Adding…" : `Add ${picked.length} repositor${picked.length === 1 ? "y" : "ies"}`}
            </button>
          </>
        ) : undefined
      }
    >
      {!found && (
        <>
          <div className="row" style={{ gap: 10, marginBottom: 14 }}>
            <button
              className="btn btn-primary"
              disabled={scanning}
              onClick={() => void scan().catch(fail)}
            >
              {scanning ? "Scanning…" : "Scan a folder…"}
            </button>
            <button className="btn" disabled={busy} onClick={() => void pickFolders().catch(fail)}>
              Pick folders…
            </button>
            {scanning && <Spinner />}
          </div>
          <div className="muted" style={{ lineHeight: 1.6 }}>
            <b>Scan</b> points at a parent folder — say <code>~/code</code> — and finds every git
            repository up to three levels down, so a whole stack goes in at once.
            <br />
            <b>Pick</b> selects specific repository folders; hold ⌘ to choose several.
            <br /><br />
            Repositories must already be cloned locally. A task later picks whichever of
            them its change touches.
          </div>
        </>
      )}

      {found && (
        <>
          <div className="row" style={{ marginBottom: 10 }}>
            <span className="muted">
              {found.length} found under <code>{scanned}</code>
            </span>
            <div className="spacer" />
            <button
              className="btn btn-sm"
              onClick={() => setPicked(selectable.map((r) => r.path))}
            >
              All
            </button>
            <button className="btn btn-sm" onClick={() => setPicked([])}>None</button>
            <button className="btn btn-sm" onClick={() => setFound(null)}>Back</button>
          </div>

          <Field
            label="Group"
            hint="Repos added together land in one group, which is how the sidebar and the task picker organise them. Leave blank for ungrouped."
          >
            <Combo
              value={group}
              options={existingGroups}
              onChange={setGroup}
              placeholder="backend"
              width="100%"
            />
          </Field>

          <div className="repo-picker" style={{ maxHeight: 280 }}>
            {found.map((r) => (
              <label
                key={r.path}
                className="repo-pick"
                style={r.registered ? { opacity: 0.45, cursor: "default" } : undefined}
              >
                <input
                  type="checkbox"
                  disabled={r.registered}
                  checked={r.registered || picked.includes(r.path)}
                  onChange={() =>
                    setPicked((p) =>
                      p.includes(r.path) ? p.filter((x) => x !== r.path) : [...p, r.path],
                    )
                  }
                />
                <span style={{ flex: 1 }}>{r.name}</span>
                {r.branch && <span className="chip">{r.branch}</span>}
                {r.registered && <span className="chip add">added</span>}
                <span className="path" style={{ maxWidth: 240 }}>{r.path}</span>
              </label>
            ))}
          </div>
        </>
      )}
    </Modal>
  );
}
