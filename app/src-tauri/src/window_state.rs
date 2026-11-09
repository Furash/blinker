//! Window position/size memory: the main window reopens where it was closed.
//!
//! `window_state.json` lives next to the config (`<config dir>\window_state.json`, so
//! `BLINKER_CONFIG_DIR` runs never touch `%APPDATA%`). It holds the NORMAL (restored)
//! rect of the main window in physical pixels of the virtual desktop (outer top-left as
//! `outer_position`/`set_position` use it, inner size as `inner_size`/`set_size` use it),
//! the maximized flag and the scale factor of the monitor at save time.
//! Delete the file to get the default placement back.
//!
//! - Restore: `restore` in `setup()`, while the window is still hidden (the frontend
//!   shows it after first paint), so it never flashes at the default place. `fit` decides
//!   whether the saved rect is still usable on the current monitors.
//! - Save: on `CloseRequested` (synchronously, before the Blender shutdown), when the
//!   updater quits the app (`flush`), and ~500 ms after the last move/resize (a single
//!   idle worker thread) so a crash or kill keeps the last position.
//!
//! What is saved: only rects the window really had while it was normal (not minimised,
//! maximized or fullscreen): a maximized window keeps its last normal rect plus
//! `maximized: true`; a minimised window is never recorded (Windows parks it at
//! -32000,-32000). Moves/resizes before the window was first shown (our own restore,
//! the default placement) update the tracker but do not arm the debounced save; when
//! `fit` clamped a saved rect, the clamped rect is what the user sees, so it is written
//! on the next save (close or a later move). A rejected file is replaced by the rect the
//! window actually has at the next save.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "window_state.json";
/// Minimum size when tauri.conf.json sets none (it sets none today).
pub const DEFAULT_MIN: (u32, u32) = (400, 300);
/// Delay after the last move/resize before the state is written.
pub const DEBOUNCE: Duration = Duration::from_millis(500);
/// How much of the title-bar strip must be on a monitor for a saved rect to be restored.
const MIN_VISIBLE_W: i64 = 120;
const MIN_VISIBLE_H: i64 = 60;
/// Coordinates/sizes beyond this are nonsense (and keep the i64 arithmetic far from overflow).
const ABSURD: i64 = 1_000_000;
/// Windows reports a minimised window's position as -32000,-32000.
const MINIMIZED_SENTINEL: i32 = -32000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
    /// Scale factor of the monitor the window was on when saved (0 = unknown).
    #[serde(default)]
    pub scale_factor: f64,
}

/// A monitor's work area (physical px) and scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

/// `<config dir>\window_state.json` for the given config file path.
pub fn state_path(config_path: &Path) -> PathBuf {
    config_path.parent().map(|d| d.join(FILE_NAME)).unwrap_or_else(|| PathBuf::from(FILE_NAME))
}

/// Tolerant loader: missing, empty, garbage, wrong types -> None. A UTF-8 BOM is skipped,
/// unknown fields are ignored.
pub fn load(path: &Path) -> Option<WindowState> {
    let bytes = std::fs::read(path).ok()?;
    let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    serde_json::from_slice(body).ok()
}

