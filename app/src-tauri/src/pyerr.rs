//! Python 3.11 (Windows) error texts for the theme file IO of the Settings dialog:
//! what `str(exc)` shows in the "Export failed" / "Import failed" boxes for
//! `Path.write_text`, `Path.read_text(encoding="utf-8")` and `json.loads`
//! (blinker_ui.py `_export_theme` / `_import_theme`).

use std::path::Path;

/// `Path.read_text(encoding="utf-8")`: strict UTF-8 decoding (the whole file in one
/// decode, so positions are file offsets), then universal newlines (`\r\n`, `\r` -> `\n`).
pub fn decode_text(bytes: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| utf8_error(bytes, &e))?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `bytes.decode("utf-8")`: strict UTF-8, no newline translation (the updater's
/// `_http_json`).
pub fn decode_utf8(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|e| utf8_error(bytes, &e))
}

/// Python's type name for a JSON value (`'list' object has no attribute ...`).
pub fn json_type_name(v: &serde_json::Value) -> &'static str {
    use serde_json::Value;
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `UnicodeDecodeError` text. Rust reports the same maximal invalid prefix as CPython;
/// a prefix that starts with a valid lead byte (C2-F4) failed on a continuation byte.
fn utf8_error(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let start = e.valid_up_to();
    let (len, reason) = match e.error_len() {
        None => (bytes.len() - start, "unexpected end of data"),
        Some(n) if (0xC2..=0xF4).contains(&bytes[start]) => (n, "invalid continuation byte"),
        Some(n) => (n, "invalid start byte"),
    };
    if len == 1 {
        format!("'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}", bytes[start])
    } else {
        format!("'utf-8' codec can't decode bytes in position {start}-{}: {reason}", start + len - 1)
    }
}

// ---------------------------------------------------------------------------
// json.loads (CPython's json.decoder / _json scanner), error positions only
// ---------------------------------------------------------------------------

enum Fail {
    /// StopIteration from scan_once: "Expecting value" at this index
    Stop(usize),
    Msg(&'static str, usize),
    /// nesting too deep to mirror (Python raises RecursionError around 1000)
    Deep,
}

const MAX_DEPTH: usize = 500;

struct Scan<'a> {
    s: &'a [char],
}

