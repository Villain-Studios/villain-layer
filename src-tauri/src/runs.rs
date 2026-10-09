//! Agent run history, stored locally only (no telemetry, no external export).
//!
//! A **run** is one agent pane from start to exit: which agent, how long it
//! took, whether it succeeded, errors it hit, and tokens used when available.
//! Runs are kept in `runs.json` alongside the config, queried for the
//! dashboard, and can be cleared by the user.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// One agent run from start to exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub agent_id: String,
    pub task_id: String,
    pub task_name: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub branch: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_secs: f64,
    /// Exit code when available.
    pub exit_code: Option<i32>,
    /// Success: clean exit (code 0); failure: non-zero exit or error;
    /// stopped: user stopped it; error: could not run.
    pub result: RunResult,
    /// Loop rounds completed before passing or giving up, when on a loop.
    pub loop_rounds: Option<u32>,
    /// Errors encountered during the run.
    pub error_count: u32,
    /// Token usage, when the agent reports it. Not all agents do.
    pub tokens: Option<TokenUsage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunResult {
    Success,
    Failure,
    Stopped,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
}

/// Runs kept in memory and persisted to disk.
pub struct RunStore {
    path: PathBuf,
    runs: RwLock<Vec<AgentRun>>,
}

impl RunStore {
    pub fn new(app_dir: &std::path::Path) -> Arc<Self> {
        let path = app_dir.join("runs.json");
        let runs = Self::load(&path);
        Arc::new(Self {
            path,
            runs: RwLock::new(runs),
        })
    }

    fn load(path: &std::path::Path) -> Vec<AgentRun> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    fn save(&self) {
        let runs = self.runs.read();
        if let Ok(json) = serde_json::to_vec_pretty(&*runs) {
            let tmp = self.path.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    /// Record one run. Keeps the most recent 1000 runs.
    pub fn record(&self, run: AgentRun) {
        const MAX_RUNS: usize = 1000;
        let mut runs = self.runs.write();
        runs.insert(0, run);
        if runs.len() > MAX_RUNS {
            runs.truncate(MAX_RUNS);
        }
        drop(runs);
        self.save();
    }

    /// All runs, newest first.
    pub fn list(&self) -> Vec<AgentRun> {
        self.runs.read().clone()
    }

    /// Filtered runs: by agent, by project, both or neither.
    pub fn filter(&self, agent_id: Option<&str>, project_id: Option<&str>) -> Vec<AgentRun> {
        self.runs
            .read()
            .iter()
            .filter(|r| {
                if let Some(a) = agent_id {
                    if r.agent_id != a {
                        return false;
                    }
                }
                if let Some(p) = project_id {
                    if r.project_id.as_deref() != Some(p) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect()
    }

    /// Aggregate statistics over all runs or filtered.
    pub fn stats(&self, agent_id: Option<&str>, project_id: Option<&str>) -> RunStats {
        let runs = self.filter(agent_id, project_id);
        if runs.is_empty() {
            return RunStats::default();
        }

        let total = runs.len();
        let success = runs
            .iter()
            .filter(|r| r.result == RunResult::Success)
            .count();
        let failure = runs
            .iter()
            .filter(|r| r.result == RunResult::Failure)
            .count();
        let stopped = runs
            .iter()
            .filter(|r| r.result == RunResult::Stopped)
            .count();
        let error = runs.iter().filter(|r| r.result == RunResult::Error).count();

        let total_duration: f64 = runs.iter().map(|r| r.duration_secs).sum();
        let avg_duration = if total > 0 {
            total_duration / total as f64
        } else {
            0.0
        };

        let total_errors: u32 = runs.iter().map(|r| r.error_count).sum();
        let error_rate = if total > 0 {
            total_errors as f64 / total as f64
        } else {
            0.0
        };

        let total_tokens = runs.iter().filter_map(|r| r.tokens.as_ref()).fold(
            TokenUsage {
                input: 0,
                output: 0,
                total: 0,
            },
            |acc, t| TokenUsage {
                input: acc.input + t.input,
                output: acc.output + t.output,
                total: acc.total + t.total,
            },
        );

        let runs_with_tokens = runs
            .iter()
            .filter(|r| r.tokens.is_some())
            .count();

        RunStats {
            total,
            success,
            failure,
            stopped,
            error,
            avg_duration,
            error_rate,
            total_tokens: if runs_with_tokens > 0 {
                Some(total_tokens)
            } else {
                None
            },
            runs_with_loop: runs.iter().filter(|r| r.loop_rounds.is_some()).count(),
        }
    }

    /// Clear all runs.
    pub fn clear(&self) {
        let mut runs = self.runs.write();
        runs.clear();
        drop(runs);
        self.save();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunStats {
    pub total: usize,
    pub success: usize,
    pub failure: usize,
    pub stopped: usize,
    pub error: usize,
    pub avg_duration: f64,
    pub error_rate: f64,
    pub total_tokens: Option<TokenUsage>,
    pub runs_with_loop: usize,
}

/// Track errors during a run: incremented when the pane encounters errors.
#[derive(Default)]
pub struct RunTracker {
    errors: HashMap<String, u32>,
}

impl RunTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an error for this pane.
    pub fn record_error(&mut self, pane_id: &str) {
        *self.errors.entry(pane_id.to_string()).or_insert(0) += 1;
    }

    /// Get error count for a pane and remove it from tracking.
    pub fn take_errors(&mut self, pane_id: &str) -> u32 {
        self.errors.remove(pane_id).unwrap_or(0)
    }
}