/// Pretty JSON, written atomically (temp file + rename, like `config::save`).
pub fn save(path: &Path, state: &WindowState) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, json)?;
    crate::config::rename_with_retry(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A rectangle in i64 (no overflow for any i32/u32 input).
#[derive(Clone, Copy)]
struct Area {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    scale: f64,
}

impl Area {
    fn right(&self) -> i64 {
        self.x + self.w
    }
    fn bottom(&self) -> i64 {
        self.y + self.h
    }
    /// Width and height of the intersection with (x, y, w, h).
    fn overlap(&self, x: i64, y: i64, w: i64, h: i64) -> (i64, i64) {
        let iw = (self.right().min(x + w) - self.x.max(x)).max(0);
        let ih = (self.bottom().min(y + h) - self.y.max(y)).max(0);
        (iw, ih)
    }
}

fn sane_rect(x: i64, y: i64, w: i64, h: i64) -> bool {
    x.abs() <= ABSURD && y.abs() <= ABSURD && (1..=ABSURD).contains(&w) && (1..=ABSURD).contains(&h)
}

fn clean_scale(s: f64) -> f64 {
    if s.is_finite() && s > 0.0 {
        s
    } else {
        0.0
    }
}

/// Up to this much of the title strip may be off-screen vertically (the invisible DWM
/// frame and windows nudged a few px above the top edge).
const FRAME_SLACK: i64 = 16;

/// Can `state` be restored on these monitors (work areas)? None = use the default
/// placement. Rejected: minimised sentinel, empty or absurd values, no monitor showing at
/// least 120 x 60 px of the title-bar strip (top 60 px of the window; 16 px of slack for
/// the invisible frame). Otherwise the size is converted to the current scale factor and
/// clamped to [min, work area] of the monitor the window mostly sits on, and the
/// position nudged so the title bar is on that monitor (fully inside it when the size
/// had to be clamped).
pub fn fit(state: &WindowState, monitors: &[MonitorRect], min: (u32, u32)) -> Option<WindowState> {
    if state.x <= MINIMIZED_SENTINEL && state.y <= MINIMIZED_SENTINEL {
        return None;
    }
    let (x, y) = (i64::from(state.x), i64::from(state.y));
    if !sane_rect(x, y, i64::from(state.width), i64::from(state.height)) {
        return None;
    }
    let mons: Vec<Area> = monitors
        .iter()
        .map(|m| Area { x: m.x.into(), y: m.y.into(), w: m.width.into(), h: m.height.into(), scale: m.scale_factor })
        .filter(|a| sane_rect(a.x, a.y, a.w, a.h))
        .collect();
    let area_of = |a: &Area| {
        let (iw, ih) = a.overlap(x, y, state.width.into(), state.height.into());
        iw * ih
    };
    let home = *mons.iter().max_by_key(|a| area_of(a)).filter(|a| area_of(a) > 0)?;
    let (w, h) = scale_size(state.width, state.height, state.scale_factor, home.scale);
    let (w, h) = (i64::from(w).min(ABSURD), i64::from(h).min(ABSURD));
    // enough of the title bar on some monitor?
    let strip_h = h.min(MIN_VISIBLE_H);
    let need_w = w.min(MIN_VISIBLE_W);
    let need_h = (strip_h - FRAME_SLACK).max(1);
    if !mons.iter().any(|a| {
        let (iw, ih) = a.overlap(x, y, w, strip_h);
        iw >= need_w && ih >= need_h
    }) {
        return None;
    }
    // size: never larger than the work area, never below the minimum
    let cw = w.max(min.0.into()).min(home.w);
    let ch = h.max(min.1.into()).min(home.h);
    let (mut nx, mut ny) = (x, y);
    if cw < w {
        nx = nx.clamp(home.x, home.right() - cw);
    }
    if ch < h {
        ny = ny.clamp(home.y, home.bottom() - ch);
    }
    // title bar on the home monitor: not above its top, at least a strip above its bottom
    ny = ny.clamp(home.y, (home.bottom() - ch.min(MIN_VISIBLE_H)).max(home.y));
    let vis_w = cw.min(MIN_VISIBLE_W);
    nx = nx.clamp(home.x - cw + vis_w, (home.right() - vis_w).max(home.x - cw + vis_w));
    Some(WindowState {
        x: i32::try_from(nx).ok()?,
        y: i32::try_from(ny).ok()?,
        width: u32::try_from(cw).ok()?,
        height: u32::try_from(ch).ok()?,
        maximized: state.maximized,
        scale_factor: clean_scale(home.scale),
    })
}

/// Size converted from the saved monitor's scale to the current one (`w * now / saved`);
/// unchanged when either scale is unknown/invalid. Saturates instead of overflowing.
pub fn scale_size(w: u32, h: u32, saved: f64, now: f64) -> (u32, u32) {
    let (saved, now) = (clean_scale(saved), clean_scale(now));
    if saved == 0.0 || now == 0.0 || saved == now {
        return (w, h);
    }
    let f = now / saved;
    let conv = |v: u32| (f64::from(v) * f).round() as u32; // `as` saturates
    (conv(w), conv(h))
}

/// What the window is doing when a move/resize is observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Maximized,
    Minimized,
    Fullscreen,
}

/// The window's mode from its flags (a minimised maximized window is minimised).
pub fn mode_from(minimized: bool, fullscreen: bool, maximized: bool) -> Mode {
    if minimized {
        Mode::Minimized
    } else if fullscreen {
        Mode::Fullscreen
    } else if maximized {
        Mode::Maximized
    } else {
        Mode::Normal
    }
}

/// Remembers the last NORMAL rect and whether the window is maximized.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tracker {
    normal: Option<(i32, i32, u32, u32)>,
    maximized: bool,
    scale_factor: f64,
}

impl Tracker {
    /// Records an observation; returns true when the snapshot changed.
    pub fn observe(&mut self, mode: Mode, pos: (i32, i32), size: (u32, u32), scale_factor: f64) -> bool {
        let before = self.clone();
        match mode {
            Mode::Normal => {
                // a state change the flags do not show yet (minimise in progress): ignore
                let sentinel = pos.0 <= MINIMIZED_SENTINEL && pos.1 <= MINIMIZED_SENTINEL;
                if !sentinel && sane_rect(pos.0.into(), pos.1.into(), size.0.into(), size.1.into()) {
                    self.normal = Some((pos.0, pos.1, size.0, size.1));
                    self.maximized = false;
                    self.scale_factor = clean_scale(scale_factor);
                }
            }
            Mode::Maximized => self.maximized = true,
            Mode::Minimized | Mode::Fullscreen => {}
        }
        *self != before
    }