impl Scan<'_> {
    fn at(&self, i: usize) -> Option<char> {
        self.s.get(i).copied()
    }

    fn ws(&self, mut i: usize) -> usize {
        while matches!(self.at(i), Some(' ' | '\t' | '\n' | '\r')) {
            i += 1;
        }
        i
    }

    fn starts(&self, i: usize, lit: &str) -> bool {
        lit.chars().enumerate().all(|(k, c)| self.at(i + k) == Some(c))
    }

    fn digits(&self, mut i: usize) -> usize {
        while matches!(self.at(i), Some('0'..='9')) {
            i += 1;
        }
        i
    }

    /// `scan_once`: index after the value.
    fn value(&self, i: usize, depth: usize) -> Result<usize, Fail> {
        if depth > MAX_DEPTH {
            return Err(Fail::Deep);
        }
        match self.at(i) {
            None => Err(Fail::Stop(i)),
            Some('"') => self.string(i + 1),
            Some('{') => self.object(i + 1, depth + 1),
            Some('[') => self.array(i + 1, depth + 1),
            Some('n') if self.starts(i, "null") => Ok(i + 4),
            Some('t') if self.starts(i, "true") => Ok(i + 4),
            Some('f') if self.starts(i, "false") => Ok(i + 5),
            Some('N') if self.starts(i, "NaN") => Ok(i + 3),
            Some('I') if self.starts(i, "Infinity") => Ok(i + 8),
            Some('-') if self.starts(i, "-Infinity") => Ok(i + 9),
            Some(_) => self.number(i).ok_or(Fail::Stop(i)),
        }
    }

    /// `-?(?:0|[1-9]\d*)(\.\d+)?([eE][-+]?\d+)?`
    fn number(&self, i: usize) -> Option<usize> {
        let mut e = if self.at(i) == Some('-') { i + 1 } else { i };
        match self.at(e) {
            Some('0') => e += 1,
            Some('1'..='9') => e = self.digits(e + 1),
            _ => return None,
        }
        if self.at(e) == Some('.') && matches!(self.at(e + 1), Some('0'..='9')) {
            e = self.digits(e + 1);
        }
        if matches!(self.at(e), Some('e' | 'E')) {
            let d = if matches!(self.at(e + 1), Some('+' | '-')) { e + 2 } else { e + 1 };
            if matches!(self.at(d), Some('0'..='9')) {
                e = self.digits(d);
            }
        }
        Some(e)
    }

    /// `scanstring` from just after the opening quote.
    fn string(&self, start: usize) -> Result<usize, Fail> {
        let begin = start - 1;
        let mut p = start;
        loop {
            match self.at(p) {
                None => return Err(Fail::Msg("Unterminated string starting at", begin)),
                Some('"') => return Ok(p + 1),
                Some('\\') => match self.at(p + 1) {
                    None => return Err(Fail::Msg("Unterminated string starting at", begin)),
                    Some('u') => {
                        let ok = (p + 2..p + 6).all(|k| self.at(k).is_some_and(|c| c.is_ascii_hexdigit()));
                        if !ok {
                            return Err(Fail::Msg("Invalid \\uXXXX escape", p + 1));
                        }
                        p += 6;
                    }
                    Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => p += 2,
                    Some(_) => return Err(Fail::Msg("Invalid \\escape", p)),
                },
                Some(c) if (c as u32) < 0x20 => return Err(Fail::Msg("Invalid control character at", p)),
                Some(_) => p += 1,
            }
        }
    }

    fn object(&self, start: usize, depth: usize) -> Result<usize, Fail> {
        const NAME: &str = "Expecting property name enclosed in double quotes";
        let mut q = start; // index of the key's opening quote
        if self.at(q) != Some('"') {
            q = self.ws(q);
            if self.at(q) == Some('}') {
                return Ok(q + 1);
            }
            if self.at(q) != Some('"') {
                return Err(Fail::Msg(NAME, q));
            }
        }
        loop {
            let mut e = self.string(q + 1)?;
            if self.at(e) != Some(':') {
                e = self.ws(e);
                if self.at(e) != Some(':') {
                    return Err(Fail::Msg("Expecting ':' delimiter", e));
                }
            }
            e = self.ws(e + 1);
            e = match self.value(e, depth) {
                Err(Fail::Stop(p)) => return Err(Fail::Msg("Expecting value", p)),
                r => r?,
            };
            e = self.ws(e);
            match self.at(e) {
                Some('}') => return Ok(e + 1),
                Some(',') => {}
                _ => return Err(Fail::Msg("Expecting ',' delimiter", e)),
            }
            q = self.ws(e + 1);
            if self.at(q) != Some('"') {
                return Err(Fail::Msg(NAME, q));
            }
        }
    }

    fn array(&self, start: usize, depth: usize) -> Result<usize, Fail> {
        let mut e = self.ws(start);
        if self.at(e) == Some(']') {
            return Ok(e + 1);
        }
        loop {
            e = match self.value(e, depth) {
                Err(Fail::Stop(p)) => return Err(Fail::Msg("Expecting value", p)),
                r => r?,
            };
            e = self.ws(e);
            match self.at(e) {
                Some(']') => return Ok(e + 1),
                Some(',') => {}
                _ => return Err(Fail::Msg("Expecting ',' delimiter", e)),
            }
            e = self.ws(e + 1);
        }
    }
}

