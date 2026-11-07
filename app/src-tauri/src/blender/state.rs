use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::history::History;

pub trait OutputSink: Send + Sync {
    /// `seq` is the port's console sequence number after `text` was appended (see
    /// `History::total`): `Supervisor::output_with_seq` returns the same counter, so a
    /// pushed text is in a snapshot exactly when its `seq` <= the snapshot's.
    fn push(&self, port: u16, text: &str, seq: u64);
    fn exited(&self, port: u16, code: Option<i32>);
}

pub(super) type ChildRef = Arc<Mutex<Child>>;

pub(super) struct Slot {
    pub(super) history: History,
    pub(super) alive: bool,
    pub(super) stop_requested: bool,
    pub(super) child: Option<ChildRef>,
    /// Blender has exited but `sink.exited` for it has not been delivered yet; a new
    /// launch on this port waits for it so a stale `exited` can never follow the new
    /// launch's header.
    pub(super) finishing: bool,
}

impl Slot {
    pub(super) fn new() -> Self {
        Slot { history: History::new(), alive: false, stop_requested: false, child: None, finishing: false }
    }
}

pub(super) struct State {
    pub(super) slots: HashMap<u16, Slot>,
    pub(super) shutting_down: bool,
}

pub(super) struct Shared {
    pub(super) state: Mutex<State>,
    /// Notified when a slot's `finishing` clears.
    pub(super) finished: Condvar,
    pub(super) sink: Arc<dyn OutputSink>,
    /// `tempfile.gettempdir()`: where bootstrap.py leaves the restart markers.
    pub(super) temp_dir: PathBuf,
    /// Test seam: pauses the monitor between Blender's exit and the restart decision.
    #[cfg(test)]
    pub(super) gate: Mutex<Option<(mpsc::Sender<Option<i32>>, mpsc::Receiver<()>)>>,
}

impl Shared {
    /// Append to the history, then hand the text to the sink with no lock held.
    pub(super) fn emit(&self, port: u16, text: &str) {
        if text.is_empty() {
            return;
        }
        let seq = lock(&self.state).slots.entry(port).or_insert_with(Slot::new).history.append(text);
        self.sink.push(port, text, seq);
    }
}

pub(super) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