    /// The state to save: the last normal rect + maximized flag (None before any normal rect).
    pub fn snapshot(&self) -> Option<WindowState> {
        self.normal.map(|(x, y, width, height)| WindowState {
            x,
            y,
            width,
            height,
            maximized: self.maximized,
            scale_factor: self.scale_factor,
        })
    }
}

/// Debounce decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flush {
    /// Nothing pending.
    Idle,
    /// Pending, write after this long.
    Wait(Duration),
    /// Write now.
    Now,
}

/// `last_change`: the last armed move/resize not yet written (None = nothing pending).
pub fn should_flush(last_change: Option<Instant>, now: Instant, delay: Duration) -> Flush {
    let Some(t) = last_change else { return Flush::Idle };
    let quiet = now.saturating_duration_since(t);
    if quiet >= delay {
        Flush::Now
    } else {
        Flush::Wait(delay - quiet)
    }
}

/// Tracker + debounced writer, managed by Tauri (one per process, main window only).
pub struct WindowMemory {
    path: PathBuf,
    shared: Arc<Shared>,
}

struct Shared {
    inner: Mutex<Inner>,
    cv: Condvar,
    /// What is on disk (as far as we know); also serialises the writes.
    written: Mutex<Option<WindowState>>,
}

struct Inner {
    tracker: Tracker,
    last_change: Option<Instant>,
    /// Set after the close-time save: later events (window teardown) are ignored.
    frozen: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl WindowMemory {
    /// `on_disk`: the state loaded at startup (so an unchanged state is not rewritten).
    pub fn new(path: PathBuf, tracker: Tracker, on_disk: Option<WindowState>) -> Self {
        let shared = Shared {
            inner: Mutex::new(Inner { tracker, last_change: None, frozen: false }),
            cv: Condvar::new(),
            written: Mutex::new(on_disk),
        };
        WindowMemory { path, shared: Arc::new(shared) }
    }

    /// Records a move/resize; `arm` starts (or restarts) the debounced save.
    pub fn observe(&self, mode: Mode, pos: (i32, i32), size: (u32, u32), scale_factor: f64, arm: bool) {
        let mut g = lock(&self.shared.inner);
        if g.frozen {
            return;
        }
        if g.tracker.observe(mode, pos, size, scale_factor) && arm {
            g.last_change = Some(Instant::now());
            self.shared.cv.notify_one();
        }
    }

    /// Writes the current state now if it differs from the file. Ok(true) = written.
    pub fn flush(&self) -> std::io::Result<bool> {
        flush_shared(&self.path, &self.shared)
    }

    /// Ignore everything from now on (called after the close-time save).
    pub fn freeze(&self) {
        let mut g = lock(&self.shared.inner);
        g.frozen = true;
        g.last_change = None;
    }

    /// Starts the idle writer thread: it sleeps on a condvar until a move/resize arms it,
    /// then writes once `delay` has passed without another change (no polling).
    pub fn start_worker(&self, delay: Duration) {
        let (path, shared) = (self.path.clone(), self.shared.clone());
        let _ = std::thread::Builder::new().name("window-state".into()).spawn(move || {
            let mut g = lock(&shared.inner);
            loop {
                match should_flush(g.last_change, Instant::now(), delay) {
                    Flush::Idle => g = shared.cv.wait(g).unwrap_or_else(|e| e.into_inner()),
                    Flush::Wait(d) => g = shared.cv.wait_timeout(g, d).unwrap_or_else(|e| e.into_inner()).0,
                    Flush::Now => {
                        g.last_change = None;
                        drop(g); // never hold the tracker (UI thread) while writing
                        let _ = flush_shared(&path, &shared);
                        g = lock(&shared.inner);
                    }
                }
            }
        });
    }
}

/// Snapshot under the write lock (so a slower, older write cannot overwrite a newer one),
/// then write when it differs from what is on disk.
fn flush_shared(path: &Path, shared: &Shared) -> std::io::Result<bool> {
    let mut written = lock(&shared.written);
    let Some(snap) = lock(&shared.inner).tracker.snapshot() else { return Ok(false) };
    if written.as_ref() == Some(&snap) {
        return Ok(false);
    }
    save(path, &snap)?;
    *written = Some(snap);
    Ok(true)
}

// ----- Tauri glue (main window only; not unit-tested, verified live) -----

/// Label of the window whose state is remembered.
pub const MAIN: &str = "main";

/// Startup (in `setup()`, window still hidden): load the state file, apply it when `fit`
/// accepts it, and return the memory seeded with what the window now has. Never fails:
/// without a usable file the window keeps the default placement from tauri.conf.json.
pub fn restore(w: &tauri::WebviewWindow, path: PathBuf) -> WindowMemory {
    let on_disk = load(&path);
    let monitors: Vec<MonitorRect> = w
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let a = m.work_area();
            MonitorRect { x: a.position.x, y: a.position.y, width: a.size.width, height: a.size.height, scale_factor: m.scale_factor() }
        })
        .collect();
    let target = on_disk.as_ref().and_then(|s| fit(s, &monitors, DEFAULT_MIN));
    let mut maximized = false;
    if let Some(t) = &target {
        // Position first: moving to a monitor with another DPI makes the window rescale
        // itself; the physical size set afterwards is final. Maximize last, so the
        // window maximizes on (and restores to) the right monitor. A hidden window only
        // gets the WS_MAXIMIZE style here; it is shown maximized by the frontend's show().
        let _ = w.set_position(tauri::PhysicalPosition::new(t.x, t.y));
        let _ = w.set_size(tauri::PhysicalSize::new(t.width, t.height));
        if t.maximized {
            #[cfg(windows)]
            pin_restore_rect(w);
            maximized = w.maximize().is_ok();
        }
    }
    // Seed with what the window really has now (default placement when nothing applied),
    // so a close without any move writes that rect.
    let mut tracker = Tracker::default();
    if let (Ok(p), Ok(s)) = (w.outer_position(), w.inner_size()) {
        tracker.observe(Mode::Normal, (p.x, p.y), (s.width, s.height), w.scale_factor().unwrap_or(0.0));
    }
    if maximized {
        tracker.observe(Mode::Maximized, (0, 0), (0, 0), 0.0);
    }
    WindowMemory::new(path, tracker, on_disk)
}

