//! Update commands (updater.rs; `_check_for_updates`).

use tauri::{AppHandle, Manager};

use super::state::AppState;
use crate::updater;

// ----- Check for updates (updater.rs; `_check_for_updates`) -----

/// The check up to the "Update available" question: which message box to show.
#[tauri::command]
pub async fn check_for_updates() -> Result<updater::UpdateCheck, String> {
    tauri::async_runtime::spawn_blocking(updater::check).await.map_err(|e| e.to_string())
}

/// After Yes: download the zip into `%TEMP%`, verify its SHA-256, start the updater
/// script and quit (`QApplication.quit()`; running Blenders are closed as on window
/// close). It installs the offer the backend kept from `check_for_updates`; the webview
/// passes no URLs or names.
#[tauri::command]
pub async fn apply_update(app: AppHandle) -> Result<updater::ApplyResult, String> {
    let r = tauri::async_runtime::spawn_blocking(updater::apply).await.map_err(|e| e.to_string())?;
    quit_if_started(&app, &r);
    Ok(r)
}

/// "Could not verify SHA256 ... Proceed anyway?" -> Yes.
#[tauri::command]
pub async fn continue_update(app: AppHandle, zip_path: String) -> Result<updater::ApplyResult, String> {
    let r = tauri::async_runtime::spawn_blocking(move || updater::continue_pending(&zip_path))
        .await
        .map_err(|e| e.to_string())?;
    quit_if_started(&app, &r);
    Ok(r)
}

/// "Proceed anyway?" -> No: delete the downloaded zip.
#[tauri::command]
pub async fn cancel_update(zip_path: String) {
    updater::cancel_pending(&zip_path);
}

fn quit_if_started(app: &AppHandle, r: &updater::ApplyResult) {
    if matches!(r, updater::ApplyResult::Started) {
        // `app.exit` emits no CloseRequested: remember the window first.
        crate::window_state::save_on_exit(app);
        app.state::<AppState>().supervisor.shutdown();
        app.exit(0);
    }
}
