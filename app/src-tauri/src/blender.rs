use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{self, PipeReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::config::Folder;

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
    /// `seq` is the port's console sequence number after `text` was appended (see
    /// `History::total`): `Supervisor::output_with_seq` returns the same counter, so a
    /// pushed text is in a snapshot exactly when its `seq` <= the snapshot's.
    fn push(&self, port: u16, text: &str, seq: u64);
    fn exited(&self, port: u16, code: Option<i32>);
}

const RESTART_EXIT_CODE: i32 = 75;
const RESTART_MARKER: &str = "blinker_restart_path";
const RESTART_BLEND: &str = "blinker_restart.blend";
const KILL_LINE: &str = "[kill] process killed\n";
const RESTARTING_LINE: &str = "\n[restarting blender...]\n\n";
/// `QProcess::waitForFinished(500)` after `terminate()` (WM_CLOSE), before the hard kill.
const TERMINATE_WAIT: Duration = Duration::from_millis(500);
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
    /// Bytes ever appended to this port's console; never decreases (not even on clear).
    total: u64,
}

impl History {
    fn new() -> Self {
        History { lines: VecDeque::from([String::new()]), total: 0 }
    }
    /// Empty the console but keep the sequence counter monotonic.
    fn clear(&mut self) {
        self.lines = VecDeque::from([String::new()]);
    }
    /// Append and return the new sequence number.
    fn append(&mut self, text: &str) -> u64 {
        self.total += text.len() as u64;
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
        self.total
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
    /// Blender has exited but `sink.exited` for it has not been delivered yet; a new
    /// launch on this port waits for it so a stale `exited` can never follow the new
    /// launch's header.
    finishing: bool,
}

impl Slot {
    fn new() -> Self {
        Slot { history: History::new(), alive: false, stop_requested: false, child: None, finishing: false }
    }
}

/// Upper bound for a launch waiting on the previous process's `exited` delivery.
const FINISHING_WAIT: Duration = Duration::from_secs(2);

struct State {
    slots: HashMap<u16, Slot>,
    shutting_down: bool,
}

struct Shared {
    state: Mutex<State>,
    /// Notified when a slot's `finishing` clears.
    finished: Condvar,
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
        let seq = lock(&self.state).slots.entry(port).or_insert_with(Slot::new).history.append(text);
        self.sink.push(port, text, seq);
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

impl Supervisor {
    pub fn new(sink: Arc<dyn OutputSink>) -> Self {
        Self::with_temp_dir(sink, std::env::temp_dir())
    }