/// tao maximizes a hidden window by setting the WS_MAXIMIZE style only, and Windows then
/// does not always take the current rect as the restore rect: measured live, about half
/// of the starts un-maximized to the creation (CW_USEDEFAULT) rect. Set it explicitly
/// (WINDOWPLACEMENT with SW_HIDE: the window stays hidden). `rcNormalPosition` is in
/// workspace coordinates (shifted by a taskbar at the top/left of the primary monitor),
/// so correct by the offset observed after the first try.
#[cfg(windows)]
fn pin_restore_rect(w: &tauri::WebviewWindow) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, GetWindowRect, SetWindowPlacement, SW_HIDE, WINDOWPLACEMENT};
    let Ok(h) = w.hwnd() else { return };
    let h = h.0 as windows_sys::Win32::Foundation::HWND;
    let zero = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    let (mut want, mut got) = (zero, zero);
    // SAFETY: Win32 calls on our own window handle with valid, initialised out-structs.
    unsafe {
        if GetWindowRect(h, &mut want) == 0 {
            return;
        }
        let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
        wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
        if GetWindowPlacement(h, &mut wp) == 0 {
            return;
        }
        wp.flags = 0;
        wp.showCmd = SW_HIDE as u32;
        let mut guess = want;
        for _ in 0..2 {
            wp.rcNormalPosition = guess;
            if SetWindowPlacement(h, &wp) == 0 || GetWindowRect(h, &mut got) == 0 {
                return;
            }
            let (dx, dy) = (got.left - want.left, got.top - want.top);
            if dx == 0 && dy == 0 {
                return;
            }
            guess = RECT { left: guess.left - dx, top: guess.top - dy, right: guess.right - dx, bottom: guess.bottom - dy };
        }
    }
}

/// `on_window_event` for the main window: track moves/resizes (arming the debounced
/// save only once the window is visible, i.e. after the frontend showed it), and save
/// synchronously on close. Never panics; a failed save is ignored.
pub fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    use tauri::Manager;
    if window.label() != MAIN {
        return;
    }
    let Some(mem) = window.try_state::<WindowMemory>() else { return };
    match event {
        tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
            observe_window(window, &mem, window.is_visible().unwrap_or(false));
        }
        tauri::WindowEvent::CloseRequested { .. } => {
            // The window is still intact here (the app never prevents the close).
            observe_window(window, &mem, false);
            let _ = mem.flush();
            mem.freeze();
        }
        _ => {}
    }
}

/// The updater quits with `app.exit(0)`, which emits no `CloseRequested`: save here.
pub fn save_on_exit<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::Manager;
    if let Some(mem) = app.try_state::<WindowMemory>() {
        let _ = mem.flush();
        mem.freeze();
    }
}

fn observe_window(w: &tauri::Window, mem: &WindowMemory, arm: bool) {
    let (Ok(p), Ok(s)) = (w.outer_position(), w.inner_size()) else { return };
    mem.observe(window_mode(w), (p.x, p.y), (s.width, s.height), w.scale_factor().unwrap_or(0.0), arm);
}

