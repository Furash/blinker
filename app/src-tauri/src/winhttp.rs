//! Minimal WinHTTP GET client for the updater: blinker_ui.py `_winhttp_stream`,
//! `_http_json` and `_http_download` (302-459). The OS stack does TLS (SChannel), the
//! proxy (WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY) and redirects, so the app needs no TLS
//! crate. Error texts are the Python ones (`WinHttpX failed (err=N)`, `HTTP 404`).

use std::fmt;
use std::io::Write;
use std::path::Path;

use serde_json::Value;

use crate::pyerr;

/// `HTTPError` (status >= 400) or any other failure (`str(e)` of the Python exception).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    Status(u32),
    Other(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // `f"HTTP {code} {reason}".rstrip()` with an empty reason
            HttpError::Status(code) => write!(f, "HTTP {code}"),
            HttpError::Other(msg) => f.write_str(msg),
        }
    }
}

/// Connection target of a URL (`urlsplit` as `_winhttp_stream` uses it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    /// path (default `/`) plus `?query`; the fragment is dropped
    pub path: String,
}

fn is_scheme(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic())
        && c.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

/// `urllib.parse.urlsplit` reduced to what `_winhttp_stream` reads, with its errors.
pub fn split_url(url: &str) -> Result<Target, String> {
    let (scheme, rest) = match url.split_once(':') {
        Some((s, r)) if is_scheme(s) => (s.to_ascii_lowercase(), r),
        _ => (String::new(), url),
    };
    if scheme != "http" && scheme != "https" {
        return Err(format!("unsupported scheme: {}", crate::theme::python_repr_str(&scheme)));
    }
    let secure = scheme == "https";
    let (netloc, rest) = match rest.strip_prefix("//") {
        Some(r) => {
            let end = r.find(['/', '?', '#']).unwrap_or(r.len());
            (&r[..end], &r[end..])
        }
        None => ("", rest),
    };
    let rest = rest.split('#').next().unwrap_or("");
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));

    let hostinfo = netloc.rsplit('@').next().unwrap_or("");
    let (host, port) = if let Some(b) = hostinfo.strip_prefix('[') {
        let (h, after) = b.split_once(']').unwrap_or((b, ""));
        (h, after.strip_prefix(':').unwrap_or(""))
    } else {
        hostinfo.split_once(':').unwrap_or((hostinfo, ""))
    };
    let port = if port.is_empty() {
        0
    } else if port.bytes().all(|b| b.is_ascii_digit()) {
        match port.parse::<u32>() {
            Ok(p) if p <= 65535 => p as u16,
            _ => return Err("Port out of range 0-65535".into()),
        }
    } else {
        return Err(format!("Port could not be cast to integer value as {}", crate::theme::python_repr_str(port)));
    };
    let port = if port == 0 { if secure { 443 } else { 80 } } else { port };
    let mut full = if path.is_empty() { "/".to_string() } else { path.to_string() };
    if !query.is_empty() {
        full.push('?');
        full.push_str(query);
    }
    Ok(Target { secure, host: host.to_lowercase(), port, path: full })
}

fn user_agent() -> String {
    format!("BlinkerUI/{}", env!("CARGO_PKG_VERSION"))
}

/// `_http_json`: GET with the GitHub Accept header, decode UTF-8, `json.loads`.
pub fn get_json(url: &str, timeout_ms: u32) -> Result<Value, HttpError> {
    let mut body = Vec::new();
    let ua = user_agent();
    stream(url, &[("User-Agent", &ua), ("Accept", "application/vnd.github+json")], &mut body, timeout_ms)?;
    let text = pyerr::decode_utf8(&body).map_err(HttpError::Other)?;
    if let Some(e) = pyerr::json_error(text) {
        return Err(HttpError::Other(e));
    }
    serde_json::from_str(text).map_err(|e| HttpError::Other(e.to_string()))
}

/// `_http_download`: stream the body to `dest` (created/truncated first, as `open(dest,
/// "wb")`). A partial file is removed on failure.
pub fn download(url: &str, dest: &Path, timeout_ms: u32) -> Result<(), HttpError> {
    let mut f = std::fs::File::create(dest).map_err(|e| HttpError::Other(pyerr::os_error(&e, dest)))?;
    let ua = user_agent();
    let r = stream(url, &[("User-Agent", &ua)], &mut f, timeout_ms)
        .and_then(|()| f.flush().map_err(|e| HttpError::Other(e.to_string())));
    drop(f);
    if r.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    r
}

