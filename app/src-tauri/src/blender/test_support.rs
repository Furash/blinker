//! Shared helpers for the blender submodule tests.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::spawn::LaunchSpec;
use super::state::{lock, OutputSink};
use super::supervisor::Supervisor;
use crate::config::Folder;

    pub(super) const WAIT: Duration = Duration::from_secs(20);

    #[derive(Default)]
    pub(super) struct Rec {
        pub(super) text: HashMap<u16, String>,
        /// (port, seq, text) in push order
        pub(super) pushes: Vec<(u16, u64, String)>,
        pub(super) exits: Vec<(u16, Option<i32>)>,
    }

    #[derive(Default)]
    pub(super) struct RecSink {
        pub(super) rec: Mutex<Rec>,
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
        pub(super) fn wait_exits(&self, n: usize) -> Vec<(u16, Option<i32>)> {
            let deadline = Instant::now() + WAIT;
            let mut g = lock(&self.rec);
            while g.exits.len() < n {
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "timed out waiting for {n} exit(s); got {:?}", g.exits);
                g = self.cv.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
            }
            g.exits.clone()
        }
        pub(super) fn text(&self, port: u16) -> String {
            lock(&self.rec).text.get(&port).cloned().unwrap_or_default()
        }
    }

    /// Fields drop in declaration order: the Supervisor (which kills its children)
    /// goes before the TempDir the children run from.
    pub(super) struct Env {
        pub(super) sup: Supervisor,
        pub(super) sink: Arc<RecSink>,
        pub(super) t: tempfile::TempDir,
    }

    pub(super) fn env() -> Env {
        let t = tempfile::tempdir().unwrap();
        let tmp = t.path().join("temp");
        fs::create_dir_all(&tmp).unwrap();
        let sink = Arc::new(RecSink::default());
        let sup = Supervisor::with_temp_dir(sink.clone(), tmp);
        Env { sup, sink, t }
    }

    impl Env {
        pub(super) fn temp(&self) -> PathBuf {
            self.t.path().join("temp")
        }
        pub(super) fn addon(&self, name: &str, port: u16) -> Folder {
            let dir = self.t.path().join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("__init__.py"), b"").unwrap();
            Folder { path: dir.to_string_lossy().into_owned(), port, ..Folder::default() }
        }
        pub(super) fn bootstrap(&self) -> PathBuf {
            let p = self.t.path().join("bootstrap.py");
            fs::write(&p, b"# dummy").unwrap();
            p
        }
        pub(super) fn cmd(&self, name: &str, port: u16, script: &str) -> LaunchSpec {
            LaunchSpec {
                blender: "cmd.exe".into(),
                bootstrap: self.bootstrap(),
                folder: self.addon(name, port),
                blend: None,
                args_override: Some(vec!["/d".into(), "/c".into(), script.into()]),
            }
        }
        pub(super) fn bat(&self, name: &str, body: &str) -> PathBuf {
            let p = self.t.path().join(name);
            fs::write(&p, body.replace('\n', "\r\n")).unwrap();
            p
        }
        pub(super) fn set_gate(&self) -> (mpsc::Receiver<Option<i32>>, mpsc::Sender<()>) {
            let (code_tx, code_rx) = mpsc::channel();
            let (go_tx, go_rx) = mpsc::channel();
            *lock(&self.sup.shared.gate) = Some((code_tx, go_rx));
            (code_rx, go_tx)
        }
    }

    pub(super) const BUSY: &str = "for /L %i in (1,1,2000000000) do @rem";
