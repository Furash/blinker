mod addons;
mod blender;
mod commands;
mod config;
mod net;
mod paths;
mod terminal;
mod theme;

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

pub fn run() {
    #[cfg(debug_assertions)]
    if selftest() {
        return;
    }

    let config_path = paths::config_path();
    if let Some(legacy) = paths::legacy_config_path() {
        config::migrate_legacy_config(&config_path, &legacy);
    }

    tauri::Builder::default()
        .setup(move |app| {
            let cfg = config::load(&config_path);
            let ports = Arc::new(Mutex::new(cfg.folders.iter().map(|f| f.port).collect::<Vec<_>>()));
            let sink = Arc::new(TauriSink { app: app.handle().clone() });
            app.manage(AppState {
                config_path,
                config: Mutex::new(cfg),
                supervisor: Arc::new(blender::Supervisor::new(sink)),
                ports: ports.clone(),
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running BlinkerUI");
}
