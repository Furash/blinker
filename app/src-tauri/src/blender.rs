use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{self, PipeReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::config::Folder;

#[allow(dead_code)]
pub fn find_blender(env_path: Option<&str>) -> Option<String> {
    let roots: Vec<PathBuf> = ["PROGRAMFILES", "PROGRAMFILES(X86)"]
        .iter()
        .filter_map(|v| std::env::var_os(v))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .collect();
    find_blender_in(env_path, std::env::var_os("PATH"), &roots)
}

fn find_blender_in(
    env_path: Option<&str>,
    path_var: Option<OsString>,
    program_roots: &[PathBuf],
) -> Option<String> {
    if let Some(e) = env_path {
        if !e.is_empty() && Path::new(e).is_file() {
            return Some(e.to_string());
        }
    }
    if let Some(pv) = path_var {
        for dir in std::env::split_paths(&pv) {
            let exe = dir.join("blender.exe");
            if exe.is_file() {
                return Some(exe.to_string_lossy().into_owned());
            }
        }
    }
    for root in program_roots {
        let bf = root.join("Blender Foundation");
        let Ok(rd) = std::fs::read_dir(&bf) else { continue };
        let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        for d in dirs {
            let exe = d.join("blender.exe");
            if exe.is_file() {
                return Some(exe.to_string_lossy().into_owned());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Blender supervisor
// ---------------------------------------------------------------------------

/// `QPlainTextEdit.setMaximumBlockCount(20000)` in the Python UI.
pub const HISTORY_MAX_LINES: usize = 20000;

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct LaunchSpec {
    pub blender: String,
    pub bootstrap: PathBuf,
    pub folder: Folder,
    pub blend: Option<String>,
    /// Test seam: replaces `--python <bootstrap> [blend]`; each entry is appended raw
    /// (unquoted) to the command line. Always `None` in the app.
    #[doc(hidden)]
    pub args_override: Option<Vec<String>>,
}

pub trait OutputSink: Send + Sync {
    fn push(&self, port: u16, text: &str);
    fn exited(&self, port: u16, code: Option<i32>);
}

const RESTART_EXIT_CODE: i32 = 75;
const RESTART_MARKER: &str = "blinker_restart_path";
const RESTART_BLEND: &str = "blinker_restart.blend";
const KILL_LINE: &str = "[kill] process killed\n";
const RESTARTING_LINE: &str = "\n[restarting blender...]\n\n";
/// `closeEvent`: `terminate()` + `waitForFinished(500)` per Blender.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(500);
/// How often the monitor checks whether Blender has exited.
const POLL: Duration = Duration::from_millis(20);
/// After exit, how long to wait for output still in the pipe (a grandchild may hold it open).
const DRAIN_TIMEOUT: Duration = Duration::from_millis(1000);
const READ_CHUNK: usize = 16 * 1024;
/// Upper bound for one coalesced sink push while Blender floods output.
const MAX_EMIT: usize = 256 * 1024;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Console history, one entry per text block (like `QPlainTextEdit` blocks:
/// newline count + 1), capped at `HISTORY_MAX_LINES`.
struct History {
    lines: VecDeque<String>,
}

impl History {
    fn new() -> Self {
        History { lines: VecDeque::from([String::new()]) }
    }
    fn append(&mut self, text: &str) {
        let mut parts = text.split('\n');
        if let Some(first) = parts.next() {
            if let Some(last) = self.lines.back_mut() {
                last.push_str(first);
            }
        }
        for p in parts {
            self.lines.push_back(p.to_string());
        }
        while self.lines.len() > HISTORY_MAX_LINES {
            self.lines.pop_front();
        }
    }
    fn text(&self) -> String {
        let len = self.lines.iter().map(|l| l.len() + 1).sum();
        let mut s = String::with_capacity(len);
        for (i, l) in self.lines.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            s.push_str(l);
        }
        s
    }
}

/// Incremental UTF-8 decoder: invalid bytes become U+FFFD, a sequence split across
/// two reads is kept until the next read completes it.
#[derive(Default)]
struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    fn decode(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let mut out = String::with_capacity(self.pending.len());
        let mut rest: &[u8] = &self.pending;
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    rest = &[];
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    // from_utf8 just validated this prefix, so this never falls back.
                    out.push_str(std::str::from_utf8(&rest[..valid]).unwrap_or_default());
                    match e.error_len() {
                        Some(n) => {
                            out.push('\u{FFFD}');
                            rest = &rest[valid + n..];
                        }
                        None => {
                            rest = &rest[valid..];
                            break;
                        }
                    }
                }
            }
        }
        self.pending = rest.to_vec();
        out
    }
    fn finish(&mut self) -> String {
        let out = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        out
    }
}

type ChildRef = Arc<Mutex<Child>>;

struct Slot {
    history: History,
    alive: bool,
    stop_requested: bool,
    child: Option<ChildRef>,
    /// Lower-cased resolved addon path, to refuse launching the same addon twice.
    addon_key: String,
}

impl Slot {
    fn new() -> Self {
        Slot { history: History::new(), alive: false, stop_requested: false, child: None, addon_key: String::new() }
    }
}

struct State {
    slots: HashMap<u16, Slot>,
    shutting_down: bool,
}

struct Shared {
    state: Mutex<State>,
    sink: Arc<dyn OutputSink>,
    /// `tempfile.gettempdir()`: where bootstrap.py leaves the restart markers.
    temp_dir: PathBuf,
    /// Test seam: pauses the monitor between Blender's exit and the restart decision.
    #[cfg(test)]
    gate: Mutex<Option<(mpsc::Sender<Option<i32>>, mpsc::Receiver<()>)>>,
}

impl Shared {
    /// Append to the history, then hand the text to the sink with no lock held.
    fn emit(&self, port: u16, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(slot) = lock(&self.state).slots.get_mut(&port) {
            slot.history.append(text);
        }
        self.sink.push(port, text);
    }
}

/// Everything needed to (re)spawn the same Blender with the same environment.
struct Plan {
    program: String,
    bootstrap: PathBuf,
    env: Vec<(&'static str, String)>,
    args_override: Option<Vec<String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Supervisor {
    shared: Arc<Shared>,
}

#[allow(dead_code)]
impl Supervisor {
    pub fn new(sink: Arc<dyn OutputSink>) -> Self {
        Self::with_temp_dir(sink, std::env::temp_dir())
    }

    fn with_temp_dir(sink: Arc<dyn OutputSink>, temp_dir: PathBuf) -> Self {
        Supervisor {
            shared: Arc::new(Shared {
                state: Mutex::new(State { slots: HashMap::new(), shutting_down: false }),
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
        let key = resolved.to_lowercase();

        // Port check, spawn and registration happen under one lock so that a concurrent
        // launch or shutdown can never miss (and orphan) the new process.
        let (child, reader) = {
            let mut st = lock(&self.shared.state);
            if st.shutting_down {
                return Err("Blinker is shutting down".into());
            }
            if st.slots.get(&port).is_some_and(|s| s.alive) {
                return Err(format!("Port {port} is already used by a running Blender."));
            }
            if let Some((p, _)) = st.slots.iter().find(|(_, s)| s.alive && s.addon_key == key) {
                return Err(format!("This addon is already running in Blender on port {p}."));
            }
            let (child, reader) = spawn(&plan, blend.as_deref())
                .map_err(|e| format!("Could not start Blender:\n{}\n{e}", spec.blender))?;
            let child: ChildRef = Arc::new(Mutex::new(child));
            let slot = st.slots.entry(port).or_insert_with(Slot::new);
            slot.history.append(&header);
            slot.alive = true;
            slot.stop_requested = false;
            slot.addon_key = key;
            slot.child = Some(child.clone());
            (child, reader)
        };
        self.shared.sink.push(port, &header);

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
        let child = {
            let mut st = lock(&self.shared.state);
            match st.slots.get_mut(&port) {
                Some(s) if s.alive => {
                    s.stop_requested = true;
                    s.history.append(KILL_LINE);
                    s.child.clone()
                }
                _ => return false,
            }
        };
        self.shared.sink.push(port, KILL_LINE);
        if let Some(c) = child {
            let _ = lock(&c).kill();
        }
        true
    }

    pub fn is_alive(&self, port: u16) -> bool {
        lock(&self.shared.state).slots.get(&port).is_some_and(|s| s.alive)
    }

    pub fn output(&self, port: u16) -> String {
        lock(&self.shared.state).slots.get(&port).map(|s| s.history.text()).unwrap_or_default()
    }

    /// Append a line produced by the app (e.g. `[reload] ok`) to a port's console.
    pub fn append(&self, port: u16, text: &str) {
        let mut st = lock(&self.shared.state);
        st.slots.entry(port).or_insert_with(Slot::new);
        drop(st);
        self.shared.emit(port, text);
    }

    pub fn clear(&self, port: u16) {
        if let Some(s) = lock(&self.shared.state).slots.get_mut(&port) {
            s.history = History::new();
        }
    }

    /// `closeEvent`: no restarts or launches from now on; kill every Blender and wait
    /// up to 500 ms for each.
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
        for c in children {
            let _ = lock(&c).kill();
            let deadline = Instant::now() + SHUTDOWN_WAIT;
            while !matches!(lock(&c).try_wait(), Ok(Some(_)) | Err(_)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// `Path.resolve()`: absolute, links resolved, without the `\\?\` prefix.
fn resolve(p: &Path) -> String {
    let abs = std::fs::canonicalize(p)
        .or_else(|_| std::path::absolute(p))
        .unwrap_or_else(|_| p.to_path_buf());
    display_path(&abs)
}

fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.into_owned()
    }
}

/// `["--python", BOOTSTRAP] + [blend_file]`
fn blender_args(bootstrap: &Path, blend: Option<&str>) -> Vec<OsString> {
    let mut args = vec![OsString::from("--python"), bootstrap.as_os_str().to_owned()];
    if let Some(b) = blend {
        args.push(OsString::from(b));
    }
    args
}

/// Start Blender with stdout and stderr on one pipe (QProcess `MergedChannels`).
fn spawn(plan: &Plan, blend: Option<&str>) -> io::Result<(Child, PipeReader)> {
    let (reader, writer) = io::pipe()?;
    let writer_err = writer.try_clone()?;
    let mut cmd = Command::new(&plan.program);
    match &plan.args_override {
        #[cfg(windows)]
        Some(raw) => {
            use std::os::windows::process::CommandExt;
            for a in raw {
                cmd.raw_arg(a);
            }
        }
        #[cfg(not(windows))]
        Some(raw) => {
            cmd.args(raw);
        }
        None => {
            cmd.args(blender_args(&plan.bootstrap, blend));
        }
    }
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null()).stdout(writer).stderr(writer_err);
    // QProcess uses CREATE_NO_WINDOW when the parent has no console: Blender's console
    // stays hidden and its output still goes to our pipe (STARTF_USESTDHANDLES).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let child = cmd.spawn()?;
    // `cmd` owns our copies of the write end; drop them so the reader sees EOF.
    drop(cmd);
    Ok((child, reader))
}

fn read_pipe(mut reader: PipeReader, tx: mpsc::SyncSender<String>) {
    let mut dec = Utf8Stream::default();
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let s = dec.decode(&buf[..n]);
                if !s.is_empty() && tx.send(s).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let rest = dec.finish();
    if !rest.is_empty() {
        let _ = tx.send(rest);
    }
}

fn start_reader(port: u16, reader: PipeReader) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::sync_channel(256);
    let spawned = std::thread::Builder::new()
        .name(format!("blender-{port}-out"))
        .spawn(move || read_pipe(reader, tx));
    // If the thread cannot start, `rx` reports Disconnected and output is lost, nothing else.
    drop(spawned);
    rx
}

/// Coalesce whatever output is already queued into one sink push.
fn emit_batch(shared: &Shared, port: u16, first: String, rx: &mpsc::Receiver<String>) {
    let mut text = first;
    while text.len() < MAX_EMIT {
        match rx.try_recv() {
            Ok(more) => text.push_str(&more),
            Err(_) => break,
        }
    }
    shared.emit(port, &text);
}

enum Next {
    Restart(ChildRef, PipeReader),
    Stop,
}

/// One thread per launched Blender: streams output, then handles exit / restart
/// (`_on_proc_output` + `_on_proc_finished`).
fn monitor(
    shared: Arc<Shared>,
    port: u16,
    plan: Plan,
    mut blend: Option<String>,
    mut child: ChildRef,
    mut reader: PipeReader,
) {
    loop {
        let rx = start_reader(port, reader);
        let mut pipe_open = true;
        let code = loop {
            if pipe_open {
                match rx.recv_timeout(POLL) {
                    Ok(text) => emit_batch(&shared, port, text, &rx),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => pipe_open = false,
                }
            } else {
                std::thread::sleep(POLL);
            }
            match lock(&child).try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {}
                Err(_) => break None,
            }
        };
        // QProcess delivers the remaining output before `finished`.
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        while pipe_open {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(text) => emit_batch(&shared, port, text, &rx),
                Err(_) => pipe_open = false,
            }
        }
        drop(rx);
        let code_str = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
        shared.emit(port, &format!("\n[blender exited code {code_str}]\n"));

        #[cfg(test)]
        {
            let gate = lock(&shared.gate);
            if let Some((tx, go)) = gate.as_ref() {
                let _ = tx.send(code);
                let _ = go.recv_timeout(Duration::from_secs(20));
            }
        }

        // The restart decision, the marker read and the respawn happen under the state
        // lock: `kill()` and `shutdown()` either run before (no restart) or see the new child.
        let next = {
            let mut st = lock(&shared.state);
            let shutting_down = st.shutting_down;
            let Some(slot) = st.slots.get_mut(&port) else { return };
            if code == Some(RESTART_EXIT_CODE) && !slot.stop_requested && !shutting_down {
                let marker = shared.temp_dir.join(RESTART_MARKER);
                if marker.is_file() {
                    // Python: read_text(utf-8).strip() or blend_file; remove; errors ignored.
                    if let Ok(Ok(s)) = std::fs::read(&marker).map(String::from_utf8) {
                        let t = s.trim();
                        if !t.is_empty() {
                            blend = Some(t.to_string());
                        }
                        let _ = std::fs::remove_file(&marker);
                    }
                }
                slot.history.append(RESTARTING_LINE);
                match spawn(&plan, blend.as_deref()) {
                    Ok((c, r)) => {
                        let c: ChildRef = Arc::new(Mutex::new(c));
                        slot.child = Some(c.clone());
                        Next::Restart(c, r)
                    }
                    Err(e) => {
                        let msg = format!("[could not restart blender: {e}]\n");
                        slot.history.append(&msg);
                        slot.alive = false;
                        slot.child = None;
                        drop(st);
                        shared.sink.push(port, RESTARTING_LINE);
                        shared.sink.push(port, &msg);
                        shared.sink.exited(port, code);
                        return;
                    }
                }
            } else {
                slot.alive = false;
                slot.child = None;
                Next::Stop
            }
        };
        match next {
            Next::Restart(c, r) => {
                shared.sink.push(port, RESTARTING_LINE);
                child = c;
                reader = r;
            }
            Next::Stop => {
                for name in [RESTART_MARKER, RESTART_BLEND] {
                    let _ = std::fs::remove_file(shared.temp_dir.join(name));
                }
                shared.sink.exited(port, code);
                return;
            }
        }
    }
}

/// The repo-root `bootstrap.py`, copied into OUT_DIR by build.rs.
const BOOTSTRAP_PY: &str = include_str!(concat!(env!("OUT_DIR"), "/bootstrap.py"));
const BOOTSTRAP_NAME: &str = "blinker_bootstrap.py";

/// Writes the embedded bootstrap to `%TEMP%\blinker_bootstrap.py` (only if it differs).
#[allow(dead_code)]
pub fn write_bootstrap() -> io::Result<PathBuf> {
    write_bootstrap_to(&std::env::temp_dir())
}

fn write_bootstrap_to(dir: &Path) -> io::Result<PathBuf> {
    let path = dir.join(BOOTSTRAP_NAME);
    if std::fs::read(&path).ok().as_deref() != Some(BOOTSTRAP_PY.as_bytes()) {
        std::fs::write(&path, BOOTSTRAP_PY)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc;
    use std::sync::Condvar;
    use std::time::{Duration, Instant};

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"").unwrap();
    }

    #[test]
    fn env_path_wins_when_file() {
        let t = tempfile::tempdir().unwrap();
        let exe = t.path().join("mine.exe");
        touch(&exe);
        let other = t.path().join("bin");
        touch(&other.join("blender.exe"));
        let got = find_blender_in(exe.to_str(), Some(other.clone().into_os_string()), &[]);
        assert_eq!(got.as_deref(), exe.to_str());
    }

    #[test]
    fn env_path_missing_falls_through_to_path() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("bin");
        touch(&bin.join("blender.exe"));
        let got = find_blender_in(t.path().join("nope").join("blender.exe").to_str(), Some(bin.clone().into_os_string()), &[]);
        assert_eq!(got.map(PathBuf::from), Some(bin.join("blender.exe")));
    }

    #[test]
    fn program_files_newest_first_then_x86() {
        let t = tempfile::tempdir().unwrap();
        let pf = t.path().join("pf");
        let x86 = t.path().join("x86");
        touch(&pf.join("Blender Foundation").join("Blender 4.2").join("blender.exe"));
        touch(&pf.join("Blender Foundation").join("Blender 4.5").join("blender.exe"));
        touch(&x86.join("Blender Foundation").join("Blender 9.9").join("blender.exe"));
        let got = find_blender_in(None, None, &[pf.clone(), x86.clone()]).unwrap();
        assert_eq!(PathBuf::from(got), pf.join("Blender Foundation").join("Blender 4.5").join("blender.exe"));
        let got = find_blender_in(None, None, &[t.path().join("none"), x86.clone()]).unwrap();
        assert!(got.contains("Blender 9.9"));
    }

    #[test]
    fn nothing_found_is_none() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(find_blender_in(None, Some(t.path().into()), &[t.path().to_path_buf()]), None);
    }

    // ----- supervisor helpers -----

    const WAIT: Duration = Duration::from_secs(20);

    #[derive(Default)]
    struct Rec {
        text: HashMap<u16, String>,
        exits: Vec<(u16, Option<i32>)>,
    }

    #[derive(Default)]
    struct RecSink {
        rec: Mutex<Rec>,
        cv: Condvar,
    }

    impl OutputSink for RecSink {
        fn push(&self, port: u16, text: &str) {
            lock(&self.rec).text.entry(port).or_default().push_str(text);
            self.cv.notify_all();
        }
        fn exited(&self, port: u16, code: Option<i32>) {
            lock(&self.rec).exits.push((port, code));
            self.cv.notify_all();
        }
    }

    impl RecSink {
        fn wait_exits(&self, n: usize) -> Vec<(u16, Option<i32>)> {
            let deadline = Instant::now() + WAIT;
            let mut g = lock(&self.rec);
            while g.exits.len() < n {
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "timed out waiting for {n} exit(s); got {:?}", g.exits);
                g = self.cv.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
            }
            g.exits.clone()
        }
        fn text(&self, port: u16) -> String {
            lock(&self.rec).text.get(&port).cloned().unwrap_or_default()
        }
    }

    struct Env {
        t: tempfile::TempDir,
        sink: Arc<RecSink>,
        sup: Supervisor,
    }

    fn env() -> Env {
        let t = tempfile::tempdir().unwrap();
        let tmp = t.path().join("temp");
        fs::create_dir_all(&tmp).unwrap();
        let sink = Arc::new(RecSink::default());
        let sup = Supervisor::with_temp_dir(sink.clone(), tmp);
        Env { t, sink, sup }
    }

    impl Env {
        fn temp(&self) -> PathBuf {
            self.t.path().join("temp")
        }
        fn addon(&self, name: &str, port: u16) -> Folder {
            let dir = self.t.path().join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("__init__.py"), b"").unwrap();
            Folder { path: dir.to_string_lossy().into_owned(), port, ..Folder::default() }
        }
        fn bootstrap(&self) -> PathBuf {
            let p = self.t.path().join("bootstrap.py");
            fs::write(&p, b"# dummy").unwrap();
            p
        }
        fn cmd(&self, name: &str, port: u16, script: &str) -> LaunchSpec {
            LaunchSpec {
                blender: "cmd.exe".into(),
                bootstrap: self.bootstrap(),
                folder: self.addon(name, port),
                blend: None,
                args_override: Some(vec!["/d".into(), "/c".into(), script.into()]),
            }
        }
        fn bat(&self, name: &str, body: &str) -> PathBuf {
            let p = self.t.path().join(name);
            fs::write(&p, body.replace('\n', "\r\n")).unwrap();
            p
        }
        fn set_gate(&self) -> (mpsc::Receiver<Option<i32>>, mpsc::Sender<()>) {
            let (code_tx, code_rx) = mpsc::channel();
            let (go_tx, go_rx) = mpsc::channel();
            *lock(&self.sup.shared.gate) = Some((code_tx, go_rx));
            (code_rx, go_tx)
        }
    }

    const BUSY: &str = "for /L %i in (1,1,2000000000) do @rem";

    // ----- supervisor tests -----

    #[test]
    fn output_is_streamed_and_merged() {
        let e = env();
        let spec = e.cmd("merged", 9001, "echo out1& echo err1 1>&2& echo out2");
        let addon = fs::canonicalize(&spec.folder.path).unwrap();
        let addon = display_path(&addon);
        let boot = spec.bootstrap.clone();
        e.sup.launch(spec).unwrap();
        assert_eq!(e.sink.wait_exits(1), vec![(9001, Some(0))]);
        let out = e.sup.output(9001);
        let header = format!(
            "$ cmd.exe --python {}\n  addon:  {}\n  module: (legacy) merged\n  port:   9001\n\n",
            boot.display(),
            addon
        );
        assert!(!addon.starts_with(r"\\?\"), "{addon}");
        assert!(out.starts_with(&header), "{out:?}\nexpected header {header:?}");
        let i1 = out.find("out1").expect("out1");
        let i2 = out.find("err1").expect("err1 (stderr)");
        let i3 = out.find("out2").expect("out2");
        assert!(i1 < i2 && i2 < i3, "merged order: {out:?}");
        assert!(out.ends_with("\n[blender exited code 0]\n"), "{out:?}");
        assert_eq!(e.sink.text(9001), out, "sink saw the same stream as history");
        assert!(!e.sup.is_alive(9001));
    }

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
    fn exit_75_relaunches_once_then_stops() {
        let e = env();
        let bat = e.bat(
            "fake blender.bat",
            "@echo off\necho ARGS:%*\nif exist \"%~dp0ran\" exit /b 0\necho.>\"%~dp0ran\"\nexit /b 75\n",
        );
        let marker = e.temp().join("blinker_restart_path");
        let restart_blend = e.temp().join("blinker_restart.blend");
        fs::write(&marker, "C:\\restored dir\\scene.blend\n").unwrap();
        fs::write(&restart_blend, b"x").unwrap();
        let spec = LaunchSpec {
            blender: bat.to_string_lossy().into_owned(),
            bootstrap: e.bootstrap(),
            folder: e.addon("restarter", 9004),
            blend: Some("C:\\first.blend".into()),
            args_override: None,
        };
        e.sup.launch(spec).unwrap();
        assert_eq!(e.sink.wait_exits(1), vec![(9004, Some(0))]);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(lock(&e.sink.rec).exits.len(), 1, "exactly one final exit");
        let out = e.sup.output(9004);
        assert_eq!(out.matches("[restarting blender...]").count(), 1, "{out}");
        assert!(out.contains("\n[blender exited code 75]\n\n[restarting blender...]\n\n"), "{out}");
        assert!(out.ends_with("\n[blender exited code 0]\n"), "{out}");
        let args: Vec<&str> = out.lines().filter(|l| l.starts_with("ARGS:")).collect();
        assert_eq!(args.len(), 2, "{out}");
        assert!(args[0].contains("C:\\first.blend"), "{}", args[0]);
        assert!(args[1].contains("\"C:\\restored dir\\scene.blend\""), "{}", args[1]);
        assert!(!marker.exists(), "restart marker consumed");
        assert!(!restart_blend.exists(), "temp restart blend removed on final exit");
        assert!(!e.sup.is_alive(9004));
    }

    #[test]
    fn exit_75_after_kill_does_not_relaunch() {
        let e = env();
        let (code_rx, go) = e.set_gate();
        e.sup.launch(e.cmd("killed", 9005, "exit 75")).unwrap();
        assert_eq!(code_rx.recv_timeout(WAIT).unwrap(), Some(75));
        // Blender has exited with 75 but the restart decision has not been made yet.
        assert!(e.sup.kill(9005), "kill stops a pending restart");
        go.send(()).unwrap();
        assert_eq!(e.sink.wait_exits(1), vec![(9005, Some(75))]);
        let out = e.sup.output(9005);
        assert!(!out.contains("[restarting blender...]"), "{out}");
        assert!(out.contains("[kill] process killed\n"), "{out}");
        assert!(!e.sup.is_alive(9005));
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
        // the same folder after its port was edited while Blender runs
        let err = e.sup.launch(e.cmd("first", 9008, "exit 0")).unwrap_err();
        assert!(err.contains("9007"), "{err}");
        assert!(!e.sup.is_alive(9008));
        assert_eq!(e.sup.output(9008), "");
        let out = e.sup.output(9007);
        assert_eq!(out.matches("$ cmd.exe").count(), 1, "{out}");
        assert!(e.sup.kill(9007));
        e.sink.wait_exits(1);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(lock(&e.sink.rec).exits.len(), 1, "no second process was started");
        // once stopped the port is free again
        e.sup.launch(e.cmd("second", 9007, "exit 0")).unwrap();
        e.sink.wait_exits(2);
    }

    #[test]
    fn shutdown_prevents_restart() {
        let e = env();
        let (code_rx, go) = e.set_gate();
        e.sup.launch(e.cmd("closing", 9009, "exit 75")).unwrap();
        assert_eq!(code_rx.recv_timeout(WAIT).unwrap(), Some(75));
        // window closed between Blender's exit 75 and the restart decision
        e.sup.shutdown();
        go.send(()).unwrap();
        assert_eq!(e.sink.wait_exits(1), vec![(9009, Some(75))]);
        let out = e.sup.output(9009);
        assert!(!out.contains("[restarting blender...]"), "{out}");
        assert!(!e.sup.is_alive(9009));
        assert!(e.sup.launch(e.cmd("closing", 9009, "exit 0")).is_err(), "no launches after shutdown");
    }

    #[test]
    fn shutdown_terminates_running_children() {
        let e = env();
        e.sup.launch(e.cmd("a", 9010, BUSY)).unwrap();
        e.sup.launch(e.cmd("b", 9011, BUSY)).unwrap();
        let t0 = Instant::now();
        e.sup.shutdown();
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        let exits = e.sink.wait_exits(2);
        assert_eq!(exits.len(), 2);
        assert!(!e.sup.is_alive(9010) && !e.sup.is_alive(9011));
    }

    #[test]
    fn history_capped_at_20000_lines() {
        let mut h = History::new();
        for i in 0..25000 {
            h.append(&format!("line {i}\n"));
        }
        let text = h.text();
        assert_eq!(text.split('\n').count(), HISTORY_MAX_LINES);
        assert!(text.starts_with("line 5001\n"), "{}", &text[..20]);
        assert!(text.ends_with("line 24999\n"));
        h.append("par");
        h.append("tial\n");
        assert!(h.text().ends_with("line 24999\npartial\n"));

        let e = env();
        e.sup.launch(e.cmd("flood", 9012, "for /L %i in (1,1,25000) do @echo line %i")).unwrap();
        e.sink.wait_exits(1);
        let out = e.sup.output(9012);
        assert_eq!(out.split('\n').count(), HISTORY_MAX_LINES, "history capped");
        assert!(!out.starts_with("$ "), "oldest lines dropped");
        assert!(out.contains("line 25000\r\n"));
        assert!(out.ends_with("\n[blender exited code 0]\n"));
        e.sup.clear(9012);
        assert_eq!(e.sup.output(9012), "");
    }

    #[test]
    fn invalid_utf8_is_replaced() {
        let mut d = Utf8Stream::default();
        let s = "Привет".as_bytes();
        let mut got = d.decode(&s[..3]); // split inside a 2-byte sequence
        got += &d.decode(&s[3..]);
        got += &d.decode(b"a\xffb\xc3(");
        got += &d.decode(b"\xe2\x82"); // truncated at EOF
        got += &d.finish();
        assert_eq!(got, "Приветa\u{FFFD}b\u{FFFD}(\u{FFFD}");

        let e = env();
        let f = e.t.path().join("bytes.txt");
        let mut bytes = b"before \xff mid \xc3( after\n".to_vec();
        bytes.extend_from_slice("Привет\n".as_bytes());
        fs::write(&f, bytes).unwrap();
        e.sup.launch(e.cmd("bytes", 9013, &format!("type \"{}\"", f.display()))).unwrap();
        e.sink.wait_exits(1);
        let out = e.sup.output(9013);
        assert!(out.contains("before \u{FFFD} mid \u{FFFD}( after\n"), "{out}");
        assert!(out.contains("Привет\n"), "{out}");
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
            fn push(&self, _port: u16, text: &str) {
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

    #[test]
    fn path_with_spaces_and_cyrillic_is_passed_intact() {
        let e = env();
        let bat = e.bat(
            "fake blender.bat",
            "@echo off\nchcp 65001>nul\necho ARGS:%*\nset BLINKER\n",
        );
        let boot_dir = e.t.path().join("boot dir Ж");
        fs::create_dir_all(&boot_dir).unwrap();
        let boot = boot_dir.join("bootstrap.py");
        fs::write(&boot, b"#").unwrap();
        let name = "my addon 'q' & (x) Привет";
        let folder = e.addon(name, 9015);
        let blend = e.t.path().join("сцена 'x' & y.blend").to_string_lossy().into_owned();
        let resolved = display_path(&fs::canonicalize(&folder.path).unwrap());
        e.sup
            .launch(LaunchSpec {
                blender: bat.to_string_lossy().into_owned(),
                bootstrap: boot.clone(),
                folder,
                blend: Some(blend.clone()),
                args_override: None,
            })
            .unwrap();
        e.sink.wait_exits(1);
        let out = e.sup.output(9015);
        assert!(out.contains(&format!("BLINKER_ADDON_PATH={resolved}\r\n")), "{out}");
        assert!(out.contains(&format!("BLINKER_MODULE={name}\r\n")), "{out}");
        assert!(out.contains("BLINKER_LEGACY=1\r\n"), "{out}");
        assert!(out.contains("BLINKER_REPO=blinker\r\n"), "{out}");
        let args = out.lines().find(|l| l.starts_with("ARGS:")).expect("ARGS line");
        assert!(
            args.contains(&format!("--python \"{}\" \"{}\"", boot.display(), blend)),
            "{args}"
        );
        assert!(out.contains(&format!("\n  addon:  {resolved}\n")), "{out}");
        assert_eq!(
            blender_args(&boot, Some(&blend)),
            vec![OsString::from("--python"), boot.clone().into_os_string(), OsString::from(&blend)]
        );
        assert_eq!(blender_args(&boot, None).len(), 2);
    }

    #[test]
    fn bootstrap_written_matches_embedded() {
        let repo_copy = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bootstrap.py")).unwrap();
        assert_eq!(BOOTSTRAP_PY.as_bytes(), &repo_copy[..], "embedded copy is the repo-root bootstrap.py");

        let t = tempfile::tempdir().unwrap();
        let p = write_bootstrap_to(t.path()).unwrap();
        assert_eq!(p, t.path().join("blinker_bootstrap.py"));
        assert_eq!(fs::read(&p).unwrap(), BOOTSTRAP_PY.as_bytes());

        // identical content is not rewritten (a read-only file would fail a write)
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&p, perm.clone()).unwrap();
        let again = write_bootstrap_to(t.path());
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(&p, perm).unwrap();
        assert_eq!(again.unwrap(), p);

        // stale content is replaced
        fs::write(&p, b"old").unwrap();
        write_bootstrap_to(t.path()).unwrap();
        assert_eq!(fs::read(&p).unwrap(), BOOTSTRAP_PY.as_bytes());
    }
}