/// `_winhttp_stream`: one GET, body streamed into `sink`. Timeouts (resolve, connect,
/// send, receive) are each `max(1000, timeout_ms)`.
#[cfg(windows)]
pub fn stream(url: &str, headers: &[(&str, &str)], sink: &mut dyn Write, timeout_ms: u32) -> Result<(), HttpError> {
    use core::ffi::c_void;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Networking::WinHttp::*;

    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    fn fail(what: &str) -> HttpError {
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        HttpError::Other(format!("{what} failed (err={err})"))
    }

    let t = split_url(url).map_err(HttpError::Other)?;
    unsafe {
        let session = WinHttpOpen(wide(&user_agent()).as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0);
        if session.is_null() {
            return Err(fail("WinHttpOpen"));
        }
        let session = Handle(session);
        let ms = timeout_ms.max(1000) as i32;
        WinHttpSetTimeouts(session.0, ms, ms, ms, ms);
        // Python passes 1 under the name "ALWAYS"; 1 is DISALLOW_HTTPS_TO_HTTP (the
        // WinHTTP default): redirects are followed except an https -> http downgrade.
        let policy: u32 = WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP;
        WinHttpSetOption(session.0, WINHTTP_OPTION_REDIRECT_POLICY, &policy as *const u32 as *const c_void, 4);

        let conn = WinHttpConnect(session.0, wide(&t.host).as_ptr(), t.port, 0);
        if conn.is_null() {
            return Err(fail("WinHttpConnect"));
        }
        let conn = Handle(conn);
        let flags = if t.secure { WINHTTP_FLAG_SECURE } else { 0 };
        let req = WinHttpOpenRequest(conn.0, wide("GET").as_ptr(), wide(&t.path).as_ptr(), null(), null(), null(), flags);
        if req.is_null() {
            return Err(fail("WinHttpOpenRequest"));
        }
        let req = Handle(req);
        let hdr: String = headers.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\r\n");
        if !hdr.is_empty() {
            let w = wide(&hdr);
            if WinHttpAddRequestHeaders(req.0, w.as_ptr(), (w.len() - 1) as u32, WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE) == 0 {
                return Err(fail("WinHttpAddRequestHeaders"));
            }
        }
        if WinHttpSendRequest(req.0, null(), 0, null(), 0, 0, 0) == 0 {
            return Err(fail("WinHttpSendRequest"));
        }
        if WinHttpReceiveResponse(req.0, null_mut()) == 0 {
            return Err(fail("WinHttpReceiveResponse"));
        }
        let mut status: u32 = 0;
        let mut size: u32 = 4;
        let mut idx: u32 = 0;
        if WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            &mut status as *mut u32 as *mut c_void,
            &mut size,
            &mut idx,
        ) == 0
        {
            return Err(fail("WinHttpQueryHeaders"));
        }
        if status >= 400 {
            return Err(HttpError::Status(status));
        }
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read: u32 = 0;
            if WinHttpReadData(req.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read) == 0 {
                return Err(fail("WinHttpReadData"));
            }
            if read == 0 {
                break;
            }
            sink.write_all(&buf[..read as usize]).map_err(|e| HttpError::Other(e.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn stream(_url: &str, _headers: &[(&str, &str)], _sink: &mut dyn Write, _timeout_ms: u32) -> Result<(), HttpError> {
    Err(HttpError::Other("WinHTTP is only available on Windows".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secure: bool, host: &str, port: u16, path: &str) -> Target {
        Target { secure, host: host.into(), port, path: path.into() }
    }

    #[test]
    fn split_url_like_urlsplit() {
        assert_eq!(
            split_url("https://api.github.com/repos/Furash/blinker/releases/latest").unwrap(),
            t(true, "api.github.com", 443, "/repos/Furash/blinker/releases/latest")
        );
        assert_eq!(split_url("HTTP://127.0.0.1:8123").unwrap(), t(false, "127.0.0.1", 8123, "/"));
        assert_eq!(split_url("https://u:p@Host.Example:8443/a?b=1#frag").unwrap(), t(true, "host.example", 8443, "/a?b=1"));
        assert_eq!(split_url("https://[::1]:9/x").unwrap(), t(true, "::1", 9, "/x"));
        assert_eq!(split_url("https://h:0/").unwrap(), t(true, "h", 443, "/"));
    }

    #[test]
    fn split_url_errors_like_python() {
        assert_eq!(split_url("ftp://x/y").unwrap_err(), "unsupported scheme: 'ftp'");
        assert_eq!(split_url("no-scheme").unwrap_err(), "unsupported scheme: ''");
        assert_eq!(split_url("https://h:abc/").unwrap_err(), "Port could not be cast to integer value as 'abc'");
        assert_eq!(split_url("https://h:70000/").unwrap_err(), "Port out of range 0-65535");
    }

    #[test]
    fn http_error_text() {
        assert_eq!(HttpError::Status(404).to_string(), "HTTP 404");
        assert_eq!(HttpError::Other("x".into()).to_string(), "x");
    }
}
