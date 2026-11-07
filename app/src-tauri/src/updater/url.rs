use crate::winhttp;

/// A downloaded asset's local file name: the last path component of the (network
/// supplied) name with characters Windows rejects replaced, never `.`/`..`/empty and
/// never a device name; otherwise `fallback`.
pub fn safe_file_name(name: &str, fallback: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).trim_start();
    if cleaned.is_empty() {
        return fallback.to_string();
    }
    const DEVICES: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
        "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = cleaned.split('.').next().unwrap_or("").trim_end();
    if DEVICES.iter().any(|d| d.eq_ignore_ascii_case(stem)) {
        return format!("_{cleaned}");
    }
    cleaned.to_string()
}

/// Asset URLs must be `https://`. `local_http_ok` (debug builds with
/// `BLINKER_UPDATE_API` set) also allows `http://127.0.0.1` for the local test server.
/// Decided on the parsed target (what WinHTTP connects to); URLs with user info are
/// refused.
pub fn url_allowed(url: &str, local_http_ok: bool) -> bool {
    match winhttp::split_url(url) {
        Ok(t) if !t.userinfo => t.secure || (local_http_ok && t.host == "127.0.0.1"),
        _ => false,
    }
}

pub(super) fn insecure_url(url: &str) -> String {
    format!("refusing a non-https URL: {url}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_file_name_keeps_plain_names_only() {
        assert_eq!(safe_file_name("BlinkerUI-v0.2.0.zip", "f"), "BlinkerUI-v0.2.0.zip");
        assert_eq!(safe_file_name("..\\..\\Windows\\evil.zip", "f"), "evil.zip");
        assert_eq!(safe_file_name("../x/evil.zip", "f"), "evil.zip");
        assert_eq!(safe_file_name("..", "f"), "f");
        assert_eq!(safe_file_name("", "f"), "f");
        assert_eq!(safe_file_name("dir/", "f"), "f");
        assert_eq!(safe_file_name("C:evil.zip", "f"), "C_evil.zip");
        assert_eq!(safe_file_name("a<b>|?*\".zip", "f"), "a_b_____.zip");
        assert_eq!(safe_file_name("x.zip. . ", "f"), "x.zip");
        assert_eq!(safe_file_name("CON.zip", "f"), "_CON.zip");
        assert_eq!(safe_file_name("nul", "f"), "_nul");
    }

    #[test]
    fn only_https_urls_unless_local_test_server() {
        assert!(url_allowed("https://github.com/x.zip", false));
        assert!(url_allowed("HTTPS://github.com/x.zip", false));
        assert!(!url_allowed("http://github.com/x.zip", false));
        assert!(!url_allowed("http://127.0.0.1:8000/x.zip", false));
        assert!(!url_allowed("file:///C:/x.zip", true));
        assert!(url_allowed("http://127.0.0.1:8000/x.zip", true));
        assert!(url_allowed("http://127.0.0.1/x.zip", true));
        assert!(!url_allowed("http://127.0.0.1.evil.com/x.zip", true));
        assert!(!url_allowed("http://127.0.0.1@evil.com/x.zip", true));
        assert!(!url_allowed("http://github.com/x.zip", true));
        // user info: the prefix "http://127.0.0.1:" would pass a string check
        assert!(!url_allowed("http://127.0.0.1:1@evil.com/x.zip", true));
        assert!(!url_allowed("http://u:p@127.0.0.1:8000/x.zip", true));
        assert!(!url_allowed("https://u@github.com/x.zip", false));
        assert!(!url_allowed("http://localhost:8000/x.zip", true));
    }
}
