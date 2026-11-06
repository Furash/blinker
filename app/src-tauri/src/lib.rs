mod addons;
mod blender;
mod commands;
mod config;
mod fonts;
mod net;
mod paths;
mod pyerr;
mod terminal;
mod updater;
mod theme;
mod winhttp;

use std::sync::{Arc, Mutex};

use tauri::{Manager, WindowEvent};

use commands::{AppState, TauriSink};

/// `BlinkerUI.exe --selftest` (debug builds only): print `get_state` for the real
/// config as JSON and exit. Verifies the config/state path without a window.
#[cfg(debug_assertions)]
fn selftest() -> bool {
    if !std::env::args().any(|a| a == "--selftest") {
        return false;
    }
    let path = paths::config_path();
    let cfg = config::load(&path);
    let state = commands::build_state(&cfg, &path, &std::collections::BTreeMap::new());
    println!("{}", serde_json::to_string_pretty(&state).unwrap_or_default());
    true
}

/// `BlinkerUI.exe --update-selftest [--check-only] [--proceed-unverified]` (debug builds
/// only): the "Check for updates" flow without a window, answering Yes to "Update
/// available" (unless `--check-only`) and, with `--proceed-unverified`, Yes to "Proceed
/// anyway?" (otherwise No). Prints one JSON line per step. When the updater starts,
/// this process exits right away, so the script's `-WaitPid` sees it gone. Point `BLINKER_UPDATE_API` at a test server.
#[cfg(debug_assertions)]
fn update_selftest() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--update-selftest") {
        return false;
    }
    let check = updater::check();
    println!("check: {}", json(&check));
    let updater::UpdateCheck::Available { .. } = check else {
        return true;
    };
    if args.iter().any(|a| a == "--check-only") {
        return true;
    }
    let mut r = updater::apply();
    println!("apply: {}", json(&r));
    if let updater::ApplyResult::ShaUnverifiable { zip_path, .. } = &r {
        if args.iter().any(|a| a == "--proceed-unverified") {
            r = updater::continue_pending(zip_path);
            println!("continue: {}", json(&r));
        } else {
            updater::cancel_pending(zip_path);
            println!("cancel: deleted {zip_path}");
        }
    }
    true
}

#[cfg(debug_assertions)]
fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

pub fn run() {
    #[cfg(debug_assertions)]
    if selftest() || update_selftest() {
        return;
    }

    let config_path = paths::config_path();
    // BLINKER_CONFIG_DIR runs (tests, side-by-side) never migrate the exe-dir config.
    if !paths::config_dir_overridden() {
        if let Some(legacy) = paths::legacy_config_path() {
            config::migrate_legacy_config(&config_path, &legacy);
        }
    }

    tauri::Builder::default()
        // Native pickers: `open` (folders/files) and `save` (theme export); capability
        // `dialog:allow-open` + `dialog:allow-save` only.
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let cfg = config::load(&config_path);
            let ports = Arc::new(Mutex::new(cfg.folders.iter().map(|f| f.port).collect::<Vec<_>>()));
            let sink = Arc::new(TauriSink { app: app.handle().clone() });
            app.manage(AppState {
                config_path,
                config: Mutex::new(cfg),
                supervisor: Arc::new(blender::Supervisor::new(sink)),
                ports: ports.clone(),
                last_probe: Mutex::new(Default::default()),
            });
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title(&format!("Blinker UI  v{}", env!("CARGO_PKG_VERSION")));
            }
            net::start_poller(app.handle().clone(), ports);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { .. } = event {
                window.app_handle().state::<AppState>().supervisor.shutdown();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::save_config,
            commands::scan_addons,
            commands::addon_kind_of,
            commands::launch,
            commands::reload,
            commands::restart,
            commands::kill,
            commands::terminate_process,
            commands::clear_output,
            commands::get_output,
            commands::open_ai,
            commands::open_folder,
            commands::find_blender,
            commands::set_ports,
            commands::alias_icon,
            commands::is_dev,
            commands::report_ready,
            commands::probe_now,
            commands::status_now,
            commands::normalize_hex,
            commands::detect_preset,
            commands::export_theme,
            commands::import_theme,
            commands::list_fonts,
            commands::check_for_updates,
            commands::apply_update,
            commands::continue_update,
            commands::cancel_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running BlinkerUI");
}
