//! Site Grabber (IDM spider): crawl same-origin pages and collect downloadable files.
//! No extra crates — plain attribute scanning over the HTML.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct GrabItem {
    pub url: String,
    pub filename: String,
    pub kind: String,
}

const IMG: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "svg", "bmp", "ico", "avif"];
const AUD: &[&str] = &["mp3", "wav", "ogg", "oga", "m4a", "flac", "opus", "aac"];
const VID: &[&str] = &["mp4", "webm", "mov", "mkv", "avi", "m4v", "flv"];
const DOC: &[&str] = &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "csv", "epub", "rtf"];
const ARC: &[&str] = &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "exe", "msi", "apk", "dmg", "bin"];

fn kind_of(ext: &str) -> Option<&'static str> {
    if IMG.contains(&ext) {
        Some("image")
    } else if AUD.contains(&ext) {
        Some("audio")
    } else if VID.contains(&ext) {
        Some("video")
    } else if DOC.contains(&ext) {
        Some("document")
    } else if ARC.contains(&ext) {
        Some("archive")
    } else {
        None
    }
}

fn ext_of(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let seg = path.rsplit('/').next().unwrap_or("");
    match seg.rfind('.') {
        Some(i) if i + 1 < seg.len() => seg[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

fn filename_of(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let seg = path.rsplit('/').next().unwrap_or("");
    let name = urlencoding_decode(seg);
    if name.is_empty() {
        url.to_string()
    } else {
        name
    }
}

fn urlencoding_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as char);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            out.push(' ');
            i += 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Scan raw HTML for href="..." / src='...' values (case-insensitive attr names).
fn attr_at(b: &[u8], i: usize, name: &[u8]) -> bool {
    if i + name.len() > b.len() {
        return false;
    }
    b[i..i + name.len()].iter().zip(name.iter()).all(|(a, n)| a.to_ascii_lowercase() == *n)
}

fn extract_links(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0;
    while i + 5 < bytes.len() {
        let is_href = attr_at(bytes, i, b"href");
        let is_src = !is_href && attr_at(bytes, i, b"src");
        let is_srcset = !is_href && !is_src && attr_at(bytes, i, b"srcset");
        let is_datasrc = !is_href && !is_src && !is_srcset && attr_at(bytes, i, b"data-src");
        if !is_href && !is_src && !is_srcset && !is_datasrc {
            i += 1;
            continue;
        }
        // Skip matches inside longer attribute names (e.g. data-href).
        if i > 0 {
            let p = bytes[i - 1];
            if p.is_ascii_alphanumeric() || p == b'-' || p == b'_' || p == b':' {
                i += 1;
                continue;
            }
        }
        let mut j = i + if is_href { 4 } else if is_src { 3 } else if is_srcset { 6 } else { 8 };
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r') {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            i += 1;
            continue;
        }
        j += 1;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let quote = bytes[j];
        if quote != b'"' && quote != b'\'' {
            i = j + 1;
            continue;
        }
let rest = &html[j + 1..];
        let q = quote as char;
        if let Some(end) = rest.find(q) {
            let val = rest[..end].trim();
            if !val.is_empty() && val.len() < 4096 {
                if is_srcset {
                    for part in val.split(',') {
                        let u = part.split_whitespace().next().unwrap_or("").trim();
                        if !u.is_empty() && u.len() < 2048 {
                            out.push(u.to_string());
                        }
                    }
                } else if !val.is_empty() && val.len() < 2048 {
                    out.push(val.to_string());
                }
            }
            i = j + 1 + end + 1;
        } else {
            break;
        }
    }
    out
}

fn same_host(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.host_str().map(|s| s.to_lowercase()) == b.host_str().map(|s| s.to_lowercase())
}

/// Crawl `start_url` (same host only) and collect files matching `want` kinds.
/// `cancel` is an external stop flag: when set, crawling aborts immediately
/// and everything found so far is returned (used by the Stop button).
pub async fn grab_site(
    start_url: &str,
    max_pages: usize,
    want: Vec<String>,
    cancel: &AtomicBool,
) -> Result<Vec<GrabItem>, String> {
    let start = reqwest::Url::parse(start_url.trim()).map_err(|_| "Invalid URL".to_string())?;
    if start.scheme() != "http" && start.scheme() != "https" {
        return Err("Only http(s) URLs supported".to_string());
    }
    let want: HashSet<String> = want.into_iter().map(|s| s.to_lowercase()).collect();
    if want.is_empty() {
        return Err("Select at least one file type".to_string());
    }
    let max_pages = max_pages.clamp(1, 50);
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("Client error: {e}"))?;

    let mut queue: VecDeque<String> = VecDeque::from([start.to_string()]);
    let mut seen_pages: HashSet<String> = HashSet::new();
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut files: Vec<GrabItem> = Vec::new();
    let mut fetched = 0usize;

    while let Some(page) = queue.pop_front() {
        if fetched >= max_pages || cancel.load(Ordering::Relaxed) {
            break;
        }
        if !seen_pages.insert(page.clone()) {
            continue;
        }
        let page_url = match reqwest::Url::parse(&page) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let resp = match client
            .get(page_url.clone())
            .header(reqwest::header::USER_AGENT, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Vortex/1.0")
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        // Direct file URL (non-HTML): record it and move on.
        if !ct.contains("text/html") && !ct.is_empty() {
            let u = page_url.to_string();
            let ext = ext_of(&u);
            if let Some(kind) = kind_of(&ext) {
                if want.contains(kind) && seen_files.insert(u.clone()) {
                    files.push(GrabItem { url: u.clone(), filename: filename_of(&u), kind: kind.into() });
                }
            }
            fetched += 1;
            continue;
        }
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(_) => continue,
        };
        fetched += 1;
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let capped = if bytes.len() > 3_000_000 { &bytes[..3_000_000] } else { &bytes[..] };
        let html = String::from_utf8_lossy(capped);
        for raw in extract_links(&html) {
            let low = raw.to_lowercase();
            if low.starts_with("mailto:")
                || low.starts_with("javascript:")
                || low.starts_with("tel:")
                || low.starts_with("data:")
                || low.starts_with('#')
            {
                continue;
            }
            let abs = match page_url.join(&raw) {
                Ok(u) => u,
                Err(_) => continue,
            };
            if abs.scheme() != "http" && abs.scheme() != "https" {
                continue;
            }
            let u = abs.to_string();
            let ext = ext_of(&u);
            if let Some(kind) = kind_of(&ext) {
                if want.contains(kind) && seen_files.insert(u.clone()) {
                    files.push(GrabItem { url: u, filename: filename_of(&abs.to_string()), kind: kind.into() });
                }
            } else if same_host(&start, &abs) && fetched + queue.len() < max_pages && seen_pages.len() + queue.len() < 60 {
                // Candidate sub-page (no file extension): crawl one level deeper.
                if ext.is_empty() || ["html", "htm", "php", "asp", "aspx", "jsp"].contains(&ext.as_str()) {
                    queue.push_back(u);
                }
            }
            if files.len() >= 500 {
                break;
            }
        }
    }
    files.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.filename.cmp(&b.filename)));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_links_basics() {
        let html = r#"<a HREF="a.mp4">x</a><img src='b.webp'/><a href=noquote>c</a><a data-href="d.mp4">e</a>"#;
        let links = extract_links(html);
        assert!(links.contains(&"a.mp4".to_string()), "{links:?}");
        assert!(links.contains(&"b.webp".to_string()), "{links:?}");
        assert!(!links.iter().any(|l| l == "noquote"), "{links:?}");
        assert!(!links.iter().any(|l| l == "d.mp4"), "{links:?}");
    }

    #[test]
    fn ext_and_kind_cover_expected_types() {
        assert_eq!(kind_of("mp4"), Some("video"));
        assert_eq!(kind_of("mp3"), Some("audio"));
        assert_eq!(kind_of("pdf"), Some("document"));
        assert_eq!(kind_of("zip"), Some("archive"));
        assert_eq!(kind_of("webp"), Some("image"));
        assert_eq!(kind_of("html"), None);
        assert_eq!(ext_of("http://x/f.AAC?a=1#h"), "aac");
        assert_eq!(filename_of("http://x/a%20b.mp4"), "a b.mp4");
    }

    /// Live crawl against a localhost fixture (needs the `grabtest` server on
    /// 127.0.0.1:8931; skipped quietly when it isn't running).
    #[tokio::test]
    async fn grab_fixture_page() {
        let cancel = AtomicBool::new(false);
        let probe = reqwest::Client::new()
            .get("http://127.0.0.1:8931/index.html")
            .timeout(Duration::from_secs(3))
            .send()
            .await;
        if probe.is_err() {
            eprintln!("grabtest server not running — skipping");
            return;
        }
        let items = grab_site(
            "http://127.0.0.1:8931/index.html",
            10,
            vec!["video".into(), "audio".into(), "document".into(), "archive".into(), "image".into()],
            &cancel,
        )
        .await
        .expect("grab failed");
        let urls: Vec<&str> = items.iter().map(|i| i.url.as_str()).collect();
        for want in [
            "http://127.0.0.1:8931/video.mp4",
            "http://127.0.0.1:8931/song.MP3",
            "http://127.0.0.1:8931/files/doc.pdf?token=abc#page=2",
            "http://127.0.0.1:8931/pic.webp",
            "http://127.0.0.1:8931/movie.webm",
            "http://127.0.0.1:8931/deep.mp3",
            "https://other.example.com/x.zip",
        ] {
            assert!(urls.contains(&want), "missing {want} in {urls:?}");
        }
    }
}
