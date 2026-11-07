use std::collections::VecDeque;

/// `QPlainTextEdit.setMaximumBlockCount(20000)` in the Python UI.
pub const HISTORY_MAX_LINES: usize = 20000;

/// Console history, one entry per text block (like `QPlainTextEdit` blocks:
/// newline count + 1), capped at `HISTORY_MAX_LINES`.
pub(super) struct History {
    lines: VecDeque<String>,
    /// Bytes ever appended to this port's console; never decreases (not even on clear).
    pub(super) total: u64,
}

impl History {
    pub(super) fn new() -> Self {
        History { lines: VecDeque::from([String::new()]), total: 0 }
    }
    /// Empty the console but keep the sequence counter monotonic.
    pub(super) fn clear(&mut self) {
        self.lines = VecDeque::from([String::new()]);
    }
    /// Append and return the new sequence number.
    pub(super) fn append(&mut self, text: &str) -> u64 {
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
    pub(super) fn text(&self) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blender::test_support::*;

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
}