/// The live flags. On Windows IsIconic/IsZoomed: tao updates its own maximized flag only
/// on WM_SIZE, after the `Moved` of the same maximize/restore, so it can lag one event.
fn window_mode(w: &tauri::Window) -> Mode {
    let fullscreen = w.is_fullscreen().unwrap_or(false);
    #[cfg(windows)]
    if let Ok(h) = w.hwnd() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsZoomed};
        let h = h.0 as windows_sys::Win32::Foundation::HWND;
        // SAFETY: plain queries on the window's own handle; a stale handle just returns 0.
        let (min, max) = unsafe { (IsIconic(h) != 0, IsZoomed(h) != 0) };
        return mode_from(min, fullscreen, max);
    }
    mode_from(w.is_minimized().unwrap_or(false), fullscreen, w.is_maximized().unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(x: i32, y: i32, w: u32, h: u32) -> WindowState {
        WindowState { x, y, width: w, height: h, maximized: false, scale_factor: 1.0 }
    }
    fn mon(x: i32, y: i32, w: u32, h: u32) -> MonitorRect {
        MonitorRect { x, y, width: w, height: h, scale_factor: 1.0 }
    }
    fn rect(s: &WindowState) -> (i32, i32, u32, u32) {
        (s.x, s.y, s.width, s.height)
    }
    const MIN: (u32, u32) = DEFAULT_MIN;

    // ----- fit -----

    #[test]
    fn fit_keeps_a_rect_that_is_on_screen() {
        let m = [mon(0, 0, 1920, 1040)];
        assert_eq!(fit(&st(100, 80, 1100, 820), &m, MIN), Some(st(100, 80, 1100, 820)));
        let mut max = st(100, 80, 1100, 820);
        max.maximized = true;
        assert_eq!(fit(&max, &m, MIN), Some(max.clone()), "maximized flag kept");
    }

    #[test]
    fn fit_rejects_a_window_on_an_unplugged_monitor() {
        // saved on a second monitor at x=2500; only one 1920 monitor left
        assert_eq!(fit(&st(2500, 100, 1100, 820), &[mon(0, 0, 1920, 1040)], MIN), None);
        // left monitor unplugged
        assert_eq!(fit(&st(-1800, 100, 1100, 820), &[mon(0, 0, 1920, 1040)], MIN), None);
    }

    #[test]
    fn fit_rejects_when_too_little_of_the_title_bar_is_visible() {
        let m = [mon(0, 0, 1920, 1040)];
        // only 70 px of the title bar on the monitor horizontally
        assert_eq!(fit(&st(1850, 100, 1100, 820), &m, MIN), None);
        // title bar below the bottom of the work area
        assert_eq!(fit(&st(100, 1000, 1100, 820), &m, MIN), None);
        // title bar above the top (only the lower part of the window visible)
        assert_eq!(fit(&st(100, -500, 1100, 820), &m, MIN), None);
        // 200 px visible: kept
        assert_eq!(fit(&st(1720, 100, 1100, 820), &m, MIN).map(|s| rect(&s)), Some((1720, 100, 1100, 820)));
    }

    #[test]
    fn fit_tolerates_the_invisible_frame_at_the_edges() {
        let m = [mon(0, 0, 1920, 1040)];
        // snapped to the left edge: the outer rect starts at -11 (invisible resize border)
        assert_eq!(fit(&st(-11, 0, 1100, 820), &m, MIN).map(|s| rect(&s)), Some((-11, 0, 1100, 820)));
        // a few px above the top: kept, nudged down so the title bar can be grabbed
        assert_eq!(fit(&st(100, -10, 1100, 820), &m, MIN).map(|s| rect(&s)), Some((100, 0, 1100, 820)));
    }

    #[test]
    fn fit_accepts_negative_coordinates_on_a_monitor_left_of_the_primary() {
        let m = [mon(-2560, 0, 2560, 1392), mon(0, 0, 2560, 1392)];
        assert_eq!(fit(&st(-2000, 50, 1100, 820), &m, MIN), Some(st(-2000, 50, 1100, 820)));
        // a monitor above the primary
        let m = [mon(0, -1080, 1920, 1040), mon(0, 0, 1920, 1040)];
        assert_eq!(fit(&st(10, -1000, 800, 600), &m, MIN), Some(st(10, -1000, 800, 600)));
    }

    #[test]
    fn fit_clamps_a_window_bigger_than_the_screen() {
        let m = [mon(0, 0, 1920, 1040)];
        let f = fit(&st(0, 0, 3000, 2000), &m, MIN).unwrap();
        assert_eq!(rect(&f), (0, 0, 1920, 1040));
        // after lowering the resolution: size clamped and moved fully inside
        let f = fit(&st(500, 300, 2400, 1300), &[mon(0, 0, 1280, 680)], MIN).unwrap();
        assert_eq!(rect(&f), (0, 0, 1280, 680));
        // clamped to the monitor it mostly sits on (the smaller right one)
        let m = [mon(0, 0, 2560, 1392), mon(2560, 0, 1280, 984)];
        let f = fit(&st(2600, 10, 2000, 1200), &m, MIN).unwrap();
        assert_eq!(rect(&f), (2560, 0, 1280, 984));
    }

    #[test]
    fn fit_keeps_the_title_bar_on_screen_after_a_resolution_change() {
        // was on a 2560x1440 monitor at y=600; now 1920x1040: title must stay above the bottom
        let m = [mon(0, 0, 1920, 1040)];
        let f = fit(&st(700, 900, 1100, 820), &m, MIN).unwrap();
        assert!(f.y >= 0 && i64::from(f.y) + MIN_VISIBLE_H <= 1040, "{f:?}");
        assert_eq!((f.width, f.height), (1100, 820));
    }

    #[test]
    fn fit_raises_tiny_sizes_to_the_minimum() {
        let m = [mon(0, 0, 1920, 1040)];
        assert_eq!(fit(&st(100, 100, 50, 20), &m, MIN).map(|s| (s.width, s.height)), Some((400, 300)));
        assert_eq!(fit(&st(100, 100, 50, 20), &m, (640, 480)).map(|s| (s.width, s.height)), Some((640, 480)));
        // a work area smaller than the minimum: never larger than the work area
        assert_eq!(fit(&st(0, 0, 800, 600), &[mon(0, 0, 350, 250)], MIN).map(|s| rect(&s)), Some((0, 0, 350, 250)));
    }

    #[test]
    fn fit_rejects_zero_sizes_the_minimised_sentinel_and_absurd_values() {
        let m = [mon(0, 0, 1920, 1040)];
        assert_eq!(fit(&st(100, 100, 0, 820), &m, MIN), None);
        assert_eq!(fit(&st(100, 100, 1100, 0), &m, MIN), None);
        assert_eq!(fit(&st(-32000, -32000, 160, 28), &m, MIN), None);
        assert_eq!(fit(&st(i32::MAX, i32::MAX, u32::MAX, u32::MAX), &m, MIN), None);
        assert_eq!(fit(&st(i32::MIN, i32::MIN, u32::MAX, u32::MAX), &m, MIN), None);
        assert_eq!(fit(&st(100, 100, u32::MAX, 820), &m, MIN), None);
        assert_eq!(fit(&st(100, 100, 1100, 820), &[], MIN), None, "no monitors");
        let mut nan = st(100, 100, 1100, 820);
        nan.scale_factor = f64::NAN;
        assert_eq!(fit(&nan, &m, MIN).map(|s| rect(&s)), Some((100, 100, 1100, 820)), "bad scale = unknown");
        // absurd monitor values do not overflow either
        assert_eq!(fit(&st(100, 100, 1100, 820), &[mon(i32::MAX, i32::MAX, u32::MAX, u32::MAX)], MIN), None);
    }

    #[test]
    fn fit_converts_the_size_when_the_scale_factor_changed() {
        let mut s = st(100, 100, 1650, 1230);
        s.scale_factor = 1.5;
        let f = fit(&s, &[mon(0, 0, 2560, 1392)], MIN).unwrap();
        assert_eq!((f.width, f.height), (1100, 820));
        assert_eq!(f.scale_factor, 1.0);
        // unknown saved scale: no conversion
        s.scale_factor = 0.0;
        assert_eq!(fit(&s, &[mon(0, 0, 2560, 1392)], MIN).map(|f| (f.width, f.height)), Some((1650, 1230)));
    }

    #[test]
    fn scale_size_is_proportional_and_overflow_safe() {
        assert_eq!(scale_size(1100, 820, 1.0, 1.5), (1650, 1230));
        assert_eq!(scale_size(1650, 1230, 1.5, 1.0), (1100, 820));
        assert_eq!(scale_size(1100, 820, 1.25, 1.25), (1100, 820));
        assert_eq!(scale_size(1100, 820, 0.0, 1.5), (1100, 820), "unknown");
        assert_eq!(scale_size(1100, 820, f64::NAN, 1.5), (1100, 820));
        assert_eq!(scale_size(1100, 820, 1.0, f64::INFINITY), (1100, 820));
        assert_eq!(scale_size(u32::MAX, 10, 1.0, 4.0), (u32::MAX, 40), "saturates");
    }

    // ----- file -----

    #[test]
    fn state_file_is_next_to_the_config() {
        let p = state_path(Path::new("D:\\cfg\\blinker_ui_config.json"));
        assert_eq!(p, Path::new("D:\\cfg").join("window_state.json"));
    }

    #[test]
    fn save_and_load_round_trip_as_pretty_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join(FILE_NAME);
        let s = WindowState { x: -1500, y: 40, width: 1200, height: 900, maximized: true, scale_factor: 1.5 };
        save(&p, &s).unwrap();
        assert_eq!(load(&p), Some(s.clone()));
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("\n  \"x\": -1500"), "{text}");
        assert!(!dir.path().join("sub").join("window_state.json.tmp").exists(), "temp file renamed");
        // overwrite
        let s2 = st(1, 2, 500, 400);
        save(&p, &s2).unwrap();
        assert_eq!(load(&p), Some(s2));
    }

    #[test]
    fn load_is_tolerant() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        assert_eq!(load(&p), None, "missing");
        for bad in [
            &b""[..],
            b"   ",
            b"garbage{",
            b"[1,2,3]",
            b"null",
            b"{\"x\": \"10\", \"y\": 0, \"width\": 100, \"height\": 100}",
            b"{\"x\": 10, \"y\": 0, \"width\": -100, \"height\": 100}",
            b"{\"x\": 10, \"y\": 0, \"width\": 100}",
            b"{\"x\": 1e40, \"y\": 0, \"width\": 100, \"height\": 100}",
            b"\xff\xfe\x00garbage",
        ] {
            std::fs::write(&p, bad).unwrap();
            assert_eq!(load(&p), None, "{:?}", String::from_utf8_lossy(bad));
        }
        // BOM and unknown fields; missing optional fields default
        std::fs::write(&p, b"\xef\xbb\xbf{\"x\": 10, \"y\": 20, \"width\": 300, \"height\": 200, \"future\": [1]}").unwrap();
        assert_eq!(load(&p), Some(WindowState { x: 10, y: 20, width: 300, height: 200, maximized: false, scale_factor: 0.0 }));
        // a directory where the file should be
        std::fs::remove_file(&p).unwrap();
        std::fs::create_dir(&p).unwrap();
        assert_eq!(load(&p), None);
    }

    #[test]
    fn save_failure_leaves_the_old_file_and_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        // the target is a non-empty directory: rename fails
        let p = dir.path().join(FILE_NAME);
        std::fs::create_dir(&p).unwrap();
        std::fs::write(p.join("keep"), b"x").unwrap();
        assert!(save(&p, &st(1, 2, 500, 400)).is_err());
        assert!(!dir.path().join("window_state.json.tmp").exists());
        assert!(p.join("keep").exists());
    }

    // ----- tracker -----

    #[test]
    fn tracker_keeps_the_last_normal_rect_while_maximized() {
        let mut t = Tracker::default();
        assert_eq!(t.snapshot(), None);
        assert!(t.observe(Mode::Normal, (100, 50), (1100, 820), 1.0));
        assert!(t.observe(Mode::Normal, (200, 60), (1000, 700), 1.0));
        assert!(!t.observe(Mode::Normal, (200, 60), (1000, 700), 1.0), "unchanged");
        // maximize: Windows reports the maximized rect (-8,-8 full screen); ignored
        assert!(t.observe(Mode::Maximized, (-8, -8), (2576, 1400), 1.0));
        assert_eq!(t.snapshot(), Some(WindowState { x: 200, y: 60, width: 1000, height: 700, maximized: true, scale_factor: 1.0 }));
        assert!(!t.observe(Mode::Maximized, (-8, -8), (2576, 1400), 1.0));
        // restore: back to the normal rect
        assert!(t.observe(Mode::Normal, (200, 60), (1000, 700), 1.0));
        assert_eq!(t.snapshot(), Some(WindowState { x: 200, y: 60, width: 1000, height: 700, maximized: false, scale_factor: 1.0 }));
    }

    #[test]
    fn tracker_ignores_minimised_and_fullscreen() {
        let mut t = Tracker::default();
        t.observe(Mode::Normal, (100, 50), (1100, 820), 1.25);
        let before = t.snapshot();
        assert!(!t.observe(Mode::Minimized, (-32000, -32000), (160, 28), 1.0));
        assert!(!t.observe(Mode::Fullscreen, (0, 0), (2560, 1440), 1.0));
        assert_eq!(t.snapshot(), before);
        // minimised from maximized: still maximized
        t.observe(Mode::Maximized, (-8, -8), (2576, 1400), 1.25);
        t.observe(Mode::Minimized, (-32000, -32000), (0, 0), 1.25);
        assert!(t.snapshot().unwrap().maximized);
    }

    #[test]
    fn tracker_never_records_the_sentinel_or_empty_sizes_as_normal() {
        let mut t = Tracker::default();
        t.observe(Mode::Normal, (100, 50), (1100, 820), 1.0);
        let before = t.snapshot();
        // a state change not yet visible in the mode (race): sentinel / zero size ignored
        assert!(!t.observe(Mode::Normal, (-32000, -32000), (160, 28), 1.0));
        assert!(!t.observe(Mode::Normal, (100, 50), (0, 0), 1.0));
        assert_eq!(t.snapshot(), before);
        // maximized before any normal rect: no snapshot (nothing to restore to)
        let mut t = Tracker::default();
        t.observe(Mode::Maximized, (-8, -8), (2576, 1400), 1.0);
        assert_eq!(t.snapshot(), None);
    }

    #[test]
    fn mode_priority_minimised_then_fullscreen_then_maximized() {
        assert_eq!(mode_from(false, false, false), Mode::Normal);
        assert_eq!(mode_from(false, false, true), Mode::Maximized);
        assert_eq!(mode_from(false, true, true), Mode::Fullscreen);
        assert_eq!(mode_from(true, false, true), Mode::Minimized, "minimised from maximized");
        assert_eq!(mode_from(true, true, false), Mode::Minimized);
    }

    // ----- debounce -----

    #[test]
    fn debounce_waits_for_quiet_then_flushes() {
        let t0 = Instant::now();
        let d = Duration::from_millis(500);
        assert_eq!(should_flush(None, t0, d), Flush::Idle);
        assert_eq!(should_flush(Some(t0), t0, d), Flush::Wait(d));
        assert_eq!(should_flush(Some(t0), t0 + Duration::from_millis(200), d), Flush::Wait(Duration::from_millis(300)));
        assert_eq!(should_flush(Some(t0), t0 + d, d), Flush::Now);
        assert_eq!(should_flush(Some(t0), t0 + Duration::from_secs(5), d), Flush::Now);
        // a newer change restarts the wait (dragging keeps postponing the write)
        let t1 = t0 + Duration::from_millis(450);
        assert_eq!(should_flush(Some(t1), t0 + Duration::from_millis(600), d), Flush::Wait(Duration::from_millis(350)));
    }

    // ----- WindowMemory -----

    fn wait_for(p: &Path, ms: u64) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            if load(p).is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn flush_writes_only_when_something_changed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        let m = WindowMemory::new(p.clone(), Tracker::default(), None);
        assert!(!m.flush().unwrap(), "nothing tracked yet");
        m.observe(Mode::Normal, (10, 20), (800, 600), 1.0, false);
        assert!(m.flush().unwrap());
        assert_eq!(load(&p).map(|s| rect(&s)), Some((10, 20, 800, 600)));
        assert!(!m.flush().unwrap(), "unchanged");
        m.observe(Mode::Minimized, (-32000, -32000), (160, 28), 1.0, false);
        assert!(!m.flush().unwrap(), "minimised is not a change");
        m.observe(Mode::Maximized, (-8, -8), (2576, 1400), 1.0, false);
        assert!(m.flush().unwrap());
        assert_eq!(load(&p), Some(WindowState { x: 10, y: 20, width: 800, height: 600, maximized: true, scale_factor: 1.0 }));
        // the state loaded at startup is not rewritten
        let on_disk = load(&p);
        let mut t = Tracker::default();
        t.observe(Mode::Normal, (10, 20), (800, 600), 1.0);
        t.observe(Mode::Maximized, (0, 0), (1, 1), 1.0);
        let m2 = WindowMemory::new(p.clone(), t, on_disk);
        assert!(!m2.flush().unwrap());
    }

    #[test]
    fn freeze_ignores_later_events() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        let m = WindowMemory::new(p.clone(), Tracker::default(), None);
        m.observe(Mode::Normal, (10, 20), (800, 600), 1.0, false);
        m.flush().unwrap();
        m.freeze();
        m.observe(Mode::Normal, (99, 99), (900, 700), 1.0, true);
        assert!(!m.flush().unwrap());
        assert_eq!(load(&p).map(|s| rect(&s)), Some((10, 20, 800, 600)));
    }

    #[test]
    fn worker_writes_after_the_debounce_and_only_when_armed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        let m = WindowMemory::new(p.clone(), Tracker::default(), None);
        m.start_worker(Duration::from_millis(50));
        // not armed (hidden window, our own restore): tracked, not written
        m.observe(Mode::Normal, (10, 20), (800, 600), 1.0, false);
        std::thread::sleep(Duration::from_millis(200));
        assert!(!p.exists());
        // armed: written after the quiet period
        m.observe(Mode::Normal, (30, 40), (800, 600), 1.0, true);
        assert!(wait_for(&p, 3000), "written by the worker");
        assert_eq!(load(&p).map(|s| rect(&s)), Some((30, 40, 800, 600)));
    }

    #[test]
    fn worker_postpones_while_changes_keep_coming() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        let m = WindowMemory::new(p.clone(), Tracker::default(), None);
        m.start_worker(Duration::from_millis(500));
        // a "drag": a change every 30 ms for ~600 ms: no write meanwhile
        for i in 0..20 {
            m.observe(Mode::Normal, (i, i), (800, 600), 1.0, true);
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(!p.exists(), "no write while dragging");
        assert!(wait_for(&p, 3000));
        assert_eq!(load(&p).map(|s| rect(&s)), Some((19, 19, 800, 600)));
    }
}