    fn with_temp_dir(sink: Arc<dyn OutputSink>, temp_dir: PathBuf) -> Self {
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

fn has_exited(c: &ChildRef) -> bool {
    matches!(lock(c).try_wait(), Ok(Some(_)) | Err(_))
}

/// `QProcess::terminate()` + `waitForFinished(500)` for several processes at once:
/// all get WM_CLOSE first, they share one 500 ms wait, then the rest are killed.
fn close_gracefully(children: Vec<ChildRef>) {
    let running: Vec<ChildRef> = children.into_iter().filter(|c| !has_exited(c)).collect();
    if running.is_empty() {
        return;
    }
    for c in &running {
        let pid = lock(c).id();
        post_close(pid);
    }
    let deadline = Instant::now() + TERMINATE_WAIT;
    while Instant::now() < deadline && !running.iter().all(has_exited) {
        std::thread::sleep(Duration::from_millis(5));
    }
    for c in &running {
        if !has_exited(c) {
            let _ = lock(c).kill();
        }
    }
}

/// Post WM_CLOSE to every top-level window (visible or not) owned by `pid`, as
/// `QProcess::terminate()` does on Windows. Returns how many windows were found.
#[cfg(windows)]
fn post_close(pid: u32) -> usize {
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };

    struct Search {
        pid: u32,
        found: usize,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the `&mut Search` passed to EnumWindows below, which is
        // alive for the whole (synchronous) enumeration.
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut owner = 0u32;
        // SAFETY: hwnd comes from EnumWindows; owner is a valid out pointer.
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        if owner == search.pid {
            // SAFETY: posting a message to a window handle has no memory effects here.
            unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
            search.found += 1;
        }
        1
    }
    let mut search = Search { pid, found: 0 };
    // SAFETY: `each` matches WNDENUMPROC and only uses lparam as `&mut Search`.
    unsafe { EnumWindows(Some(each), &mut search as *mut Search as LPARAM) };
    search.found
}

#[cfg(not(windows))]
fn post_close(_pid: u32) -> usize {
    0
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
    Restart(ChildRef, PipeReader, u64),
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
                let seq_r = slot.history.append(RESTARTING_LINE);
                match spawn(&plan, blend.as_deref()) {
                    Ok((c, r)) => {
                        let c: ChildRef = Arc::new(Mutex::new(c));
                        slot.child = Some(c.clone());
                        Next::Restart(c, r, seq_r)
                    }
                    Err(e) => {
                        let msg = format!("[could not restart blender: {e}]\n");
                        let seq_m = slot.history.append(&msg);
                        slot.alive = false;
                        slot.finishing = true;
                        slot.child = None;
                        drop(st);
                        shared.sink.push(port, RESTARTING_LINE, seq_r);
                        shared.sink.push(port, &msg, seq_m);
                        deliver_exited(&shared, port, code);
                        return;
                    }
                }
            } else {
                slot.alive = false;
                slot.finishing = true;
                slot.child = None;
                Next::Stop
            }
        };
        match next {
            Next::Restart(c, r, seq_r) => {
                shared.sink.push(port, RESTARTING_LINE, seq_r);
                child = c;
                reader = r;
            }
            Next::Stop => {
                for name in [RESTART_MARKER, RESTART_BLEND] {
                    let _ = std::fs::remove_file(shared.temp_dir.join(name));
                }
                deliver_exited(&shared, port, code);
                return;
            }
        }
    }
}

/// Deliver `exited` (no lock held), then free the port for a new launch.
fn deliver_exited(shared: &Shared, port: u16, code: Option<i32>) {
    shared.sink.exited(port, code);
    if let Some(slot) = lock(&shared.state).slots.get_mut(&port) {
        slot.finishing = false;
    }
    shared.finished.notify_all();
}

/// The repo-root `bootstrap.py`, copied into OUT_DIR by build.rs.
const BOOTSTRAP_PY: &str = include_str!(concat!(env!("OUT_DIR"), "/bootstrap.py"));
const BOOTSTRAP_NAME: &str = "blinker_bootstrap.py";

/// Writes the embedded bootstrap to `%TEMP%\blinker_bootstrap.py` (only if it differs).
pub fn write_bootstrap() -> io::Result<PathBuf> {
    write_bootstrap_to(&std::env::temp_dir())
}

