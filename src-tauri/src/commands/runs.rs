//! Commands for the agent run dashboard.

use crate::commands::AppState;
use crate::runs::{AgentRun, RunStats};
use crate::error::Result;

/// List all agent runs, optionally filtered by agent or project.
#[tauri::command]
pub fn list_runs(
    state: tauri::State<'_, AppState>,
    agent_id: Option<String>,
    project_id: Option<String>,
) -> Result<Vec<AgentRun>> {
    Ok(state.runs.filter(
        agent_id.as_deref(),
        project_id.as_deref(),
    ))
}

/// Get aggregate statistics over runs, optionally filtered.
#[tauri::command]
pub fn run_stats(
    state: tauri::State<'_, AppState>,
    agent_id: Option<String>,
    project_id: Option<String>,
) -> Result<RunStats> {
    Ok(state.runs.stats(
        agent_id.as_deref(),
        project_id.as_deref(),
    ))
}

/// Clear all run history.
#[tauri::command]
pub fn clear_runs(state: tauri::State<'_, AppState>) -> Result<()> {
    state.runs.clear();
    Ok(())
}
