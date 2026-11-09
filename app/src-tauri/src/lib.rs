mod addons;
mod blender;
mod commands;
mod config;
mod fonts;
mod import;
mod net;
mod paths;
mod pyerr;
mod startup;
mod terminal;
mod updater;
mod theme;
mod window_state;
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

/// `BlinkerUI.exe --import-selftest <file> [--with-running] [--change-between]` (debug builds only): Import
/// Config without a window against the config in `BLINKER_CONFIG_DIR` (never the real one
/// unless that variable is unset: it refuses to run then). Prints `inspect:` and `apply:`
/// as JSON lines. With `--with-running` a stand-in "Blender" (a busy `cmd.exe`) is
/// supervised first, so the first apply must be refused; it is killed and the apply
/// repeated. With `--change-between` the file is modified after the inspect, so the first
/// apply must be refused as changed.
#[cfg(debug_assertions)]
fn import_selftest() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let Some(i) = args.iter().position(|a| a == "--import-selftest") else { return false };
    if !paths::config_dir_overridden() {
        println!("refused: set BLINKER_CONFIG_DIR to a scratch directory first");
        return true;
    }
    let Some(src) = args.get(i + 1).map(std::path::PathBuf::from) else {
        println!("usage: --import-selftest <file> [--with-running]");
        return true;
    };
    struct Quiet;
    impl blender::OutputSink for Quiet {
        fn push(&self, _: u16, _: &str, _: u64) {}
        fn exited(&self, _: u16, _: Option<i32>) {}
    }
    let path = paths::config_path();
    let inspected = import::read_config_file(&src).map(|p| p.summary);
    println!("inspect: {}", json(&inspected));
    let mut fp = inspected.map(|s| s.fingerprint).unwrap_or_default();
    if args.iter().any(|a| a == "--change-between") {
        // the file changes after the user confirmed it: the apply must be refused
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&src) {
            use std::io::Write;
            let _ = f.write_all(b"\n");
        }
        let mut probe = config::load(&path);
        println!("apply(changed): {}", json(&commands::apply_import(&path, &mut probe, &src, &fp, &[])));
        fp = import::read_config_file(&src).map(|p| p.summary.fingerprint).unwrap_or_default();
    }
    let mut slot = config::load(&path);
    let sup = blender::Supervisor::new(Arc::new(Quiet));
    if args.iter().any(|a| a == "--with-running") {
        let dir = std::env::temp_dir().join(format!("blinker_import_selftest_{}", std::process::id()));
        let addon = dir.join("addon");
        let _ = std::fs::create_dir_all(&addon);
        let _ = std::fs::write(addon.join("__init__.py"), b"");
        let boot = dir.join("bootstrap.py");
        let _ = std::fs::write(&boot, b"#");
        let spec = blender::LaunchSpec {
            blender: "cmd.exe".into(),
            bootstrap: boot,
            folder: config::Folder { path: addon.to_string_lossy().into_owned(), port: 9931, ..Default::default() },
            blend: None,
            args_override: Some(vec!["/d".into(), "/c".into(), "for /L %i in (1,1,2000000000) do @rem".into()]),
        };
        println!("launch: {}", json(&sup.launch(spec)));
        println!("alive: {:?}", sup.alive_ports());
        println!("apply(running): {}", json(&commands::apply_import(&path, &mut slot, &src, &fp, &sup.alive_ports())));
        println!("kill: {}", sup.kill(9931));
        for _ in 0..500 {
            if sup.alive_ports().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    println!("apply: {}", json(&commands::apply_import(&path, &mut slot, &src, &fp, &sup.alive_ports())));
    true
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

/// Startup timeline (wall-clock ms since the Unix epoch, comparable with the webview's
/// `performance.timeOrigin`). `report_ready` writes it to `BLINKER_READY_LOG`, relative
/// to the process creation time, so a measurement can split the startup into its steps.
pub(crate) mod timeline {
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    static MARKS: Mutex<Vec<(&'static str, f64)>> = Mutex::new(Vec::new());

    pub fn now_ms() -> f64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
    }

    pub fn mark(name: &'static str) {
        if let Ok(mut m) = MARKS.lock() {
            m.push((name, now_ms()));
        }
    }

    pub fn marks() -> Vec<(&'static str, f64)> {
        MARKS.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// When this process was created (epoch ms).
    #[cfg(windows)]
    pub fn process_start_ms() -> Option<f64> {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut c, mut e, mut k, mut u) = (z, z, z, z);
        // SAFETY: the pseudo handle is always valid; the out-pointers are valid FILETIMEs.
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        if ok == 0 {
            return None;
        }
        let t100ns = (u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime);
        // 100 ns ticks since 1601-01-01 -> ms since 1970-01-01
        Some(t100ns as f64 / 10_000.0 - 11_644_473_600_000.0)
    }

    #[cfg(not(windows))]
    pub fn process_start_ms() -> Option<f64> {
        None
    }
}

/// Set once the frontend has shown the main window (`report_ready`: after the first
/// paint, or with the "Startup failed" message).
pub(crate) static WINDOW_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the first instance does on a second launch.
#[derive(Debug, PartialEq, Eq)]
struct SecondLaunch {
    show: bool,
    unminimize: bool,
    raise: bool,
}

/// The frontend reported ready, or the window is visible anyway.
fn window_ready(reported: bool, visible: bool) -> bool {
    reported || visible
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
    // A window that is already visible was shown by the frontend (first paint or the
    // "Startup failed" message), even if `report_ready` never arrived.
    let ready = window_ready(WINDOW_READY.load(std::sync::atomic::Ordering::SeqCst), w.is_visible().unwrap_or(false));
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
    if selftest() || update_selftest() || import_selftest() {
        return;
    }

    timeline::mark("run");
    // No WebView2 runtime: the webview cannot be created and, as a GUI app built with
    // panic=abort, the app would just vanish. Say so with the download link instead.
    if !startup::webview2_installed(startup::read_pv_registry) {
        startup::error_box(&startup::missing_webview2_text(), true);
        std::process::exit(1);
    }
    timeline::mark("webview2_check");
    set_app_user_model_id();

    // Single instance first: the plugin's setup runs before the window is created, so a
    // second launch exits right there, before any config work, migration or poller.
    // (Its own identifier: not the Python app's mutex/wake file, so both can coexist.)
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                bring_to_front(&w);
            }
        }))
        // Native pickers: `open` (folders/files) and `save` (theme export); capability
        // `dialog:allow-open` + `dialog:allow-save` only.
        .plugin(tauri_plugin_dialog::init())
        .on_page_load(|_, payload| {
            timeline::mark(match payload.event() {
                tauri::webview::PageLoadEvent::Started => "page_started",
                tauri::webview::PageLoadEvent::Finished => "page_finished",
            });
        })
        .setup(move |app| {
            // The windows from tauri.conf.json (webview included) exist by now.
            timeline::mark("setup");
            let config_path = paths::config_path();
            // BLINKER_CONFIG_DIR runs (tests, side-by-side) never migrate the exe-dir config.
            if !paths::config_dir_overridden() {
                if let Some(legacy) = paths::legacy_config_path() {
                    config::migrate_legacy_config(&config_path, &legacy);
                }
            }
            // Window position/size from last time, applied while the window is still
            // hidden (the frontend shows it after first paint): no flash at the default place.
            if let Some(w) = app.get_webview_window(window_state::MAIN) {
                let memory = window_state::restore(&w, window_state::state_path(&config_path));
                memory.start_worker(window_state::DEBOUNCE);
                app.manage(memory);
            }
            timeline::mark("window_state");
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
            // If the frontend never shows the window (failed to load, hung before
            // showWindow), show it anyway after a while.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(startup::FALLBACK_SHOW_AFTER);
                if let Some(w) = handle.get_webview_window("main") {
                    let ready = WINDOW_READY.load(std::sync::atomic::Ordering::SeqCst);
                    if startup::fallback_should_show(ready, w.is_visible().unwrap_or(false)) {
                        let _ = w.show();
                    }
                }
            });
            timeline::mark("setup_done");
            Ok(())
        })
        .on_window_event(|window, event| {
            // First, and on its own: the close-time save is synchronous and must not wait
            // for (or be lost to) a slow Blender shutdown; it never panics or blocks the close.
            window_state::on_window_event(window, event);
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
            commands::inspect_config_file,
            commands::apply_config_file,
            commands::list_fonts,
            commands::check_for_updates,
            commands::apply_update,
            commands::continue_update,
            commands::cancel_update,
        ])
        .run(tauri::generate_context!());
    if let Err(e) = result {
        startup::error_box(&startup::startup_error_text(&e.to_string()), false);
        std::process::exit(1);
    }
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
    fn visible_window_counts_as_ready_without_report() {
        assert!(!window_ready(false, false));
        assert!(window_ready(false, true));
        assert!(window_ready(true, false));
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
