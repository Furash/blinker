//! Commands on a running Blender: reload/restart/kill/terminate and its console output.

use std::time::Duration;

use serde::Serialize;
use tauri::State;

use super::state::AppState;
use crate::net;

async fn tcp_line(state: &State<'_, AppState>, port: u16, cmd: &'static str) -> Result<String, String> {
    let resp = tauri::async_runtime::spawn_blocking(move || net::tcp_send(port, cmd, Duration::from_secs(3)))
        .await
        .map_err(|e| e.to_string())?;
    let line = format!("[{cmd}] {resp}\n");
    state.supervisor.append(port, &line);
    Ok(line)
}

#[tauri::command]
pub async fn reload(state: State<'_, AppState>, port: u16) -> Result<String, String> {
    tcp_line(&state, port, "reload").await
}

#[tauri::command]
pub async fn restart(state: State<'_, AppState>, port: u16) -> Result<String, String> {
    tcp_line(&state, port, "restart").await
}

#[tauri::command]
pub async fn kill(state: State<'_, AppState>, port: u16) -> Result<String, String> {
    if state.supervisor.kill(port) {
        // Supervisor::kill already logged and pushed "[kill] process killed".
        return Ok("[kill] process killed\n".to_string());
    }
    tcp_line(&state, port, "kill").await
}

/// Graceful stop used when a folder is removed (blinker_ui.py:1316-1334).
#[tauri::command]
pub async fn terminate_process(state: State<'_, AppState>, port: u16) -> Result<bool, String> {
    let sup = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || sup.terminate(port))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
/// Returns the console sequence at the clear; output events at or below it are older.
pub async fn clear_output(state: State<'_, AppState>, port: u16) -> Result<u64, String> {
    Ok(state.supervisor.clear(port))
}

#[derive(Debug, Serialize)]
pub struct OutputSnapshot {
    pub text: String,
    /// Console sequence number of `text`; `blinker://output` events with a larger
    /// `seq` are not in it.
    pub seq: u64,
}

#[tauri::command]
pub async fn get_output(state: State<'_, AppState>, port: u16) -> Result<OutputSnapshot, String> {
    let (text, seq) = state.supervisor.output_with_seq(port);
    Ok(OutputSnapshot { text, seq })
}
