//! Tauri command layer: JSON in/out, shared state, events.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::addons::{self, AddonKind};
use crate::blender::{self, LaunchSpec, OutputSink, Supervisor};
use crate::config::{self, Config, Folder};
use crate::{net, terminal, theme};

/// Shared state managed by Tauri.
pub struct AppState {
    pub config_path: PathBuf,
    pub config: Mutex<Config>,
    pub supervisor: Arc<Supervisor>,
    pub ports: Arc<Mutex<Vec<u16>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Forwards Blender output and exit notifications to the webview.
pub struct TauriSink {
    pub app: AppHandle,
}

impl OutputSink for TauriSink {
    fn push(&self, port: u16, text: &str) {
        let _ = self.app.emit("blinker://output", serde_json::json!({ "port": port, "text": text }));
    }
    fn exited(&self, port: u16, code: Option<i32>) {
        let _ = self.app.emit("blinker://exited", serde_json::json!({ "port": port, "code": code }));
    }
}

// ---------------------------------------------------------------------------
// get_state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AppStateDto {
    pub config: Config,
    pub config_path: String,
    pub version: String,
    pub running: BTreeMap<String, bool>,
    pub default_theme: Map<String, Value>,
    pub presets: Vec<String>,
    pub color_keys: Vec<[String; 2]>,
}

/// Pure core of `get_state` (MainWindow.__init__ 1042-1065): favourites first,
/// theme merged over the default preset. Defaults for terminal/ai_aliases are
/// already applied by `config::load`.
pub fn build_state(cfg: &Config, config_path: &Path, running: &BTreeMap<u16, bool>) -> AppStateDto {
    let mut config = cfg.clone();
    config.folders = config::sorted_folders(config.folders);
    config.theme = theme::merge_with_default(&config.theme);
    AppStateDto {
        config,
        config_path: config_path.to_string_lossy().into_owned(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        running: running.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        default_theme: theme::preset(theme::PRESET_NAMES[0]).unwrap_or_default(),
        presets: theme::PRESET_NAMES.iter().map(|s| s.to_string()).collect(),
        color_keys: theme::COLOR_KEYS.iter().map(|(k, l)| [k.to_string(), l.to_string()]).collect(),
    }
}

// ---------------------------------------------------------------------------
// launch
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// alias icons
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// Returns at once: `running` is empty. Probing here blocked startup for ~1 s (a
/// connect to a closed localhost port waits for the full probe timeout on Windows);
/// the UI calls `probe_now` once its listeners are in place, as Python polls right
/// after the window is built (`QTimer.singleShot(0, self._poll_status)`).
#[tauri::command]
pub async fn get_state(state: State<'_, AppState>) -> Result<AppStateDto, String> {
    let cfg = lock(&state.config).clone();
    Ok(build_state(&cfg, &state.config_path, &BTreeMap::new()))
}

/// Probe the given ports now and emit the result as `blinker://status`.
#[tauri::command]
pub async fn probe_now(app: AppHandle, ports: Vec<u16>) -> Result<(), String> {
    let running = tauri::async_runtime::spawn_blocking(move || net::probe(&ports))
        .await
        .map_err(|e| e.to_string())?;
    let payload: BTreeMap<String, bool> = running.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    app.emit("blinker://status", payload).map_err(|e| e.to_string())
}

/// Entries the UI never sees (unparsable folders, wrongly typed values) are
/// `serde(skip)` fields: carry them over from the managed config.
pub fn merge_for_save(incoming: Config, current: &Config) -> Config {
    let mut cfg = incoming;
    cfg.unparsed_folders = current.unparsed_folders.clone();
    cfg.invalid = current.invalid.clone();
    cfg
}

/// The single write path for the shared config. The caller holds the config mutex
/// (`slot` is the guarded value), so no two writers can interleave on the `.tmp` file.
fn commit_config(path: &Path, slot: &mut Config, new: Config) -> std::io::Result<()> {
    let r = config::save(path, &new);
    if r.is_ok() {
        *slot = new;
    }
    r
}

#[tauri::command]
pub async fn save_config(state: State<'_, AppState>, config: Config) -> Result<(), String> {
    let mut slot = lock(&state.config);
    let cfg = merge_for_save(config, &slot);
    commit_config(&state.config_path, &mut slot, cfg).map_err(|e| format!("Could not save config: {e}"))
}

#[derive(Debug, Serialize)]
pub struct AddonEntry {
    pub path: String,
    pub kind: AddonKind,
}

#[tauri::command]
pub async fn scan_addons(root: String) -> Result<Vec<AddonEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        addons::find_addons(Path::new(&root), 3)
            .into_iter()
            .map(|(p, kind)| AddonEntry { path: p.to_string_lossy().into_owned(), kind })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn addon_kind_of(path: String) -> Result<Option<AddonKind>, String> {
    Ok(addons::addon_kind(Path::new(&path)))
}

/// Returns `Some(path)` when this call newly persisted a Blender executable to the
/// config (the user-picked `blender` argument). The UI MUST store it in its local
/// config copy, otherwise its next `save_config` overwrites it with the stale value.
#[tauri::command]
pub async fn launch(
    state: State<'_, AppState>,
    folder: Folder,
    blender: Option<String>,
) -> Result<Option<String>, LaunchError> {
    validate_folder(&folder.path)?;
    let bootstrap = blender::write_bootstrap()
        .map_err(|e| launch_err("spawn_failed", format!("bootstrap.py not found at:\n{e}")))?;
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
                    &format!("[config] could not save blender path: {e}
"),
                );
            }
            return Ok(Some(exe));
        }
    }
    Ok(None)
}

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
pub async fn clear_output(state: State<'_, AppState>, port: u16) -> Result<(), String> {
    state.supervisor.clear(port);
    Ok(())
}

