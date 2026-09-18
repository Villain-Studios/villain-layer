//! PTY-backed panes. Each pane is a real terminal: an agent CLI or a shell,
//! rooted in a workspace's worktree. Output is streamed to the frontend as
//! base64 so multi-byte sequences never get split by a chunk boundary.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;

use base64::Engine;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::error::{Error, Result};
use crate::shellenv;

/// Roughly one screenful of history per pane, replayed when React remounts it.
const SCROLLBACK_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneKind {
    Agent,
    Shell,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaneInfo {
    pub id: String,
    pub task_id: String,
    /// The repo this pane is rooted in; None means the task root, where every
    /// repo is visible as a sibling directory.
    pub checkout_id: Option<String>,
    pub kind: PaneKind,
    pub title: String,
    /// Agent id ("claude", "gemini", ...) when kind == Agent.
    pub agent_id: Option<String>,
    pub cwd: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub last_output_at: DateTime<Utc>,
}

struct PaneMeta {
    info: PaneInfo,
}

struct Pane {
    meta: Mutex<PaneMeta>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    scrollback: Mutex<Vec<u8>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnOptions {
    pub task_id: String,
    #[serde(default)]
    pub checkout_id: Option<String>,
    pub cwd: String,
    pub kind: PaneKind,
    pub title: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub rows: Option<u16>,
    #[serde(default)]
    pub cols: Option<u16>,
    /// Typed into the pane once it is up, for agents that take no prompt flag.
    #[serde(default)]
    pub initial_input: Option<String>,
}

#[derive(Default)]
pub struct PtyManager {
    panes: Mutex<HashMap<String, Arc<Pane>>>,
}

#[derive(Serialize, Clone)]
struct OutputEvent<'a> {
    pane_id: &'a str,
    data: String,
}

#[derive(Serialize, Clone)]
struct ExitEvent<'a> {
    pane_id: &'a str,
    code: Option<i32>,
}

impl PtyManager {
    pub fn spawn(&self, app: &AppHandle, opts: SpawnOptions) -> Result<PaneInfo> {
        let system = portable_pty::native_pty_system();
        let size = PtySize {
            rows: opts.rows.unwrap_or(30),
            cols: opts.cols.unwrap_or(100),
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = system
            .openpty(size)
            .map_err(|e| Error::Pty(format!("openpty: {e}")))?;

        let mut cmd = CommandBuilder::new(&opts.program);
        for arg in &opts.args {
            cmd.arg(arg);
        }
        cmd.cwd(&opts.cwd);
        for (k, v) in shellenv::user_env() {
            cmd.env(k, v);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::Pty(format!("spawn {}: {e}", opts.program)))?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::Pty(format!("clone reader: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| Error::Pty(format!("take writer: {e}")))?;
        let killer = child.clone_killer();

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let info = PaneInfo {
            id: id.clone(),
            task_id: opts.task_id.clone(),
            checkout_id: opts.checkout_id.clone(),
            kind: opts.kind,
            title: opts.title.clone(),
            agent_id: opts.agent_id.clone(),
            cwd: opts.cwd.clone(),
            running: true,
            exit_code: None,
            started_at: now,
            last_output_at: now,
        };

        let pane = Arc::new(Pane {
            meta: Mutex::new(PaneMeta { info: info.clone() }),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            scrollback: Mutex::new(Vec::new()),
        });

        self.panes.lock().insert(id.clone(), pane.clone());

        // Reader: pump PTY output to the frontend until EOF.
        {
            let app = app.clone();
            let pane = pane.clone();
            let id = id.clone();
            let mut reader = reader;
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let chunk = &buf[..n];
                            {
                                let mut sb = pane.scrollback.lock();
                                sb.extend_from_slice(chunk);
                                if sb.len() > SCROLLBACK_LIMIT {
                                    let drop_to = sb.len() - SCROLLBACK_LIMIT;
                                    sb.drain(..drop_to);
                                }
                            }
                            pane.meta.lock().info.last_output_at = Utc::now();
                            let data = base64::engine::general_purpose::STANDARD.encode(chunk);
                            let _ = app.emit(
                                "pty:output",
                                OutputEvent {
                                    pane_id: &id,
                                    data,
                                },
                            );
                        }
                    }
                }
            });
        }

        // Waiter: record the exit status and tell the UI.
        {
            let app = app.clone();
            let pane = pane.clone();
            let id = id.clone();
            std::thread::spawn(move || {
                let code = child.wait().ok().map(|s| s.exit_code() as i32);
                {
                    let mut meta = pane.meta.lock();
                    meta.info.running = false;
                    meta.info.exit_code = code;
                }
                let _ = app.emit("pty:exit", ExitEvent { pane_id: &id, code });
            });
        }

        if let Some(input) = opts.initial_input {
            let pane = pane.clone();
            std::thread::spawn(move || {
                // Give the agent's TUI a moment to draw its prompt first.
                std::thread::sleep(std::time::Duration::from_millis(1200));
                let mut w = pane.writer.lock();
                let _ = w.write_all(input.as_bytes());
                let _ = w.write_all(b"\r");
                let _ = w.flush();
            });
        }

        Ok(info)
    }

    fn get(&self, id: &str) -> Result<Arc<Pane>> {
        self.panes
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("pane {id}")))
    }

    pub fn write(&self, id: &str, data: &str) -> Result<()> {
        let pane = self.get(id)?;
        let mut w = pane.writer.lock();
        w.write_all(data.as_bytes())
            .map_err(|e| Error::Pty(format!("write: {e}")))?;
        w.flush().map_err(|e| Error::Pty(format!("flush: {e}")))?;
        Ok(())
    }

    pub fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<()> {
        let pane = self.get(id)?;
        let master = pane.master.lock();
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Pty(format!("resize: {e}")))?;
        Ok(())
    }

    pub fn scrollback(&self, id: &str) -> Result<String> {
        let pane = self.get(id)?;
        let sb = pane.scrollback.lock();
        Ok(base64::engine::general_purpose::STANDARD.encode(sb.as_slice()))
    }

    pub fn kill(&self, id: &str) -> Result<()> {
        let pane = self.get(id)?;
        let _ = pane.killer.lock().kill();
        Ok(())
    }

    pub fn close(&self, id: &str) -> Result<()> {
        if let Some(pane) = self.panes.lock().remove(id) {
            let _ = pane.killer.lock().kill();
        }
        Ok(())
    }

    pub fn list(&self, task_id: Option<&str>) -> Vec<PaneInfo> {
        let mut out: Vec<PaneInfo> = self
            .panes
            .lock()
            .values()
            .map(|p| p.meta.lock().info.clone())
            .filter(|i| task_id.is_none_or(|t| i.task_id == t))
            .collect();
        out.sort_by_key(|i| i.started_at);
        out
    }

    /// Kill every pane belonging to a task, used when it is deleted.
    pub fn close_task(&self, task_id: &str) {
        self.close_matching(|i| i.task_id == task_id);
    }

    /// Kill every pane rooted in one checkout, used when a repo leaves a task.
    pub fn close_checkout(&self, checkout_id: &str) {
        self.close_matching(|i| i.checkout_id.as_deref() == Some(checkout_id));
    }

    fn close_matching(&self, pred: impl Fn(&PaneInfo) -> bool) {
        let ids: Vec<String> = self
            .panes
            .lock()
            .iter()
            .filter(|(_, p)| pred(&p.meta.lock().info))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let _ = self.close(&id);
        }
    }
}
