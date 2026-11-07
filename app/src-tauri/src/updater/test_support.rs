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

    /// A zip archive with `entries` (name -> bytes; a name ending in `/` is a folder),
    /// stored without compression (enough for `Expand-Archive`).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut c = 0xFFFF_FFFFu32;
            for &b in data {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
                }
            }
            !c
        }
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data) in entries {
            let (crc, len, offset) = (crc32(data), data.len() as u32, out.len() as u32);
            let attrs: u32 = if name.ends_with('/') { 0x10 } else { 0 };
            // local file header: version 20, no flags, method 0 (stored), time/date 0:00 1.1.1980
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            for v in [20u16, 0, 0, 0, 0x21] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in [crc, len, len] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            for v in [20u16, 20, 0, 0, 0, 0x21] {
                central.extend_from_slice(&v.to_le_bytes());
            }
            for v in [crc, len, len] {
                central.extend_from_slice(&v.to_le_bytes());
            }
            for v in [name.len() as u16, 0, 0, 0, 0] {
                central.extend_from_slice(&v.to_le_bytes());
            }
            central.extend_from_slice(&attrs.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let (cd_offset, cd_len, n) = (out.len() as u32, central.len() as u32, entries.len() as u16);
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        for v in [0u16, 0, n, n] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&cd_len.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// Tests that touch the process-wide PENDING / OFFER state run one at a time.
    pub static GLOBALS: Mutex<()> = Mutex::new(());

