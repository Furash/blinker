//! Terminal/folder openers, Blender discovery, alias icons and fonts.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter, Manager, State};

use super::state::{lock, AppState};
use crate::{blender, terminal};

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `find_alias_icon` (blinker_ui.py:54-71): first `<alias>.svg` / `.png` found in
/// `dirs` (svg before png within a dir), returned as a `data:` URI.
pub fn find_alias_icon(dirs: &[PathBuf], alias: &str) -> Option<String> {
    let name = alias.trim().to_lowercase();
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    for d in dirs {
        for (ext, mime) in [("svg", "image/svg+xml"), ("png", "image/png")] {
            let p = d.join(format!("{name}.{ext}"));
            if p.is_file() {
                if let Ok(bytes) = std::fs::read(&p) {
                    return Some(format!("data:{mime};base64,{}", base64(&bytes)));
                }
            }
        }
    }
    None
}

fn icon_dirs(app: &AppHandle) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // %APPDATA%\BlinkerUI\icons (or BLINKER_CONFIG_DIR\icons): next to the config.
    if let Some(cfg_dir) = crate::paths::config_path().parent() {
        dirs.push(cfg_dir.join("icons"));
    }
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir.join("icons"));
    }
    if let Ok(res) = app.path().resource_dir() {
        dirs.push(res.join("icons"));
    }
    dirs
}

#[tauri::command]
pub async fn open_ai(app: AppHandle, state: State<'_, AppState>, path: String, ai: String) -> Result<(), String> {
    let template = lock(&state.config).terminal.clone();
    if template.trim().is_empty() {
        return Err("Terminal alias not set (Settings → Terminal).".into());
    }
    std::thread::spawn(move || {
        if let Err(e) = terminal::open_ai(&template, &path, &ai) {
            let _ = app.emit(
                "blinker://terminal-error",
                serde_json::json!({ "ai": e.ai.clone(), "message": e.to_string() }),
            );
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn open_folder(path: String) -> Result<(), String> {
    terminal::open_folder(&path)
}

#[tauri::command]
pub async fn find_blender() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        blender::find_blender(std::env::var("BLENDER_PATH").ok().as_deref())
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn alias_icon(app: AppHandle, alias: String) -> Result<Option<String>, String> {
    let dirs = icon_dirs(&app);
    tauri::async_runtime::spawn_blocking(move || find_alias_icon(&dirs, &alias))
        .await
        .map_err(|e| e.to_string())
}

/// `QFontDatabase.families()` for the font combo boxes.
#[tauri::command]
pub async fn list_fonts() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(crate::fonts::families).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn alias_icon_prefers_svg_and_sets_mime() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("claude.png"), b"png").unwrap();
        fs::write(t.path().join("claude.svg"), b"<svg/>").unwrap();
        let dirs = [t.path().to_path_buf()];
        assert_eq!(
            find_alias_icon(&dirs, "claude").unwrap(),
            format!("data:image/svg+xml;base64,{}", base64(b"<svg/>"))
        );
        fs::remove_file(t.path().join("claude.svg")).unwrap();
        assert_eq!(
            find_alias_icon(&dirs, "claude").unwrap(),
            format!("data:image/png;base64,{}", base64(b"png"))
        );
    }

    #[test]
    fn alias_icon_earlier_dir_wins_even_with_png() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::write(a.path().join("x.png"), b"A").unwrap();
        fs::write(b.path().join("x.svg"), b"B").unwrap();
        let dirs = [a.path().to_path_buf(), b.path().to_path_buf()];
        assert!(find_alias_icon(&dirs, "x").unwrap().starts_with("data:image/png;base64,"));
    }

    #[test]
    fn alias_icon_unknown_empty_and_missing_dirs_give_none() {
        let t = tempfile::tempdir().unwrap();
        let dirs = [t.path().join("no-such-dir"), t.path().to_path_buf()];
        assert_eq!(find_alias_icon(&dirs, "nope"), None);
        assert_eq!(find_alias_icon(&dirs, "   "), None);
        assert_eq!(find_alias_icon(&dirs, "../claude"), None);
    }

    #[test]
    fn alias_icon_trims_and_lowercases() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("codex.svg"), b"<svg/>").unwrap();
        let dirs = [t.path().to_path_buf()];
        assert!(find_alias_icon(&dirs, "  CoDeX ").is_some());
    }
}
