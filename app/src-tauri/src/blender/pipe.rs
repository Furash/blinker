use std::io::{self, PipeReader, Read};
use std::sync::mpsc;

const READ_CHUNK: usize = 16 * 1024;

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

pub(super) fn start_reader(port: u16, reader: PipeReader) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::sync_channel(256);
    let spawned = std::thread::Builder::new()
        .name(format!("blender-{port}-out"))
        .spawn(move || read_pipe(reader, tx));
    // If the thread cannot start, `rx` reports Disconnected and output is lost, nothing else.
    drop(spawned);
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blender::test_support::*;
    use std::fs;

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
}