#[tauri::command]
pub async fn get_output(state: State<'_, AppState>, port: u16) -> Result<String, String> {
    Ok(state.supervisor.output(port))
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
pub async fn set_ports(state: State<'_, AppState>, ports: Vec<u16>) -> Result<(), String> {
    *lock(&state.ports) = ports;
    Ok(())
}

#[tauri::command]
pub async fn alias_icon(app: AppHandle, alias: String) -> Result<Option<String>, String> {
    let dirs = icon_dirs(&app);
    tauri::async_runtime::spawn_blocking(move || find_alias_icon(&dirs, &alias))
        .await
        .map_err(|e| e.to_string())
}

/// Debug build? The UI keeps reload/devtools keys only in debug builds.
#[tauri::command]
pub fn is_dev() -> bool {
    cfg!(debug_assertions)
}

/// The UI reports `performance.now()` at window show (`window.__blinker_ready_ms`).
/// When `BLINKER_READY_LOG` names a file, one line `ready_ms=<ms>` is appended to it
/// (startup measurement, tools/measure_startup.ps1); otherwise this does nothing.
#[tauri::command]
pub fn report_ready(ms: f64) {
    if let Some(p) = std::env::var_os("BLINKER_READY_LOG").filter(|p| !p.is_empty()) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "ready_ms={ms:.1}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Content of the repo's real sample `blinker_ui_config.json`.
    const LEGACY_SAMPLE: &str = r#"{
  "folders": [
    { "path": "D:/git/Blender_CCP_Tools", "port": 9876, "repo": "blinker", "module": "", "blend": "" }
  ],
  "launchers": [
    { "name": "CLAUDE", "command": "wt -d {path} -NoExit -Command claude" }
  ]
}"#;

    #[test]
    fn get_state_with_legacy_config_returns_folder_and_defaults() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("blinker_ui_config.json");
        fs::write(&p, LEGACY_SAMPLE).unwrap();
        let cfg = config::load(&p);
        let s = build_state(&cfg, &p, &BTreeMap::from([(9876, true)]));
        assert_eq!(s.config.folders.len(), 1);
        assert_eq!(s.config.folders[0].path, "D:/git/Blender_CCP_Tools");
        assert_eq!(s.config.folders[0].port, 9876);
        assert_eq!(s.config.ai_aliases, "claude, codex");
        assert_eq!(s.config.terminal, config::default_terminal());
        assert_eq!(s.config.blender_path, "");
        assert_eq!(s.config.theme, theme::merge_with_default(&Map::new()));
        assert_eq!(s.default_theme, theme::preset("Tokyo Night (default)").unwrap());
        assert_eq!(s.presets, vec!["Tokyo Night (default)", "Frigus Nox", "Light"]);
        assert_eq!(s.color_keys.len(), 18);
        assert_eq!(s.running.get("9876"), Some(&true));
        assert_eq!(s.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(s.config_path, p.to_string_lossy());
        // unknown keys survive the round trip to the UI
        let j = serde_json::to_value(&s).unwrap();
        assert!(j["config"]["launchers"].is_array());
    }

    #[test]
    fn build_state_sorts_favourites_first_and_merges_user_theme() {
        let mut cfg = Config::default();
        cfg.folders = vec![
            Folder { path: "a".into(), ..Folder::default() },
            Folder { path: "b".into(), favourite: true, ..Folder::default() },
        ];
        let key = theme::COLOR_KEYS[0].0;
        cfg.theme.insert(key.into(), Value::String("#000000".into()));
        let s = build_state(&cfg, Path::new("c.json"), &BTreeMap::new());
        assert_eq!(s.config.folders[0].path, "b");
        assert_eq!(s.config.theme.get(key), Some(&Value::String("#000000".into())));
        assert_eq!(s.config.theme.len(), s.default_theme.len());
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

    #[test]
    fn merge_for_save_preserves_unparsed_invalid_and_unknown_through_ui_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("in.json");
        fs::write(
            &src,
            r#"{"folders":[{"path":"a","port":9876},{"path":"bad","port":"x"}],
                "terminal":5,"mystery":{"k":1}}"#,
        )
        .unwrap();
        let loaded = config::load(&src);
        assert_eq!(loaded.unparsed_folders.len(), 1);
        // as get_state hands it to the UI and the UI returns it
        let dto = build_state(&loaded, &src, &BTreeMap::new());
        let json = serde_json::to_string(&dto.config).unwrap();
        let incoming: Config = serde_json::from_str(&json).unwrap();
        assert!(incoming.unparsed_folders.is_empty() && incoming.invalid.is_empty());
        let merged = merge_for_save(incoming, &loaded);
        let out = t.path().join("out.json");
        config::save(&out, &merged).unwrap();
        let back = config::load(&out);
        assert_eq!(back.unparsed_folders.len(), 1);
        assert_eq!(back.unparsed_folders[0]["path"], "bad");
        assert_eq!(back.invalid.get("terminal"), Some(&serde_json::json!(5)));
        assert_eq!(back.extra.get("mystery"), Some(&serde_json::json!({"k":1})));
        assert_eq!(back.folders.len(), 1);
    }

    #[test]
    fn merge_for_save_edited_folders_replace_but_unparsed_kept_once() {
        let mut current = Config::default();
        current.folders = vec![Folder { path: "old".into(), ..Folder::default() }];
        current.unparsed_folders = vec![serde_json::json!({"path": 1})];
        let mut incoming = Config::default();
        incoming.folders = vec![
            Folder { path: "n1".into(), ..Folder::default() },
            Folder { path: "n2".into(), ..Folder::default() },
        ];
        let m = merge_for_save(incoming, &current);
        assert_eq!(m.folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["n1", "n2"]);
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("c.json");
        config::save(&p, &m).unwrap();
        let back = config::load(&p);
        assert_eq!(back.folders.len(), 2);
        assert_eq!(back.unparsed_folders.len(), 1);
    }

    #[test]
    fn commit_config_replaces_slot_only_on_success() {
        let t = tempfile::tempdir().unwrap();
        let mut slot = Config::default();
        let mut new = Config::default();
        new.blender_path = "b".into();
        commit_config(&t.path().join("c.json"), &mut slot, new.clone()).unwrap();
        assert_eq!(slot.blender_path, "b");
        // a directory in place of the file makes the rename fail
        let bad = t.path().join("dir");
        fs::create_dir(&bad).unwrap();
        let mut other = Config::default();
        other.blender_path = "z".into();
        assert!(commit_config(&bad, &mut slot, other).is_err());
        assert_eq!(slot.blender_path, "b");
    }
}
