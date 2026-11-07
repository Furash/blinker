use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::close::close_gracefully;
use super::monitor::monitor;
use super::spawn::{resolve, spawn, LaunchSpec, Plan};
use super::state::{lock, ChildRef, OutputSink, Shared, Slot, State};

const KILL_LINE: &str = "[kill] process killed\n";
/// Upper bound for a launch waiting on the previous process's `exited` delivery.
const FINISHING_WAIT: Duration = Duration::from_secs(2);

pub struct Supervisor {
    pub(super) shared: Arc<Shared>,
}

impl Supervisor {
    pub fn new(sink: Arc<dyn OutputSink>) -> Self {
        Self::with_temp_dir(sink, std::env::temp_dir())
    }

    pub(crate) fn with_temp_dir(sink: Arc<dyn OutputSink>, temp_dir: PathBuf) -> Self {
        Supervisor {
            shared: Arc::new(Shared {
                state: Mutex::new(State { slots: HashMap::new(), shutting_down: false }),
                finished: Condvar::new(),
                sink,
                temp_dir,
                #[cfg(test)]
                gate: Mutex::new(None),
            }),
        }
    }

    /// `_launch` + `_spawn_blender`. Errors carry the message the UI should show.
    pub fn launch(&self, spec: LaunchSpec) -> Result<(), String> {
        let folder = &spec.folder;
        let addon = Path::new(&folder.path);
        if !addon.is_dir() {
            return Err(format!("Path not found:\n{}", folder.path));
        }
        if !addon.join("blender_manifest.toml").exists() && !addon.join("__init__.py").exists() {
            return Err(format!(
                "Not a Blender addon (no __init__.py or blender_manifest.toml):\n{}",
                folder.path
            ));
        }
        if !spec.bootstrap.is_file() {
            return Err(format!("bootstrap.py not found at:\n{}", spec.bootstrap.display()));
        }

        let legacy = !addon.join("blender_manifest.toml").exists();
        let module = if folder.module.is_empty() {
            addon.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            folder.module.clone()
        };
        let resolved = resolve(addon);
        let port = folder.port;
        let repo_env = if folder.repo.is_empty() { "blinker".to_string() } else { folder.repo.clone() };
        let plan = Plan {
            program: spec.blender.clone(),
            bootstrap: spec.bootstrap.clone(),
            env: vec![
                ("BLINKER_ADDON_PATH", resolved.clone()),
                ("BLINKER_MODULE", module.clone()),
                ("BLINKER_REPO", repo_env),
                ("BLINKER_PORT", port.to_string()),
                ("BLINKER_LEGACY", if legacy { "1".into() } else { String::new() }),
            ],
            args_override: spec.args_override.clone(),
        };
        let module_line = if legacy {
            format!("(legacy) {module}")
        } else {
            format!("bl_ext.{}.{module}", folder.repo)
        };
        let header = format!(
            "$ {} --python {}\n  addon:  {resolved}\n  module: {module_line}\n  port:   {port}\n\n",
            spec.blender,
            spec.bootstrap.display()
        );
        let blend = spec.blend.clone().filter(|b| !b.is_empty());

        // Port check, spawn and registration happen under one lock so that a concurrent
        // launch or shutdown can never miss (and orphan) the new process.
        let (child, reader, seq) = {
            let mut st = lock(&self.shared.state);
            // The previous Blender on this port has exited but its `exited` event is
            // still being delivered: wait, so that event cannot follow our header.
            let deadline = Instant::now() + FINISHING_WAIT;
            while st.slots.get(&port).is_some_and(|s| s.finishing) {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(format!("Port {port}: the previous Blender is still exiting."));
                }
                st = self.shared.finished.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0;
            }
            if st.shutting_down {
                return Err("Blinker is shutting down".into());
            }
            if st.slots.get(&port).is_some_and(|s| s.alive) {
                return Err(format!("Port {port} is already used by a running Blender."));
            }
            let (child, reader) = spawn(&plan, blend.as_deref())
                .map_err(|e| format!("Could not start Blender:\n{}\n{e}", spec.blender))?;
            let child: ChildRef = Arc::new(Mutex::new(child));
            let slot = st.slots.entry(port).or_insert_with(Slot::new);
            let seq = slot.history.append(&header);
            slot.alive = true;
            slot.stop_requested = false;
            slot.child = Some(child.clone());
            (child, reader, seq)
        };
        self.shared.sink.push(port, &header, seq);

