use std::io::PipeReader;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::pipe::start_reader;
use super::spawn::{spawn, Plan};
use super::state::{lock, ChildRef, Shared};

const RESTART_EXIT_CODE: i32 = 75;
const RESTART_MARKER: &str = "blinker_restart_path";
const RESTART_BLEND: &str = "blinker_restart.blend";
const RESTARTING_LINE: &str = "\n[restarting blender...]\n\n";
/// How often the monitor checks whether Blender has exited.
const POLL: Duration = Duration::from_millis(20);
/// After exit, how long to wait for output still in the pipe (a grandchild may hold it open).
const DRAIN_TIMEOUT: Duration = Duration::from_millis(1000);
/// Upper bound for one coalesced sink push while Blender floods output.
const MAX_EMIT: usize = 256 * 1024;

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
pub(super) fn monitor(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blender::test_support::*;
    use crate::blender::spawn::{display_path, LaunchSpec};
    use std::fs;

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
}
