//! Shared app state, status payload and probe epochs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use tauri::{AppHandle, Emitter, Manager, State};

use crate::blender::{OutputSink, Supervisor};
use crate::config::Config;
use crate::net;

/// Shared state managed by Tauri.
pub struct AppState {
    pub config_path: PathBuf,
    pub config: Mutex<Config>,
    pub supervisor: Arc<Supervisor>,
    pub ports: Arc<Mutex<Vec<u16>>>,
    /// Last applicable TCP probe per port (poller and `probe_now`). Its lock also
    /// serialises `publish_status`, so status events leave in the order they were built.
    pub last_probe: Mutex<ProbeState>,
}

pub(super) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Forwards Blender output and exit notifications to the webview.
pub struct TauriSink {
    pub app: AppHandle,
}

impl OutputSink for TauriSink {
    fn push(&self, port: u16, text: &str, seq: u64) {
        let _ = self.app.emit("blinker://output", serde_json::json!({ "port": port, "text": text, "seq": seq }));
    }
    fn exited(&self, port: u16, code: Option<i32>) {
        let _ = self.app.emit("blinker://exited", serde_json::json!({ "port": port, "code": code }));
        // Python `_on_proc_finished`: running = False now, until a later poll says otherwise.
        // The supervisor already marked the port not alive.
        publish_status_with(&self.app, |ps| ps.exited(port));
    }
}

/// `blinker://status` payload: for every listed port, running = a Blender answers the
/// last probe OR this app supervises a live Blender on it (Python `_apply_statuses`:
/// `statuses.get(port) or proc_alive`). The backend owns alive-ness, so the UI never
/// has to infer it from the order of `launch` replies and `exited` events.
pub fn status_payload(
    ports: &[u16],
    last_probe: &BTreeMap<u16, bool>,
    alive: impl Fn(u16) -> bool,
) -> BTreeMap<String, bool> {
    ports
        .iter()
        .map(|&p| (p.to_string(), last_probe.get(&p).copied().unwrap_or(false) || alive(p)))
        .collect()
}

/// Probe results with per-port epochs, so a probe can never report a state older than
/// a launch or exit that happened while it ran, and overlapping probes cannot store an
/// older result over a newer one.
#[derive(Debug, Default)]
pub struct ProbeState {
    probe: BTreeMap<u16, bool>,
    /// Bumped on every launch and exit of a supervised Blender on the port.
    epoch: BTreeMap<u16, u64>,
    /// Start number of the probe whose result is stored for the port.
    applied: BTreeMap<u16, u64>,
    started: u64,
}

/// Taken when a probe starts; `ProbeState::apply` uses it to drop stale results.
#[derive(Debug, Clone)]
pub struct ProbeTicket {
    seq: u64,
    epochs: BTreeMap<u16, u64>,
}

impl ProbeState {
    pub fn begin(&mut self, ports: &[u16]) -> ProbeTicket {
        self.started += 1;
        let epochs = ports.iter().map(|&p| (p, self.epoch.get(&p).copied().unwrap_or(0))).collect();
        ProbeTicket { seq: self.started, epochs }
    }

    /// Store results whose port saw no launch/exit since `t` began and no newer probe.
    pub fn apply(&mut self, t: &ProbeTicket, results: &BTreeMap<u16, bool>) {
        for (&p, &v) in results {
            let same_epoch = t.epochs.get(&p) == Some(&self.epoch.get(&p).copied().unwrap_or(0));
            let newer = self.applied.get(&p).map_or(true, |&a| t.seq > a);
            if same_epoch && newer {
                self.probe.insert(p, v);
                self.applied.insert(p, t.seq);
            }
        }
    }

    /// A supervised Blender exited: not running until a probe begun later says so.
    pub fn exited(&mut self, port: u16) {
        *self.epoch.entry(port).or_insert(0) += 1;
        self.probe.insert(port, false);
    }

    /// A supervised Blender started (alive covers it); earlier probes are stale.
    pub fn launched(&mut self, port: u16) {
        *self.epoch.entry(port).or_insert(0) += 1;
    }