        let shared = self.shared.clone();
        let child2 = child.clone();
        let started = std::thread::Builder::new()
            .name(format!("blender-{port}"))
            .spawn(move || monitor(shared, port, plan, blend, child2, reader));
        if let Err(e) = started {
            let _ = lock(&child).kill();
            if let Some(slot) = lock(&self.shared.state).slots.get_mut(&port) {
                slot.alive = false;
                slot.child = None;
            }
            return Err(format!("Could not start Blender monitor: {e}"));
        }
        Ok(())
    }

    /// `_kill` when a process is running: stop any pending restart and kill Blender.
    /// Returns false when nothing is supervised on `port` (the caller then falls back
    /// to the TCP `kill` command, as Python does).
    pub fn kill(&self, port: u16) -> bool {
        let (child, seq) = {
            let mut st = lock(&self.shared.state);
            match st.slots.get_mut(&port) {
                Some(s) if s.alive => {
                    s.stop_requested = true;
                    let seq = s.history.append(KILL_LINE);
                    (s.child.clone(), seq)
                }
                _ => return false,
            }
        };
        self.shared.sink.push(port, KILL_LINE, seq);
        if let Some(c) = child {
            let _ = lock(&c).kill();
        }
        true
    }

    pub fn is_alive(&self, port: u16) -> bool {
        lock(&self.shared.state).slots.get(&port).is_some_and(|s| s.alive)
    }

    /// Ports with a live supervised Blender (Import Config refuses while any exists).
    pub fn alive_ports(&self) -> Vec<u16> {
        let mut v: Vec<u16> = lock(&self.shared.state).slots.iter().filter(|(_, s)| s.alive).map(|(p, _)| *p).collect();
        v.sort_unstable();
        v
    }

    #[cfg(test)]
    pub fn output(&self, port: u16) -> String {
        self.output_with_seq(port).0
    }

    /// Console text plus its sequence number (pushes with a larger `seq` came later).
    pub fn output_with_seq(&self, port: u16) -> (String, u64) {
        lock(&self.shared.state)
            .slots
            .get(&port)
            .map(|s| (s.history.text(), s.history.total))
            .unwrap_or_default()
    }

    /// Append a line produced by the app (e.g. `[reload] ok`) to a port's console.
    pub fn append(&self, port: u16, text: &str) {
        let mut st = lock(&self.shared.state);
        st.slots.entry(port).or_insert_with(Slot::new);
        drop(st);
        self.shared.emit(port, text);
    }

    /// Empty the console; returns its sequence number (pushes with a larger `seq` are
    /// newer than the clear).
    pub fn clear(&self, port: u16) -> u64 {
        match lock(&self.shared.state).slots.get_mut(&port) {
            Some(s) => {
                s.history.clear();
                s.history.total
            }
            None => 0,
        }
    }

    /// `stop_requested = True; proc.terminate(); waitForFinished(500)` (folder removal):
    /// no restart; ask Blender to close (WM_CLOSE to its top-level windows, so it quits
    /// gracefully), wait up to 500 ms, then hard-kill it if it is still running.
    /// Returns false when nothing is supervised on `port`.
    pub fn terminate(&self, port: u16) -> bool {
        let child = {
            let mut st = lock(&self.shared.state);
            match st.slots.get_mut(&port) {
                Some(s) if s.alive => {
                    s.stop_requested = true;
                    s.child.clone()
                }
                _ => return false,
            }
        };
        close_gracefully(child.into_iter().collect());
        true
    }

    /// `closeEvent`: no restarts or launches from now on (flags set under the state
    /// lock before anything is closed), then WM_CLOSE to every Blender, wait up to
    /// 500 ms for them, and hard-kill the stragglers.
    pub fn shutdown(&self) {
        let children: Vec<ChildRef> = {
            let mut st = lock(&self.shared.state);
            st.shutting_down = true;
            st.slots
                .values_mut()
                .filter(|s| s.alive)
                .filter_map(|s| {
                    s.stop_requested = true;
                    s.child.clone()
                })
                .collect()
        };
        close_gracefully(children);
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blender::test_support::*;
    use std::fs;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn launch_validates_folder_and_bootstrap() {
        let e = env();
        let mut spec = e.cmd("valid", 9002, "exit 0");
        spec.folder.path = e.t.path().join("missing").to_string_lossy().into_owned();
        let err = e.sup.launch(spec.clone()).unwrap_err();
        assert!(err.starts_with("Path not found:\n"), "{err}");

        let plain = e.t.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        spec.folder.path = plain.to_string_lossy().into_owned();
        let err = e.sup.launch(spec.clone()).unwrap_err();
        assert!(err.starts_with("Not a Blender addon (no __init__.py or blender_manifest.toml):\n"), "{err}");

        let mut spec = e.cmd("valid", 9002, "exit 0");
        spec.bootstrap = e.t.path().join("nope.py");
        let err = e.sup.launch(spec).unwrap_err();
        assert!(err.starts_with("bootstrap.py not found at:\n"), "{err}");
        assert!(!e.sup.is_alive(9002));
    }

    #[test]
    fn manifest_addon_uses_extension_module_and_env() {
        let e = env();
        let mut spec = e.cmd("ext_addon", 9003, "chcp 65001>nul& set BLINKER");
        let dir = PathBuf::from(&spec.folder.path);
        fs::write(dir.join("blender_manifest.toml"), b"").unwrap();
        spec.folder.module = "custom_mod".into();
        spec.folder.repo = "myrepo".into();
        e.sup.launch(spec).unwrap();
        e.sink.wait_exits(1);
        let out = e.sup.output(9003);
        assert!(out.contains("\n  module: bl_ext.myrepo.custom_mod\n"), "{out}");
        assert!(out.contains("BLINKER_MODULE=custom_mod\r\n"), "{out}");
        assert!(out.contains("BLINKER_REPO=myrepo\r\n"), "{out}");
        assert!(out.contains("BLINKER_PORT=9003\r\n"), "{out}");
        // BLINKER_LEGACY is "" -> not defined in the child environment on Windows
        assert!(!out.contains("BLINKER_LEGACY=1"), "{out}");
    }

    #[test]
    fn kill_terminates_running_child() {
        let e = env();
        assert!(!e.sup.kill(9006), "nothing to kill");
        e.sup.launch(e.cmd("busy", 9006, BUSY)).unwrap();
        assert!(e.sup.is_alive(9006));
        assert!(e.sup.kill(9006));
        let exits = e.sink.wait_exits(1);
        assert_eq!(exits.len(), 1);
        assert_ne!(exits[0].1, Some(75));
        assert!(!e.sup.is_alive(9006));
        let out = e.sup.output(9006);
        assert!(out.contains("[kill] process killed\n\n[blender exited code "), "{out}");
    }

    #[test]
    fn refuses_second_launch_on_same_port() {
        let e = env();
        e.sup.launch(e.cmd("first", 9007, BUSY)).unwrap();
        // a different folder on the same port
        let err = e.sup.launch(e.cmd("second", 9007, "exit 0")).unwrap_err();
        assert!(err.contains("9007"), "{err}");
        let out = e.sup.output(9007);
        assert_eq!(out.matches("$ cmd.exe").count(), 1, "{out}");
        assert!(e.sup.kill(9007));
        e.sink.wait_exits(1);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(lock(&e.sink.rec).exits.len(), 1, "no second process was started");
        // once stopped the port is free again
        e.sup.launch(e.cmd("second", 9007, "exit 0")).unwrap();
        e.sink.wait_exits(2);
        // like Python, the same folder may run on a different port (only ports clash)
        e.sup.launch(e.cmd("second", 9008, "exit 0")).unwrap();
        e.sink.wait_exits(3);
    }

    /// A finished process's `exited` reaches the sink before a new launch on the
    /// same port pushes its header, so it cannot be attributed to the new launch.
    #[test]
    fn exited_is_delivered_before_port_is_reused() {
        struct Slow {
            events: Mutex<Vec<String>>,
            entered: Mutex<Option<mpsc::Sender<()>>>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        impl OutputSink for Slow {
            fn push(&self, _port: u16, text: &str, _seq: u64) {
                lock(&self.events).push(format!("push:{text}"));
            }
            fn exited(&self, _port: u16, _code: Option<i32>) {
                lock(&self.events).push("exited".into());
                if let Some(tx) = lock(&self.entered).take() {
                    let _ = tx.send(());
                    let _ = lock(&self.release).recv_timeout(WAIT);
                }
            }
        }
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let sink = Arc::new(Slow {
            events: Mutex::new(Vec::new()),
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(release_rx),
        });
        let e = env(); // folders/bootstrap only
        let t = tempfile::tempdir().unwrap();
        let sup = Arc::new(Supervisor::with_temp_dir(sink.clone(), t.path().to_path_buf()));
        sup.launch(e.cmd("reuse", 9018, "exit 0")).unwrap();
        entered_rx.recv_timeout(WAIT).expect("exited delivery started");
        assert!(!sup.is_alive(9018));
        let (done_tx, done_rx) = mpsc::channel();
        let (s2, spec2) = (sup.clone(), e.cmd("reuse", 9018, "exit 0"));
        std::thread::spawn(move || {
            let _ = done_tx.send(s2.launch(spec2));
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(300)).is_err(), "launch waits for exited");
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(WAIT).expect("launch finished").unwrap();
        let ev = lock(&sink.events).clone();
        let first_exit = ev.iter().position(|x| x == "exited").unwrap();
        let headers: Vec<usize> =
            ev.iter().enumerate().filter(|(_, x)| x.starts_with("push:$ cmd.exe")).map(|(i, _)| i).collect();
        assert_eq!(headers.len(), 2, "{ev:?}");
        assert!(first_exit < headers[1], "{ev:?}");
        sup.shutdown();
    }

    #[test]
    fn shutdown_terminates_running_children() {
        let e = env();
        e.sup.launch(e.cmd("a", 9010, BUSY)).unwrap();
        e.sup.launch(e.cmd("b", 9011, BUSY)).unwrap();
        let t0 = Instant::now();
        e.sup.shutdown();
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
        let exits = e.sink.wait_exits(2);
        assert_eq!(exits.len(), 2);
        assert!(!e.sup.is_alive(9010) && !e.sup.is_alive(9011));
    }

    /// A sink that blocks inside `push` (a slow UI) must not block the supervisor's API.
    #[test]
    fn flood_with_slow_sink_keeps_api_responsive() {
        struct Blocking {
            entered: Mutex<Option<mpsc::Sender<()>>>,
            release: Mutex<mpsc::Receiver<()>>,
            exited: Mutex<Option<mpsc::Sender<()>>>,
        }
        impl OutputSink for Blocking {
            fn push(&self, _port: u16, text: &str, _seq: u64) {
                if text.contains("flood") {
                    if let Some(tx) = lock(&self.entered).take() {
                        let _ = tx.send(());
                        let _ = lock(&self.release).recv_timeout(WAIT);
                    }
                }
            }
            fn exited(&self, _port: u16, _code: Option<i32>) {
                if let Some(tx) = lock(&self.exited).take() {
                    let _ = tx.send(());
                }
            }
        }
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (exited_tx, exited_rx) = mpsc::channel();
        let sink = Arc::new(Blocking {
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(release_rx),
            exited: Mutex::new(Some(exited_tx)),
        });
        let t = tempfile::tempdir().unwrap();
        let sup = Arc::new(Supervisor::with_temp_dir(sink, t.path().to_path_buf()));
        let e = env(); // only for folders/bootstrap
        sup.launch(e.cmd("slow", 9014, "for /L %i in (1,1,3000) do @echo flood %i")).unwrap();
        entered_rx.recv_timeout(WAIT).expect("sink push entered");
        // the sink is stuck inside push(): the API must still answer
        let (tx, rx) = mpsc::channel();
        let s2 = sup.clone();
        std::thread::spawn(move || {
            let out = s2.output(9014);
            let alive = s2.is_alive(9014);
            s2.clear(9014);
            let _ = tx.send((out, alive));
        });
        let (out, _alive) = rx.recv_timeout(Duration::from_secs(3)).expect("API blocked while sink was busy");
        assert!(out.contains("flood 1"), "{out:?}");
        release_tx.send(()).unwrap();
        exited_rx.recv_timeout(WAIT).expect("process finished");
        assert!(!sup.is_alive(9014));
    }
}
