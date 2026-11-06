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

/// Same AUMID as the Python app (`_set_windows_aumid("blinker.ui")`) so existing pinned
/// taskbar shortcuts keep grouping. Must run before any window exists.
fn set_app_user_model_id() {
    #[cfg(windows)]
    {
        let id: Vec<u16> = "blinker.ui\0".encode_utf16().collect();
        // SAFETY: `id` is a valid NUL-terminated UTF-16 string for the call.
        let hr = unsafe { windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
        debug_assert!(hr >= 0, "SetCurrentProcessExplicitAppUserModelID failed: {hr:#x}");
        let _ = hr;
    }
}

/// Set once the frontend has shown the main window (first paint done, `report_ready`).
pub(crate) static WINDOW_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the first instance does on a second launch.
#[derive(Debug, PartialEq, Eq)]
struct SecondLaunch {
    show: bool,
    unminimize: bool,
    raise: bool,
}

/// Before the window has been shown for the first time (themed first frame still
/// pending) a second launch must not show it: the frontend does that after first paint.
fn second_launch_actions(ready: bool, minimized: bool) -> SecondLaunch {
    SecondLaunch { show: ready, unminimize: ready && minimized, raise: ready }
}

/// Second launch: show, restore if minimised, raise and focus (the Python wake-file
/// handler). Windows only lets the foreground owner take focus, so a plain `set_focus`
/// from a background process just flashes the taskbar; toggling always-on-top raises it.
fn bring_to_front(w: &tauri::WebviewWindow) {
    let ready = WINDOW_READY.load(std::sync::atomic::Ordering::SeqCst);
    let act = second_launch_actions(ready, w.is_minimized().unwrap_or(false));
    if act.show {
        let _ = w.show();
    }
    if act.unminimize {
        let _ = w.unminimize();
    }
    if act.raise {
        let _ = w.set_always_on_top(true);
        let _ = w.set_always_on_top(false);
        let _ = w.set_focus();
    }
}

pub fn run() {
    #[cfg(debug_assertions)]
    if selftest() || update_selftest() {
        return;
    }

    set_app_user_model_id();

    // Single instance first: the plugin's setup runs before the window is created, so a
    // second launch exits right there, before any config work, migration or poller.
    // (Its own identifier: not the Python app's mutex/wake file, so both can coexist.)
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                bring_to_front(&w);
            }
        }))
        // Native pickers: `open` (folders/files) and `save` (theme export); capability
        // `dialog:allow-open` + `dialog:allow-save` only.
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let config_path = paths::config_path();
            // BLINKER_CONFIG_DIR runs (tests, side-by-side) never migrate the exe-dir config.
            if !paths::config_dir_overridden() {
                if let Some(legacy) = paths::legacy_config_path() {
                    config::migrate_legacy_config(&config_path, &legacy);
                }
            }
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

#[cfg(test)]
mod second_launch_tests {
    use super::*;

    #[test]
    fn early_second_launch_does_nothing() {
        let none = SecondLaunch { show: false, unminimize: false, raise: false };
        assert_eq!(second_launch_actions(false, false), none);
        assert_eq!(second_launch_actions(false, true), none);
    }

    #[test]
    fn ready_second_launch_shows_restores_and_raises() {
        assert_eq!(second_launch_actions(true, true), SecondLaunch { show: true, unminimize: true, raise: true });
        assert_eq!(second_launch_actions(true, false), SecondLaunch { show: true, unminimize: false, raise: true });
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::set_app_user_model_id;

    #[test]
    fn explicit_aumid_is_blinker_ui() {
        use windows_sys::Win32::UI::Shell::GetCurrentProcessExplicitAppUserModelID;
        set_app_user_model_id();
        let mut p: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: out-pointer is valid; the returned string is read before being freed.
        let hr = unsafe { GetCurrentProcessExplicitAppUserModelID(&mut p) };
        assert_eq!(hr, 0);
        let mut len = 0;
        // SAFETY: `p` is a NUL-terminated string allocated by COM on success.
        let id = unsafe {
            while *p.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
        };
        assert_eq!(id, "blinker.ui");
    }
}
