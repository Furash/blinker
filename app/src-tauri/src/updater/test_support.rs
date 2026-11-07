use std::sync::Mutex;

use crate::updater::{sha256_hex, Asset, Offer};

    pub fn file_with(bytes: &[u8]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    /// Serves `routes` (path -> body; anything else 404) on 127.0.0.1 until the test ends.
    pub fn serve(routes: Vec<(&'static str, Vec<u8>)>) -> String {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut req = Vec::new();
                let mut b = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match c.read(&mut b) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&b[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&req).lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("");
                let (status, body) = match routes.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => ("200 OK", body.clone()),
                    None => ("404 Not Found", b"nope".to_vec()),
                };
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = c.write_all(head.as_bytes());
                let _ = c.write_all(&body);
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    pub const ZIP_BYTES: &[u8] = b"PK fake zip";

    pub fn zip_hash() -> String {
        sha256_hex(file_with(ZIP_BYTES).path()).unwrap()
    }

    pub fn offer_for(base: &str, sha_path: Option<&str>) -> Offer {
        Offer {
            current: "0.1.0".into(),
            tag: "v0.2.0".into(),
            notes_preview: String::new(),
            zip: Asset { name: "BlinkerUI-v0.2.0.zip".into(), url: Some(format!("{base}/z.zip")) },
            sha: sha_path.map(|p| Asset { name: "BlinkerUI-v0.2.0.zip.sha256".into(), url: Some(format!("{base}{p}")) }),
        }
    }

    /// Tests that touch the process-wide PENDING / OFFER state run one at a time.
    pub static GLOBALS: Mutex<()> = Mutex::new(());