    #[cfg(test)]
    pub fn get(&self, port: u16) -> bool {
        self.probe.get(&port).copied().unwrap_or(false)
    }
}

/// Update the probe state under its lock, then emit the full status (still under it).
pub fn publish_status_with(app: &AppHandle, update: impl FnOnce(&mut ProbeState)) {
    let Some(st) = app.try_state::<AppState>() else { return };
    let mut ps = lock(&st.last_probe);
    update(&mut ps);
    let ports = lock(&st.ports).clone();
    let payload = status_payload(&ports, &ps.probe, |p| st.supervisor.is_alive(p));
    let _ = app.emit("blinker://status", payload);
}

/// Start a probe (see `ProbeState::begin`).
pub fn probe_begin(app: &AppHandle, ports: &[u16]) -> Option<ProbeTicket> {
    app.try_state::<AppState>().map(|st| lock(&st.last_probe).begin(ports))
}

/// Apply a finished probe and publish.
pub fn probe_finish(app: &AppHandle, t: Option<ProbeTicket>, results: BTreeMap<u16, bool>) {
    if let Some(t) = t {
        publish_status_with(app, |ps| ps.apply(&t, &results));
    }
}

/// Emit the current status without probing (the UI asks after a `launch` reply, so it
/// sees a status built after the launch finished).
#[tauri::command]
pub async fn status_now(app: AppHandle) -> Result<(), String> {
    publish_status_with(&app, |_| {});
    Ok(())
}

/// Probe the given ports now and emit the full status (`publish_status_with`).
#[tauri::command]
pub async fn probe_now(app: AppHandle, ports: Vec<u16>) -> Result<(), String> {
    let ticket = probe_begin(&app, &ports);
    let running = tauri::async_runtime::spawn_blocking(move || net::probe(&ports))
        .await
        .map_err(|e| e.to_string())?;
    probe_finish(&app, ticket, running);
    Ok(())
}

#[tauri::command]
pub async fn set_ports(state: State<'_, AppState>, ports: Vec<u16>) -> Result<(), String> {
    *lock(&state.ports) = ports;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_begun_before_exit_cannot_resurrect_running() {
        let mut ps = ProbeState::default();
        let t0 = ps.begin(&[1]);
        ps.apply(&t0, &BTreeMap::from([(1, true)]));
        assert!(ps.get(1));
        let before = ps.begin(&[1]); // started while Blender still answered
        ps.exited(1);
        assert!(!ps.get(1), "exit clears running at once");
        ps.apply(&before, &BTreeMap::from([(1, true)]));
        assert!(!ps.get(1), "stale probe ignored");
        let after = ps.begin(&[1]); // e.g. an external Blender answers later
        ps.apply(&after, &BTreeMap::from([(1, true)]));
        assert!(ps.get(1), "probe begun after the exit applies");
    }

    #[test]
    fn overlapping_probes_keep_the_newer_result_and_launch_bumps_epoch() {
        let mut ps = ProbeState::default();
        let older = ps.begin(&[1, 2]);
        let newer = ps.begin(&[1, 2]);
        ps.apply(&newer, &BTreeMap::from([(1, false), (2, true)]));
        ps.apply(&older, &BTreeMap::from([(1, true), (2, false)]));
        assert!(!ps.get(1) && ps.get(2), "older overlapping probe must not win");
        let pre_launch = ps.begin(&[2]);
        ps.launched(2);
        ps.apply(&pre_launch, &BTreeMap::from([(2, false)]));
        assert!(ps.get(2), "probe begun before the launch is stale");
        // ports outside the ticket are ignored
        let t = ps.begin(&[1]);
        ps.apply(&t, &BTreeMap::from([(3, true)]));
        assert!(!ps.get(3));
    }

    #[test]
    fn status_payload_is_probe_or_alive_and_missing_probe_is_false() {
        let probe = BTreeMap::from([(1u16, true), (2, false), (9, true)]);
        let s = status_payload(&[1, 2, 3, 4], &probe, |p| p == 2 || p == 4);
        let want: BTreeMap<String, bool> =
            [("1", true), ("2", true), ("3", false), ("4", true)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
        assert_eq!(s, want); // port 9 is not listed any more: left out
    }
}