/// `JSONDecodeError.__str__`: `msg: line L column C (char N)`; positions in characters.
fn decode_error(s: &[char], msg: &str, pos: usize) -> String {
    let before = &s[..pos.min(s.len())];
    let line = before.iter().filter(|&&c| c == '\n').count() + 1;
    let col = match before.iter().rposition(|&c| c == '\n') {
        Some(nl) => pos - nl,
        None => pos + 1,
    };
    format!("{msg}: line {line} column {col} (char {pos})")
}

/// `json.loads` error message, or None when Python's decoder accepts the text (also
/// None when the text is nested too deeply to say; the caller reports its own error).
pub fn json_error(text: &str) -> Option<String> {
    let s: Vec<char> = text.chars().collect();
    if s.first() == Some(&'\u{feff}') {
        return Some(decode_error(&s, "Unexpected UTF-8 BOM (decode using utf-8-sig)", 0));
    }
    let sc = Scan { s: &s };
    let i = sc.ws(0);
    let r = match sc.value(i, 0) {
        Ok(e) => {
            let e = sc.ws(e);
            if e == s.len() {
                return None;
            }
            Fail::Msg("Extra data", e)
        }
        Err(f) => f,
    };
    match r {
        Fail::Stop(p) => Some(decode_error(&s, "Expecting value", p)),
        Fail::Msg(m, p) => Some(decode_error(&s, m, p)),
        Fail::Deep => None,
    }
}

// ---------------------------------------------------------------------------
// OSError
// ---------------------------------------------------------------------------

/// `str(WindowsPath(p))`: backslashes, repeated separators collapsed (a UNC prefix
/// kept), no trailing separator except at a root.
fn windows_path(p: &str) -> String {
    let s = p.replace('/', "\\");
    let unc = s.starts_with("\\\\");
    let mut out = String::new();
    for c in s.chars() {
        if c == '\\' && out.ends_with('\\') {
            continue;
        }
        out.push(c);
    }
    if unc {
        out.insert(0, '\\');
    }
    if out.len() > 1 && out.ends_with('\\') && !out.ends_with(":\\") && !(unc && out.matches('\\').count() <= 4) {
        out.pop();
    }
    out
}

fn shown_path(path: &Path) -> String {
    let p = path.to_string_lossy();
    if cfg!(windows) {
        windows_path(&p)
    } else {
        p.into_owned()
    }
}

/// `str(OSError)` for opening `path` (read or write): `[Errno N] text: 'path'` for the
/// cases CPython's open() maps on Windows (not found, permission denied, a directory,
/// an invalid name), else the system's own message.
pub fn os_error(e: &std::io::Error, path: &Path) -> String {
    use std::io::ErrorKind;
    let (errno, text) = if path.is_dir() {
        // Windows Python: PermissionError for a directory; POSIX: IsADirectoryError
        if cfg!(windows) { (13, "Permission denied") } else { (21, "Is a directory") }
    } else {
        match (e.kind(), e.raw_os_error()) {
            // ERROR_INVALID_NAME, ERROR_BAD_PATHNAME -> EINVAL in the CRT
            (_, Some(123 | 161)) if cfg!(windows) => (22, "Invalid argument"),
            (ErrorKind::NotFound, _) => (2, "No such file or directory"),
            (ErrorKind::PermissionDenied, _) => (13, "Permission denied"),
            _ => return e.to_string(),
        }
    };
    format!("[Errno {errno}] {text}: {}", crate::theme::python_repr_str(&shown_path(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_path_like_pathlib() {
        assert_eq!(windows_path("C:/a//b/"), "C:\\a\\b");
        assert_eq!(windows_path("C:/"), "C:\\");
        assert_eq!(windows_path("C:\\a\\b.json"), "C:\\a\\b.json");
        assert_eq!(windows_path("//srv/share/x"), "\\\\srv\\share\\x");
    }

    #[test]
    fn deep_nesting_is_left_to_the_caller() {
        let s = format!("{{\"a\": {}{}}}", "[".repeat(2000), "]".repeat(2000));
        assert_eq!(json_error(&s), None);
    }
}