/// Where `write_bootstrap` puts the script (for error messages).
pub fn bootstrap_path() -> PathBuf {
    std::env::temp_dir().join(BOOTSTRAP_NAME)
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
        /// (port, seq, text) in push order
        pushes: Vec<(u16, u64, String)>,
        exits: Vec<(u16, Option<i32>)>,
    }

    #[derive(Default)]
    struct RecSink {
        rec: Mutex<Rec>,
        cv: Condvar,
    }

    impl OutputSink for RecSink {
        fn push(&self, port: u16, text: &str, seq: u64) {
            let mut r = lock(&self.rec);
            r.text.entry(port).or_default().push_str(text);
            r.pushes.push((port, seq, text.to_string()));
            drop(r);
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

    /// Fields drop in declaration order: the Supervisor (which kills its children)
    /// goes before the TempDir the children run from.
    struct Env {
        sup: Supervisor,
        sink: Arc<RecSink>,
        t: tempfile::TempDir,
    }

    fn env() -> Env {
        let t = tempfile::tempdir().unwrap();
        let tmp = t.path().join("temp");
        fs::create_dir_all(&tmp).unwrap();
        let sink = Arc::new(RecSink::default());
        let sup = Supervisor::with_temp_dir(sink.clone(), tmp);
        Env { sup, sink, t }
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

    /// Stand-in Blender with a real (hidden) top-level window. Runs only as a child
    /// process of `terminate_closes_window_gracefully`, which sets the env var.
    #[test]
    #[ignore = "helper process for terminate_closes_window_gracefully"]
    fn window_child_helper() {
        use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostQuitMessage,
            RegisterClassW, MSG, WM_CLOSE, WM_DESTROY, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        };
        if std::env::var_os(WINDOW_CHILD_ENV).is_none() {
            return;
        }
        unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            match msg {
                WM_CLOSE => {
                    println!("got WM_CLOSE");
                    unsafe { DestroyWindow(hwnd) };
                    0
                }
                WM_DESTROY => {
                    unsafe { PostQuitMessage(0) };
                    0
                }
                _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
            }
        }
        let class: Vec<u16> = "BlinkerTestWindow\0".encode_utf16().collect();
        unsafe {
            let hinst = GetModuleHandleW(std::ptr::null());
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = hinst;
            wc.lpszClassName = class.as_ptr();
            // BLINKER_TEST_FORCE_NO_WINDOW: exercise the skip path by hand.
            if RegisterClassW(&wc) == 0 || std::env::var_os("BLINKER_TEST_FORCE_NO_WINDOW").is_some() {
                println!("{WINDOW_UNAVAILABLE}");
                std::process::exit(43);
            }
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPEDWINDOW, // not WS_VISIBLE: hidden, still enumerated
                0,
                0,
                100,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinst,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                println!("{WINDOW_UNAVAILABLE}");
                std::process::exit(43);
            }
            println!("window-ready");
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                DispatchMessageW(&msg);
            }
        }
        println!("closed gracefully");
        std::process::exit(42);
    }

    const WINDOW_CHILD_ENV: &str = "BLINKER_TEST_WINDOW_CHILD";
    /// Printed by the helper when it cannot create a window (no usable desktop/window
    /// station, e.g. some service-hosted CI runners): the windowed tests then skip.
    const WINDOW_UNAVAILABLE: &str = "window-unavailable";

    /// Launch the test binary itself as a windowed stand-in Blender and wait for its
    /// window. False (after printing a skip note) when no window can be created here.
    fn launch_windowed(e: &Env, port: u16) -> bool {
        // Inherited by children; cmd.exe stand-ins in other tests ignore it.
        std::env::set_var(WINDOW_CHILD_ENV, "1");
        let mut spec = e.cmd(&format!("windowed{port}"), port, "");
        spec.blender = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        spec.args_override = Some(
            ["blender::tests::window_child_helper", "--exact", "--ignored", "--nocapture", "--test-threads=1"]
                .map(String::from)
                .to_vec(),
        );
        e.sup.launch(spec).unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            let out = e.sup.output(port);
            if out.contains("window-ready") {
                return true;
            }
            if out.contains(WINDOW_UNAVAILABLE) {
                eprintln!("SKIP: no window could be created (no interactive desktop?)");
                return false;
            }
            assert!(Instant::now() < deadline, "helper never opened its window: {out}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn shutdown_closes_windows_gracefully() {
        let e = env();
        if !launch_windowed(&e, 9019) {
            return;
        }
        e.sup.launch(e.cmd("stubborn2", 9020, BUSY)).unwrap();
        let t0 = Instant::now();
        e.sup.shutdown();
        let took = t0.elapsed();
        // One shared 500 ms wait, not one per child; generous ceiling for loaded CI runners.
        assert!(took >= TERMINATE_WAIT && took < Duration::from_secs(10), "{took:?}");
        let mut exits = e.sink.wait_exits(2);
        exits.sort();
        assert_eq!(exits, vec![(9019, Some(42)), (9020, Some(1))], "windowed one closed, stubborn one killed");
        assert!(e.sup.output(9019).contains("closed gracefully"));
    }

    #[test]
    fn terminate_closes_window_gracefully() {
        let e = env();
        if !launch_windowed(&e, 9016) {
            return;
        }
        let t0 = Instant::now();
        assert!(e.sup.terminate(9016));
        // Normally well under the 500 ms wait; the ceiling only guards a loaded CI runner.
        // That it closed gracefully (not killed) is the exit code 42 below.
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        // 42 = left its message loop normally; a hard kill would give 1
        assert_eq!(e.sink.wait_exits(1), vec![(9016, Some(42))]);
        let out = e.sup.output(9016);
        assert!(out.contains("got WM_CLOSE") && out.contains("closed gracefully"), "{out}");
        assert!(!e.sup.terminate(9016), "nothing left to terminate");
    }

    #[test]
    fn terminate_kills_child_that_ignores_close() {
        let e = env();
        assert!(!e.sup.terminate(9017));
        e.sup.launch(e.cmd("stubborn", 9017, BUSY)).unwrap();
        let t0 = Instant::now();
        assert!(e.sup.terminate(9017));
        let took = t0.elapsed();
        assert!(took >= TERMINATE_WAIT, "waited for a graceful exit first: {took:?}");
        assert!(took < Duration::from_secs(10), "{took:?}");
        assert_eq!(e.sink.wait_exits(1), vec![(9017, Some(1))], "hard-killed after the wait");
        let out = e.sup.output(9017);
        assert!(!out.contains("[restarting"), "{out}");
        assert!(!out.contains("[kill]"), "terminate is silent, like Python: {out}");
        assert!(!e.sup.is_alive(9017));
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
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
        let exits = e.sink.wait_exits(2);
        assert_eq!(exits.len(), 2);
        assert!(!e.sup.is_alive(9010) && !e.sup.is_alive(9011));
    }

    /// Rebuild the console from a snapshot plus the pushes whose seq is past it.
    fn merge(snapshot: &(String, u64), pushes: &[(u16, u64, String)], port: u16) -> String {
        let mut s = snapshot.0.clone();
        for (_, _, t) in pushes.iter().filter(|(p, q, _)| *p == port && *q > snapshot.1) {
            s.push_str(t);
        }
        s
    }

    #[test]
    fn output_seq_is_monotonic_and_snapshot_plus_later_pushes_rebuilds_history() {
        let e = env();
        let p = 9020;
        assert_eq!(e.sup.output_with_seq(p), (String::new(), 0));
        e.sup.append(p, "a
");
        e.sup.append(p, "bc");
        let snap = e.sup.output_with_seq(p);
        assert_eq!(snap, ("a
bc".to_string(), 4));
        e.sup.append(p, "d
");
        e.sup.append(9021, "other port");
        e.sup.append(p, "e");
        let pushes = lock(&e.sink.rec).pushes.clone();
        let seqs: Vec<u64> = pushes.iter().filter(|x| x.0 == p).map(|x| x.1).collect();
        assert_eq!(seqs, vec![2, 4, 6, 7]);
        assert_eq!(pushes.iter().find(|x| x.0 == 9021).unwrap().1, 10);
        assert_eq!(merge(&snap, &pushes, p), e.sup.output(p));
        // clear empties the text but never moves the counter back
        assert_eq!(e.sup.clear(p), 7);
        assert_eq!(e.sup.output_with_seq(p), (String::new(), 7));
        e.sup.append(p, "x");
        assert_eq!(e.sup.output_with_seq(p), ("x".to_string(), 8));
        assert_eq!(lock(&e.sink.rec).pushes.last().unwrap().1, 8);
    }

    #[test]
    fn output_seq_snapshot_mid_stream_rebuilds_process_output() {
        let e = env();
        let p = 9022;
        e.sup.launch(e.cmd("seqflood", p, "for /L %i in (1,1,3000) do @echo line %i")).unwrap();
        // snapshot while output is flowing
        std::thread::sleep(Duration::from_millis(30));
        let snap = e.sup.output_with_seq(p);
        e.sink.wait_exits(1);
        let pushes = lock(&e.sink.rec).pushes.clone();
        let full = e.sup.output(p);
        assert!(full.contains("line 3000"), "{}", &full[full.len().saturating_sub(200)..]);
        assert_eq!(merge(&snap, &pushes, p), full);
        let seqs: Vec<u64> = pushes.iter().filter(|x| x.0 == p).map(|x| x.1).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "seq not increasing: {seqs:?}");
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
