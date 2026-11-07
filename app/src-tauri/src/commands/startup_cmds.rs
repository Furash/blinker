//! `is_dev` and `report_ready` (startup measurement).

/// Debug build? The UI keeps reload/devtools keys only in debug builds.
#[tauri::command]
pub fn is_dev() -> bool {
    cfg!(debug_assertions)
}

/// The UI calls this once the main window is shown: after the themed first paint, or
/// after showing the "Startup failed" message when startup failed. From then on a second
/// launch may show and raise the window (`bring_to_front`, which also treats an already
/// visible window as ready).
///
/// `ms` is `performance.now()` at show (`window.__blinker_ready_ms`); `marks` are the
/// UI's startup steps as epoch ms. When `BLINKER_READY_LOG` names a file, one line
/// `ready_ms=<ms> timeline=<json>` is appended to it (startup measurement,
/// tools/measure_startup.ps1): the backend's and the UI's steps in ms since the process
/// was created. Otherwise only the ready flag is set.
#[tauri::command]
pub fn report_ready(ms: f64, marks: Option<serde_json::Map<String, serde_json::Value>>) {
    crate::WINDOW_READY.store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(p) = std::env::var_os("BLINKER_READY_LOG").filter(|p| !p.is_empty()) {
        use std::io::Write;
        let line = format!("ready_ms={ms:.1} timeline={}", startup_timeline(marks.unwrap_or_default()));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Backend marks + UI marks (epoch ms) as ms since process creation, in time order.
fn startup_timeline(ui: serde_json::Map<String, serde_json::Value>) -> String {
    let base = crate::timeline::process_start_ms().unwrap_or(0.0);
    let mut all: Vec<(String, f64)> = crate::timeline::marks().into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    all.extend(ui.into_iter().filter_map(|(k, v)| v.as_f64().map(|v| (k, v))));
    all.sort_by(|a, b| a.1.total_cmp(&b.1));
    let rel: Vec<(String, f64)> = all.into_iter().map(|(k, v)| (k, ((v - base) * 10.0).round() / 10.0)).collect();
    serde_json::to_string(&rel).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_timeline_merges_ui_marks_in_time_order_relative_to_process_start() {
        let now = crate::timeline::now_ms();
        let ui = serde_json::json!({ "shown": now + 50.0, "js_start": now - 10.0, "bad": "x" });
        let s = startup_timeline(ui.as_object().unwrap().clone());
        let v: Vec<(String, f64)> = serde_json::from_str(&s).unwrap();
        let names: Vec<&str> = v.iter().map(|(k, _)| k.as_str()).collect();
        let (js, shown) = (names.iter().position(|n| *n == "js_start").unwrap(), names.iter().position(|n| *n == "shown").unwrap());
        assert!(js < shown, "{s}");
        assert!(!names.contains(&"bad"));
        assert!(v.windows(2).all(|w| w[0].1 <= w[1].1), "{s}");
        // relative to this (test) process's creation: positive, below an hour
        assert!(v[shown].1 > 0.0 && v[shown].1 < 3_600_000.0, "{s}");
    }
}
