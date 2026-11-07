//! `launch`: folder validation and Blender resolution.

use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, State};

use super::config_cmds::commit_config;
use super::state::{lock, publish_status_with, AppState};
use crate::blender::{self, LaunchSpec};
use crate::config::Folder;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaunchError {
    /// "path_not_found" | "not_addon" | "blender_not_found" | "spawn_failed"
    pub kind: &'static str,
    pub message: String,
}

fn launch_err(kind: &'static str, message: String) -> LaunchError {
    LaunchError { kind, message }
}

/// The first two `_launch` checks (blinker_ui.py:1337-1346), message texts verbatim.
pub fn validate_folder(path: &str) -> Result<(), LaunchError> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return Err(launch_err("path_not_found", format!("Path not found:\n{path}")));
    }
    if !dir.join("blender_manifest.toml").exists() && !dir.join("__init__.py").exists() {
        return Err(launch_err(
            "not_addon",
            format!("Not a Blender addon (no __init__.py or blender_manifest.toml):\n{path}"),
        ));
    }
    Ok(())
}

/// `_resolve_blender` extended with the explicit (user-picked) path: explicit file,
/// then the configured file, then auto-discovery.
pub fn resolve_blender(
    explicit: Option<&str>,
    configured: &str,
    discover: impl FnOnce() -> Option<String>,
) -> Option<String> {
    for cand in [explicit.unwrap_or(""), configured] {
        if !cand.is_empty() && Path::new(cand).is_file() {
            return Some(cand.to_string());
        }
    }
    discover()
}

/// Python's "bootstrap.py not found at:\n{path}", plus why the embedded copy could not
/// be written there.
fn bootstrap_error(path: &std::path::Path, e: &std::io::Error) -> String {
    format!("bootstrap.py not found at:\n{}\n\n{e}", path.display())
}

/// Returns `Some(path)` when this call newly persisted a Blender executable to the
/// config (the user-picked `blender` argument). The UI MUST store it in its local
/// config copy, otherwise its next `save_config` overwrites it with the stale value.
#[tauri::command]
pub async fn launch(
    app: AppHandle,
    state: State<'_, AppState>,
    folder: Folder,
    blender: Option<String>,
) -> Result<Option<String>, LaunchError> {
    validate_folder(&folder.path)?;
    let bootstrap = blender::write_bootstrap()
        .map_err(|e| launch_err("spawn_failed", bootstrap_error(&blender::bootstrap_path(), &e)))?;
    let configured = lock(&state.config).blender_path.clone();
    let exe = resolve_blender(blender.as_deref(), &configured, || {
        blender::find_blender(std::env::var("BLENDER_PATH").ok().as_deref())
    })
    .ok_or_else(|| {
        launch_err(
            "blender_not_found",
            "Could not locate Blender automatically.\n\nPick the Blender executable now?".into(),
        )
    })?;
    let port = folder.port;
    let spec = LaunchSpec {
        blender: exe.clone(),
        bootstrap,
        blend: Some(folder.blend.clone()).filter(|b| !b.is_empty()),
        folder,
        args_override: None,
    };
    let sup = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || sup.launch(spec))
        .await
        .map_err(|e| launch_err("spawn_failed", e.to_string()))?
        .map_err(|m| launch_err("spawn_failed", m))?;
    // Kill/Reload/Restart flip on now, as Python sets running right after start().
    publish_status_with(&app, |ps| ps.launched(port));

    // `_prompt_blender_path` persists a picked executable once it is accepted.
    if blender.as_deref() == Some(exe.as_str()) {
        let mut slot = lock(&state.config);
        if slot.blender_path != exe {
            let mut new = slot.clone();
            new.blender_path = exe.clone();
            if let Err(e) = commit_config(&state.config_path, &mut slot, new.clone()) {
                // Keep it for this session and make the failure visible in the console.
                *slot = new;
                drop(slot);
                state.supervisor.append(
                    port,
                    &format!("[config] could not save blender path: {e}\n"),
                );
            }
            return Ok(Some(exe));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bootstrap_error_shows_path_and_cause() {
        let e = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Access is denied.");
        let m = bootstrap_error(std::path::Path::new(r"C:\T\blinker_bootstrap.py"), &e);
        assert_eq!(m, "bootstrap.py not found at:\nC:\\T\\blinker_bootstrap.py\n\nAccess is denied.");
    }

    #[test]
    fn validate_path_not_found() {
        let e = validate_folder("Z:/definitely/not/here").unwrap_err();
        assert_eq!(e.kind, "path_not_found");
        assert_eq!(e.message, "Path not found:\nZ:/definitely/not/here");
    }

    #[test]
    fn validate_not_an_addon() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().to_string_lossy().into_owned();
        let e = validate_folder(&p).unwrap_err();
        assert_eq!(e.kind, "not_addon");
        assert_eq!(
            e.message,
            format!("Not a Blender addon (no __init__.py or blender_manifest.toml):\n{p}")
        );
    }

    #[test]
    fn validate_checks_path_before_addon_and_accepts_either_marker() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("__init__.py"), "").unwrap();
        assert!(validate_folder(&t.path().to_string_lossy()).is_ok());
        let u = tempfile::tempdir().unwrap();
        fs::write(u.path().join("blender_manifest.toml"), "").unwrap();
        assert!(validate_folder(&u.path().to_string_lossy()).is_ok());
        // a file is not a directory
        let f = t.path().join("__init__.py");
        assert_eq!(validate_folder(&f.to_string_lossy()).unwrap_err().kind, "path_not_found");
    }

    fn touch(dir: &Path, name: &str) -> String {
        let p = dir.join(name);
        fs::write(&p, "x").unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn resolve_blender_order_explicit_then_config_then_discovery() {
        let t = tempfile::tempdir().unwrap();
        let a = touch(t.path(), "a.exe");
        let b = touch(t.path(), "b.exe");
        let find = || Some("found".to_string());
        assert_eq!(resolve_blender(Some(&a), &b, find), Some(a.clone()));
        assert_eq!(resolve_blender(None, &b, find), Some(b.clone()));
        assert_eq!(resolve_blender(Some("missing.exe"), &b, find), Some(b.clone()));
        assert_eq!(resolve_blender(None, "missing.exe", find), Some("found".into()));
        assert_eq!(resolve_blender(None, "", || None), None);
    }

    #[test]
    fn resolve_blender_does_not_discover_when_config_valid() {
        let t = tempfile::tempdir().unwrap();
        let b = touch(t.path(), "b.exe");
        assert_eq!(resolve_blender(None, &b, || panic!("must not discover")), Some(b));
    }
}
