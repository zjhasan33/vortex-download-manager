use std::collections::VecDeque;
use std::error::Error as StdError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use reqwest::header::{ACCEPT_RANGES, AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, RANGE, WWW_AUTHENTICATE};
use reqwest::{Client, Proxy, StatusCode};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;

use crate::auth::{self, Cred};

const PART_EXT: &str = ".vtx.part";

/// Print the full network error chain (incl. cert/proxy causes) to stdout.
pub fn log_net_err(e: &reqwest::Error, ctx: &str) {
    eprintln!("[vortex-net] {ctx}: {e}");
    let mut src = e.source();
    let mut i = 0usize;
    while let Some(s) = src {
        eprintln!("[vortex-net]   cause[{i}]: {s}");
        src = s.source();
        i += 1;
    }
}

pub fn build_client(proxy: &str) -> Result<Client, String> {
    build_client_with_ua(proxy, crate::tools::BROWSER_UA, &[])
}

/// Like `build_client`, but with extra default request headers (e.g. cookies
/// and referer captured from a browser download takeover).
pub fn build_client_with_headers(proxy: &str, headers: &[(String, String)]) -> Result<Client, String> {
    build_client_with_ua(proxy, crate::tools::BROWSER_UA, headers)
}

/// Client that presents a neutral tool UA (e.g. `Wget/x`). Some mirror
/// anti-hotlinking guards bounce browser-like UAs in an endless 302 loop;
/// a curl/wget-grade UA gets served normally.
pub fn build_tool_client(proxy: &str) -> Result<Client, String> {
    build_client_with_ua(proxy, crate::tools::TOOL_UA, &[])
}

pub fn build_tool_client_with_headers(proxy: &str, headers: &[(String, String)]) -> Result<Client, String> {
    build_client_with_ua(proxy, crate::tools::TOOL_UA, headers)
}

fn build_client_with_ua(proxy: &str, ua: &str, extra: &[(String, String)]) -> Result<Client, String> {
    let mut cb = Client::builder()
        .user_agent(ua)
        // Small TTFB chunks: disable Nagle so range requests stream immediately.
        .tcp_nodelay(true)
        .tcp_keepalive(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(15))
        // Mirror/CDN chains can bounce 15+ hops; follow up to 20 redirects across
        // hosts and schemes (https<->http) — same behaviour as IDM.
        .redirect(reqwest::redirect::Policy::limited(20))
        // Keep a pooled connection per segment alive for retries/resume.
        .pool_max_idle_per_host(64)
        // Let HTTP/2 dynamically grow the receive window so large files don't
        // stall waiting for WINDOW_UPDATE round-trips.
        .http2_adaptive_window(true)
        .http2_initial_stream_window_size(8 * 1024 * 1024)
        .http2_initial_connection_window_size(16 * 1024 * 1024);
    if !proxy.trim().is_empty() {
        let p = Proxy::all(proxy.trim()).map_err(|e| format!("Bad proxy: {e}"))?;
        cb = cb.proxy(p);
    } else {
        // Never fall back to system/HTTP(S)_PROXY env (e.g. BurpSuite 127.0.0.1:8080).
        cb = cb.no_proxy();
    }
    if !extra.is_empty() {
        let mut hdrs = reqwest::header::HeaderMap::new();
        for (k, v) in extra {
            if let (Ok(k), Ok(v)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                hdrs.insert(k, v);
            }
        }
        if !hdrs.is_empty() {
            cb = cb.default_headers(hdrs);
        }
    }
    cb.build().map_err(|e| format!("Client error: {e}"))
}

/// First `WWW-Authenticate` header value (e.g. `Basic realm="x"`, `Digest ...`).
fn challenge_of(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Send a range GET through the task's auth context. If the server answers 401
/// with a Digest challenge the request is retried once with the computed header
/// and the winning header is stored back into `auth`.
async fn send_authorized(
    client: &Client,
    url: &str,
    range: Option<&str>,
    auth: &mut Option<AuthCtx>,
) -> Result<(reqwest::Response, Option<AuthCtx>), reqwest::Error> {
    let hdr = auth.as_ref().map(|a| a.hdr.clone());
    let mut req = client.get(url);
    if let Some(r) = range {
        req = req.header(RANGE, r);
    }
    if let Some(h) = &hdr {
        req = req.header(AUTHORIZATION, h);
    }
    let resp = req.send().await?;
    if resp.status() == StatusCode::UNAUTHORIZED {
        if let (Some(ctx), Some(ch)) = (auth.as_ref(), challenge_of(&resp)) {
            if ch.trim_start().to_ascii_lowercase().starts_with("digest") {
                if let Some(dh) = auth::digest_auth_value("GET", url, &ctx.cred, &ch) {
                    let prev = auth.clone();
                    let ctx2 = AuthCtx { hdr: dh.clone(), cred: ctx.cred.clone() };
                    let mut req2 = client.get(url);
                    if let Some(r) = range {
                        req2 = req2.header(RANGE, r);
                    }
                    req2 = req2.header(AUTHORIZATION, dh);
                    let resp2 = req2.send().await?;
                    if resp2.status() != StatusCode::UNAUTHORIZED {
                        return Ok((resp2, Some(ctx2)));
                    }
                    return Ok((resp2, prev));
                }
            }
        }
    }
    let next = auth.clone();
    Ok((resp, next))
}

/// Put a task into the waiting-for-login state and ask the UI to show a dialog.
pub fn flag_needs_auth(task: &Task) {
    task.set_error("Authentication required (HTTP 401)");
    task.set_status(DlStatus::NeedsAuth);
    let _ = task.app.emit(
        "auth-required",
        serde_json::json!({
            "id": task.id,
            "url": task.url,
            "host": auth::host_of(&task.url),
        }),
    );
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DlStatus {
    Queued,
    Downloading,
    Paused,
    Completed,
    Merging,
    Error,
    Cancelled,
    /// Waiting for the user to supply login credentials (HTTP 401/407).
    NeedsAuth,
}

impl DlStatus {
    fn desc(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Merging => "merging",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::NeedsAuth => "needs_auth",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DlView {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub title: String,
    pub category: String,
    pub total_size: u64,
    pub downloaded: u64,
    pub speed: u64,
    pub progress: f64,
    pub eta: u64,
    pub segments: usize,
    pub connections: usize,
    /// Live worker connections at this moment (0 when idle).
    #[serde(default)]
    pub live: usize,
    pub status: DlStatus,
    pub error: Option<String>,
    pub thumbnail: Option<String>,
    pub source: String,
    pub save_path: String,
    pub created_at: u64,
    #[serde(default)]
    pub completed_at: Option<u64>,
    #[serde(default)]
    pub format_id: Option<String>,
}

#[derive(Clone)]
pub struct Segment {
    start: u64,
    end: u64,
    part: PathBuf,
}

/// Login + the current Authorization header value for a task's requests.
#[derive(Clone)]
pub struct AuthCtx {
    pub cred: Cred,
    pub hdr: String,
}

#[derive(Clone)]
pub struct StartOpts {
    pub segments: usize,
    pub filename: Option<String>,
    pub categorize: bool,
    pub start_at: Option<u64>,
    pub auto_retries: u32,
    pub proxy: String,
    /// Referer header to send (the page that initiated the browser download).
    pub referer: Option<String>,
    /// Cookie header string (e.g. "sid=abc; pref=1") for authenticated downloads.
    pub cookies: Option<String>,
}

impl Default for StartOpts {
    fn default() -> Self {
        Self::new(8)
    }
}

impl StartOpts {
    pub fn new(segments: usize) -> Self {
        StartOpts {
            segments,
            filename: None,
            categorize: true,
            start_at: None,
            auto_retries: 3,
            proxy: String::new(),
            referer: None,
            cookies: None,
        }
    }

    /// Extra request headers a download should carry
    /// (Cookie/Referer captured from a browser download takeover).
    pub fn extra_headers(&self) -> Vec<(String, String)> {
        let mut h = Vec::new();
        if let Some(c) = self.cookies.as_deref().map(|c| c.trim()).filter(|c| !c.is_empty()) {
            h.push(("Cookie".into(), c.to_string()));
        }
        if let Some(r) = self.referer.as_deref().map(|r| r.trim()).filter(|r| !r.is_empty()) {
            h.push(("Referer".into(), r.to_string()));
        }
        h
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub view: DlView,
    pub segments: Vec<[u64; 2]>,
}

pub struct Task {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub save_path: PathBuf,
    pub category: String,
    pub source: String,
    pub thumbnail: Option<String>,
    pub created_at: u64,
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub num_segments: usize,
    /// Max simultaneous connections (segments are split into smaller chunks than this).
    pub max_conns: usize,
    /// Number of worker connections currently running (real-time, for stats).
    pub live: AtomicUsize,
    pub status: RwLock<DlStatus>,
    pub error: Mutex<Option<String>>,
    pub cancel: AtomicBool,
    pub paused: AtomicBool,
    pub history: Mutex<VecDeque<(Instant, u64)>>,
    pub limit: Arc<AtomicU64>,
    pub app: AppHandle,
    pub client: Client,
    pub retries: AtomicU64,
    pub auto_retries: u32,
    pub start_at: Option<u64>,
    pub auth: Mutex<Option<AuthCtx>>,
    segments: Mutex<Vec<Segment>>,
    done_flags: Mutex<Vec<bool>>,
    /// Which chunk each worker has currently claimed (for dynamic work-stealing).
    claimed: Mutex<Vec<bool>>,
    /// Epoch ms when the download reached Completed (None until then).
    completed_at: Mutex<Option<u64>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Heuristic: is this URL likely a downloadable file (vs a plain webpage)?
pub fn url_is_downloadable(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    const EXTS: &[&str] = &[
        // media
        "mp4", "mkv", "webm", "avi", "mov", "flv", "m4v", "wmv", "mpg", "mpeg", "3gp", "m4a", "aac", "flac", "wav", "ogg", "opus", "mp3",
        // archives / programs / docs
        "zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "dmg", "cab", "exe", "msi", "apk", "appimage", "whl", "deb", "rpm", "pdf", "epub", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "csv", "ttf", "otf", "bin", "img", "ipa", "torrent",
        // subtitles
        "srt", "vtt",
    ];
    if let Some(dot) = path.rfind('.') {
        let ext = &path[dot + 1..];
        let ext: &str = ext.trim_end_matches('/');
        if EXTS.contains(&ext) {
            return true;
        }
    }
    false
}

pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        format!("download_{}", now_ms())
    } else {
        trimmed.to_string()
    }
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let base = Path::new(name);
    let stem = base.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.to_string());
    let ext = base.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let cand = dir.join(name);
    if !cand.exists() {
        return cand;
    }
    let mut i = 1u32;
    loop {
        let c = dir.join(format!("{stem} ({i}){ext}"));
        if !c.exists() {
            return c;
        }
        i += 1;
    }
}

pub fn category_of(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    const VIDEO: &[&str] = &["mp4", "mkv", "webm", "avi", "mov", "flv", "m4v", "wmv", "mpg", "mpeg", "3gp"];
    const AUDIO: &[&str] = &["mp3", "m4a", "aac", "flac", "wav", "ogg", "opus", "wma", "aiff"];
    const DOC: &[&str] = &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "epub", "csv", "rtf", "odt"];
    const PROG: &[&str] = &["exe", "msi", "apk", "appimage", "whl", "deb", "rpm", "bat", "cmd", "ps1"];
    const ZIP: &[&str] = &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "dmg", "cab"];
    if VIDEO.contains(&ext.as_str()) { "video" }
    else if AUDIO.contains(&ext.as_str()) { "audio" }
    else if DOC.contains(&ext.as_str()) { "document" }
    else if PROG.contains(&ext.as_str()) { "program" }
    else if ZIP.contains(&ext.as_str()) { "zip" }
    else { "other" }
}

fn resolve_filename(resp: &reqwest::Response, url: &str) -> String {
    if let Some(cd) = resp.headers().get(CONTENT_DISPOSITION) {
        if let Ok(s) = cd.to_str() {
            if let Some(idx) = s.find("filename*=") {
                let end = s[idx + 10..].find(';').map(|e| e + idx + 10).unwrap_or(s.len());
                let raw = s[idx + 10..end].trim().trim_matches('"');
                let decoded = percent_decode(raw);
                if !decoded.is_empty() {
                    return sanitize(&decoded);
                }
            }
            if let Some(idx) = s.find("filename=") {
                let end = s[idx + 9..].find(';').map(|e| e + idx + 9).unwrap_or(s.len());
                let raw = s[idx + 9..end].trim().trim_matches('"');
                let decoded = percent_decode(raw);
                if !decoded.is_empty() {
                    return sanitize(&decoded);
                }
            }
        }
    }
    let path = url.split('?').next().unwrap_or(url);
    let last = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    sanitize(if last.is_empty() { "download" } else { last })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_total(resp: &reqwest::Response) -> u64 {
    if let Some(cr) = resp.headers().get(CONTENT_RANGE) {
        if let Ok(s) = cr.to_str() {
            if let Some(slash) = s.rfind('/') {
                let total = s[slash + 1..].trim();
                if !total.is_empty() && total != "*" {
                    if let Ok(n) = total.parse::<u64>() {
                        return n;
                    }
                }
            }
        }
    }
    resp.headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

impl Task {
    pub fn view(&self) -> DlView {
        let status = *self.status.read().unwrap();
        let total = self.total.load(Ordering::Relaxed);
        let done = self.done.load(Ordering::Relaxed);
        let speed = self.peek_speed();
        let progress = if total > 0 { (done as f64 / total as f64) * 100.0 } else { 0.0 };
        let eta = if speed > 0 && total > done { (total - done) / speed } else { 0 };
        DlView {
            id: self.id.clone(),
            url: self.url.clone(),
            filename: self.filename.clone(),
            title: self.filename.clone(),
            category: self.category.clone(),
            total_size: total,
            downloaded: done,
            speed,
            progress: progress.min(100.0),
            eta,
            segments: self.num_segments,
            connections: self.connections(),
            live: self.live.load(Ordering::Relaxed),
            status,
            error: self.error.lock().unwrap().clone(),
            thumbnail: self.thumbnail.clone(),
            source: self.source.clone(),
            save_path: self.save_path.display().to_string(),
            created_at: self.created_at,
            completed_at: *self.completed_at.lock().unwrap(),
            format_id: None,
        }
    }

    fn connections(&self) -> usize {
        if self.source == "youtube" { 1 } else { self.max_conns.max(1) }
    }

    fn peek_speed(&self) -> u64 {
        let h = self.history.lock().unwrap();
        if h.len() < 2 {
            return 0;
        }
        let (t0, b0) = *h.front().unwrap();
        let (t1, b1) = *h.back().unwrap();
        let dt = t1.duration_since(t0).as_secs_f64().max(0.05);
        ((b1 - b0) as f64 / dt) as u64
    }

    fn push_history(&self) {
        let mut h = self.history.lock().unwrap();
        h.push_back((Instant::now(), self.done.load(Ordering::Relaxed)));
        while h.len() > 2 && h.front().map(|(t, _)| t.elapsed() > Duration::from_secs(3)).unwrap_or(false) {
            h.pop_front();
        }
    }

    pub fn set_status(&self, s: DlStatus) {
        if s == DlStatus::Completed {
            *self.completed_at.lock().unwrap() = Some(now_ms());
        }
        *self.status.write().unwrap() = s;
        let err = self.error.lock().unwrap().clone();
        let _ = self.app.emit(
            "download-status",
            serde_json::json!({ "id": self.id, "status": s.desc(), "error": err }),
        );
        let _ = self.app.emit("downloads-changed", ());
        if s == DlStatus::Completed {
            let settings = crate::state::load_settings(&self.app);
            if settings.notifications {
                crate::state::notify_done(
                    &self.app,
                    &format!("{} — completed", self.filename),
                    "Download finished",
                );
            }
        }
    }

    pub fn set_error(&self, msg: &str) {
        *self.error.lock().unwrap() = Some(msg.to_string());
    }

    /// Apply a fresh login and queue for retry. Digest auth is negotiated
    /// automatically against the server's challenge on the next request.
    pub fn set_auth_ctx(&self, cred: Cred) {
        let hdr = auth::basic_auth_value(&cred.username, &cred.password);
        *self.auth.lock().unwrap() = Some(AuthCtx { cred, hdr });
        *self.error.lock().unwrap() = None;
        *self.status.write().unwrap() = DlStatus::Queued;
    }
}

pub async fn start(
    app: AppHandle,
    url: String,
    save_path: String,
    opts: StartOpts,
    limit: Arc<AtomicU64>,
) -> Result<Arc<Task>, String> {
    let base = PathBuf::from(save_path.trim());
    let mut url = url;

    let extra = opts.extra_headers();
    let mut client = if extra.is_empty() {
        build_client(&opts.proxy)?
    } else {
        build_client_with_headers(&opts.proxy, &extra)?
    };

    // Auto-login with any credential already saved for this host (Basic/Digest).
    let settings = crate::state::load_settings(&app);
    let mut auth = auth::find_cred(&settings.credentials, &url).map(|cred| AuthCtx {
        hdr: auth::basic_auth_value(&cred.username, &cred.password),
        cred,
    });

    let probe = match send_authorized(&client, &url, Some("bytes=0-0"), &mut auth).await {
        Ok((p, a)) => {
            auth = a;
            p
        }
        Err(e) if e.is_redirect() => {
            // Mirror anti-hotlinking guards bounce browser-like UAs in an endless
            // 302 loop (e.g. mirrors.nju.edu.cn redirects to itself); retry once
            // with a neutral tool UA like IDM/wget do.
            log_net_err(&e, "probe redirect-loop; retrying with tool UA");
            client = if extra.is_empty() {
                build_tool_client(&opts.proxy)?
            } else {
                build_tool_client_with_headers(&opts.proxy, &extra)?
            };
            let (p, a) = send_authorized(&client, &url, Some("bytes=0-0"), &mut auth)
                .await
                .map_err(|e2| {
                    log_net_err(&e2, "probe failed");
                    format!("Connection failed: {e2}")
                })?;
            auth = a;
            p
        }
        Err(e) => {
            log_net_err(&e, "probe failed");
            if e.is_redirect() {
                eprintln!("[vortex-net] probe redirect root cause: {:?}", e.source());
            }
            return Err(format!("Connection failed: {e}"));
        }
    };

    let accept_ranges = probe
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok().map(|s| s.to_lowercase().contains("bytes")))
        .unwrap_or(false);

    // Resolve every redirect once, then pin segment workers straight to the final
    // host (mirrors/CDNs bounce a lot; replaying the chain per chunk is fragile).
    let final_url = probe.url().to_string();
    if final_url != url {
        eprintln!("[vortex-net] url resolved: {url} -> {final_url}");
        url = final_url;
    }

    let got_206 = probe.status() == StatusCode::PARTIAL_CONTENT;
    let total = parse_total(&probe);

    // Some servers reply 200 to `bytes=0-0` but still honour real ranges; re-probe once.
    let mut ranged = got_206;
    if !ranged && accept_ranges && total > 0 {
        let mut auth2 = auth.clone();
        if let Ok((p2, a2)) = send_authorized(&client, &url, Some("bytes=0-1"), &mut auth2).await {
            if p2.status() == StatusCode::PARTIAL_CONTENT {
                ranged = true;
            }
            auth = a2;
        }
    }

    let name = opts
        .filename
        .as_deref()
        .map(|f| sanitize(f))
        .unwrap_or_else(|| resolve_filename(&probe, &url));
    let category = category_of(&name).to_string();

    let eff_dir = crate::state::save_dir_for(&base, &name, opts.categorize);
    fs::create_dir_all(&eff_dir).map_err(|e| format!("Cannot create folder: {e}"))?;
    let save_path_final = unique_path(&eff_dir, &name);

    let segs;
    let max_conns;
    if ranged && total > 0 && opts.segments > 1 {
        max_conns = opts.segments.clamp(2, 32);
        segs = split_range(total, max_conns, &save_path_final);
    } else {
        max_conns = 1;
        segs = vec![Segment { start: 0, end: u64::MAX, part: part_of(&save_path_final, 0) }];
    }
    let num_segments = segs.len();

    let mut done0 = 0u64;
    for s in &segs {
        if let Ok(m) = s.part.metadata() {
            done0 += m.len();
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    let fname = save_path_final
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.clone());
    let task = Arc::new(Task {
        id,
        url,
        filename: fname,
        save_path: save_path_final,
        category,
        source: "http".into(),
        thumbnail: None,
        created_at: now_ms(),
        total: AtomicU64::new(total),
        done: AtomicU64::new(done0),
        segments: Mutex::new(segs),
        done_flags: Mutex::new(vec![false; num_segments]),
        claimed: Mutex::new(vec![false; num_segments]),
        num_segments,
        max_conns,
        live: AtomicUsize::new(0),
        status: RwLock::new(DlStatus::Queued),
        error: Mutex::new(None),
        cancel: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        history: Mutex::new(VecDeque::new()),
        limit,
        app,
        client,
        retries: AtomicU64::new(0),
        auto_retries: opts.auto_retries,
        start_at: opts.start_at,
        auth: Mutex::new(auth),
        completed_at: Mutex::new(None),
    });

    Ok(task)
}

/// Rebuild a live task from persisted data (for resume after restart).
pub fn restore(
    app: AppHandle,
    view: DlView,
    segments: Vec<[u64; 2]>,
    limit: Arc<AtomicU64>,
) -> Result<Arc<Task>, String> {
    let save_path = PathBuf::from(view.save_path.clone());
    let num_segments = segments.len();
    let segs = segments
        .iter()
        .enumerate()
        .map(|(i, [s, e])| Segment { start: *s, end: *e, part: part_of(&save_path, i) })
        .collect::<Vec<_>>();
    let status = match view.status {
        DlStatus::Completed => DlStatus::Completed,
        _ => DlStatus::Paused,
    };
    let settings = crate::state::load_settings(&app);
    let auth = auth::find_cred(&settings.credentials, &view.url).map(|cred| AuthCtx {
        hdr: auth::basic_auth_value(&cred.username, &cred.password),
        cred,
    });
    let task = Arc::new(Task {
        id: view.id.clone(),
        url: view.url.clone(),
        filename: view.filename.clone(),
        save_path,
        category: view.category.clone(),
        source: view.source.clone(),
        thumbnail: view.thumbnail.clone(),
        created_at: view.created_at,
        total: AtomicU64::new(view.total_size),
        done: AtomicU64::new(view.downloaded),
        num_segments,
        max_conns: view.connections.clamp(1, 32),
        live: AtomicUsize::new(0),
        status: RwLock::new(status),
        error: Mutex::new(view.error.clone()),
        cancel: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        history: Mutex::new(VecDeque::new()),
        limit,
        app,
        client: build_client("").unwrap_or_else(|_| Client::new()),
        retries: AtomicU64::new(0),
        auto_retries: 3,
        start_at: None,
        auth: Mutex::new(auth),
        segments: Mutex::new(segs),
        done_flags: Mutex::new(vec![false; num_segments]),
        claimed: Mutex::new(vec![false; num_segments]),
        completed_at: Mutex::new(view.completed_at),
    });
    Ok(task)
}

impl Task {
    pub fn snapshot(&self) -> TaskSnapshot {
        let segs: Vec<[u64; 2]> = self
            .segments
            .lock()
            .unwrap()
            .iter()
            .map(|s| [s.start, s.end])
            .collect();
        TaskSnapshot { view: self.view(), segments: segs }
    }
}

pub async fn run(task: Arc<Task>) {
    // Detect pre-existing completed state (all segments done) so resume finishes instantly.
    task.push_history();

    // Scheduled start: wait (Queued) until the target timestamp.
    if let Some(ta) = task.start_at {
        let now = now_ms();
        if now < ta {
            task.set_status(DlStatus::Queued);
            while !task.cancel.load(Ordering::Relaxed) && now_ms() < ta {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if task.cancel.load(Ordering::Relaxed) {
                task.set_status(DlStatus::Cancelled);
                cleanup_parts(task.clone());
                return;
            }
        }
    }

    task.set_status(DlStatus::Downloading);
    let monitor = tokio::spawn(monitor_task(task.clone()));

    loop {
        if task.cancel.load(Ordering::Relaxed) {
            task.set_status(DlStatus::Cancelled);
            cleanup_parts(task.clone());
            break;
        }
        if task.paused.load(Ordering::Relaxed) {
            task.set_status(DlStatus::Paused);
            break;
        }

        // Re-open every unfinished chunk for claiming (covers retries after failure).
        reset_claims(&task);

        // Spin up to `max_conns` workers; each one pulls the next available chunk
        // until none are left, so fast connections take over slow connections' work.
        let mut workers = Vec::new();
        for _ in 0..task.max_conns {
            if task.cancel.load(Ordering::Relaxed) || task.paused.load(Ordering::Relaxed) {
                break;
            }
            let t = task.clone();
            let t2 = task.clone();
            t.live.fetch_add(1, Ordering::Relaxed);
            let spawned = tokio::spawn(async move {
                let r = worker_loop(t).await;
                t2.live.fetch_sub(1, Ordering::Relaxed);
                r
            });
            workers.push(spawned);
        }

        let mut all_ok = true;
        for w in workers {
            match w.await {
                Ok(res) => { if !res { all_ok = false; } }
                Err(e) => {
                    task.set_error(&format!("Worker crashed: {e}"));
                    all_ok = false;
                }
            }
        }

        if task.cancel.load(Ordering::Relaxed) || task.paused.load(Ordering::Relaxed) {
            continue;
        }

        if all_chunks_done(&task) {
            finalize(&task);
            break;
        }

        if all_ok {
            // Workers drained but chunks remain (paused mid-chunk); loop again.
            task.retries.store(0, Ordering::Relaxed);
            continue;
        }

        // Login needed: stay in this state until credentials are supplied.
        if *task.status.read().unwrap() == DlStatus::NeedsAuth {
            break;
        }

        // Some chunk failed — retry after short backoff, capped by auto_retries.
        let r = task.retries.fetch_add(1, Ordering::Relaxed) + 1;
        if r > task.auto_retries as u64 {
            task.set_error("Too many consecutive failures");
            task.set_status(DlStatus::Error);
            break;
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }

    monitor.abort();
}

/// A worker claims chunks one after another until the queue is empty.
async fn worker_loop(task: Arc<Task>) -> bool {
    loop {
        if task.cancel.load(Ordering::Relaxed) || task.paused.load(Ordering::Relaxed) {
            return true;
        }
        let (idx, seg) = match claim_chunk(&task) {
            Some(c) => c,
            None => return true,
        };
        if !download_segment(task.clone(), seg, idx).await {
            return false;
        }
    }
}

/// Atomically hand out the next unclaimed, unfinished chunk.
fn claim_chunk(task: &Task) -> Option<(usize, Segment)> {
    let segs = task.segments.lock().unwrap();
    let mut claimed = task.claimed.lock().unwrap();
    for (i, seg) in segs.iter().enumerate() {
        if claimed.get(i).copied().unwrap_or(true) {
            continue;
        }
        claimed[i] = true;
        if segment_done(task, seg, i) {
            continue;
        }
        return Some((i, seg.clone()));
    }
    None
}

/// Mark finished chunks as taken; leave failed/partial chunks claimable again.
fn reset_claims(task: &Task) {
    let segs = task.segments.lock().unwrap();
    let mut claimed = task.claimed.lock().unwrap();
    for (i, seg) in segs.iter().enumerate() {
        if claimed.get(i).is_some() {
            claimed[i] = segment_done(task, seg, i);
        }
    }
}

fn all_chunks_done(task: &Task) -> bool {
    let segs = task.segments.lock().unwrap();
    (0..segs.len()).all(|i| segment_done(task, &segs[i], i))
}

fn finalize(task: &Arc<Task>) {
    task.set_status(DlStatus::Merging);
    let out = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&task.save_path);
    if let Ok(mut f) = out {
        let mut ok = true;
        let mut parts: Vec<PathBuf> = {
            let mut segs = task.segments.lock().unwrap().clone();
            segs.retain(|s| s.end != u64::MAX);
            segs.sort_by_key(|s| s.start);
            segs.into_iter().map(|s| s.part).collect()
        };
        if parts.is_empty() {
            parts = vec![part_of(&task.save_path, 0)];
        }
        for p in &parts {
            match fs::File::open(p) {
                Ok(src) => {
                    let mut r = std::io::BufReader::with_capacity(1024 * 1024, src);
                    if std::io::copy(&mut r, &mut f).is_err() {
                        ok = false;
                        break;
                    }
                }
                Err(_) => { ok = false; }
            }
        }
        let _ = f.flush();
        drop(f);
        if ok {
            cleanup_parts(task.clone());
            let sz = fs::metadata(&task.save_path).map(|m| m.len()).unwrap_or(0);
            task.total.store(sz.max(task.total.load(Ordering::Relaxed)), Ordering::Relaxed);
            task.done.store(sz, Ordering::Relaxed);
            task.set_status(DlStatus::Completed);
        } else {
            task.set_error("Failed to merge parts");
            task.set_status(DlStatus::Error);
        }
    }
}

fn segment_done(task: &Task, seg: &Segment, idx: usize) -> bool {
    if task.done_flags.lock().unwrap().get(idx).copied().unwrap_or(false) {
        return true;
    }
    if seg.end == u64::MAX {
        false
    } else {
        seg.part.metadata().map(|m| m.len()).unwrap_or(0) >= seg.end - seg.start + 1
    }
}

fn mark_done(task: &Task, idx: usize) {
    if let Ok(mut f) = task.done_flags.lock() {
        if let Some(v) = f.get_mut(idx) {
            *v = true;
        }
    }
}

async fn download_segment(task: Arc<Task>, seg: Segment, idx: usize) -> bool {
    let expected = if seg.end == u64::MAX { u64::MAX } else { seg.end - seg.start + 1 };
    if seg.end != u64::MAX && seg.part.metadata().map(|m| m.len()).unwrap_or(0) >= expected {
        mark_done(&task, idx);
        return true;
    }

    let limit = task.limit.load(Ordering::Relaxed);
    let per_conn = if limit > 0 { limit / task.max_conns.max(1) as u64 } else { 0 };

    let std_file = match OpenOptions::new().create(true).append(true).open(&seg.part) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let offset = std_file.metadata().map(|m| m.len()).unwrap_or(0);
    // Buffered async writes: flush in ~1 MB batches instead of one disk syscall
    // per network chunk (this is a major throughput win on both HDD and SSD).
    let mut file = tokio::io::BufWriter::with_capacity(1024 * 1024, tokio::fs::File::from_std(std_file));
    let mut cursor = if seg.end == u64::MAX {
        seg.start.checked_add(offset).unwrap_or(seg.start)
    } else {
        seg.start + offset
    };

    if seg.end != u64::MAX && cursor >= seg.end + 1 {
        mark_done(&task, idx);
        return true;
    }

    let client = task.client.clone();
    let mut last_throttle = Instant::now();
    let mut throttle_bytes = 0u64;

    while !task.cancel.load(Ordering::Relaxed) && !task.paused.load(Ordering::Relaxed) {
        let range = if seg.end == u64::MAX {
            format!("bytes={cursor}-")
        } else {
            format!("bytes={cursor}-{}", seg.end)
        };
        let mut auth = task.auth.lock().unwrap().clone();
        let (resp, auth) = match send_authorized(&client, &task.url, Some(&range), &mut auth).await {
            Ok(v) => v,
            Err(e) => {
                log_net_err(&e, &format!("segment {idx} send"));
                tokio::time::sleep(Duration::from_millis(900)).await;
                continue;
            }
        };
        *task.auth.lock().unwrap() = auth;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED
            || (status == StatusCode::FORBIDDEN && challenge_of(&resp).is_some())
        {
            flag_needs_auth(&task);
            return false;
        }
        if !status.is_success() && status != StatusCode::PARTIAL_CONTENT {
            task.set_error(&format!("HTTP {}", status.as_u16()));
            return false;
        }
        if seg.end != u64::MAX && status != StatusCode::PARTIAL_CONTENT {
            // Server claims range support but answered with the whole file.
            task.set_error("Server ignored range request");
            return false;
        }

        let mut stream = resp.bytes_stream();
        let mut dropped = false;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(c) => {
                    if task.cancel.load(Ordering::Relaxed) || task.paused.load(Ordering::Relaxed) {
                        break;
                    }
                    if file.write_all(&c).await.is_err() {
                        let _ = file.flush().await;
                        return false;
                    }
                    task.done.fetch_add(c.len() as u64, Ordering::Relaxed);
                    cursor += c.len() as u64;

                    if seg.end != u64::MAX && cursor >= seg.end + 1 {
                        break;
                    }

                    if per_conn > 0 {
                        throttle_bytes += c.len() as u64;
                        let since = last_throttle.elapsed().as_secs_f64();
                        if throttle_bytes as f64 > per_conn as f64 * since && since > 0.0 {
                            let excess = throttle_bytes as f64 - per_conn as f64 * since;
                            tokio::time::sleep(Duration::from_secs_f64(excess / per_conn as f64)).await;
                            last_throttle = Instant::now();
                            throttle_bytes = 0;
                        }
                    }
                }
                Err(e) => {
                    log_net_err(&e, &format!("segment {idx} stream"));
                    dropped = true;
                    break;
                }
            }
        }
        let _ = file.flush().await;

        if task.cancel.load(Ordering::Relaxed) || task.paused.load(Ordering::Relaxed) {
            break;
        }

        if !dropped || (seg.end != u64::MAX && cursor >= seg.end + 1) {
            mark_done(&task, idx);
            return true;
        }
        // Connection dropped before finishing this segment — restart range from cursor.
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    true
}

async fn monitor_task(task: Arc<Task>) {
    let mut last = Instant::now();
    let mut last_done = task.done.load(Ordering::Relaxed);
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = task.done.load(Ordering::Relaxed);
        let t = Instant::now();
        {
            let mut h = task.history.lock().unwrap();
            h.push_back((t, now));
            while h.front().map(|(t0, _)| t.duration_since(*t0) > Duration::from_secs(3)).unwrap_or(false) {
                h.pop_front();
            }
        }
        let dt = t.duration_since(last).as_secs_f64().max(0.2);
        let speed = ((now.saturating_sub(last_done)) as f64 / dt) as u64;
        last = t;
        last_done = now;
        let total = task.total.load(Ordering::Relaxed);
        let progress = if total > 0 { (now as f64 / total as f64) * 100.0 } else { 0.0 };
        let eta = if speed > 0 && total > now { (total - now) / speed } else { 0 };
        let _ = task.app.emit(
            "download-progress",
            serde_json::json!({
                "id": task.id,
                "downloaded": now,
                "total_size": total,
                "speed": speed,
                "progress": progress.min(100.0),
                "eta": eta,
                "segments": task.num_segments,
                "connections": task.live.load(Ordering::Relaxed),
            }),
        );

        let st = *task.status.read().unwrap();
        if st != DlStatus::Downloading && st != DlStatus::Merging {
            break;
        }
    }
}

fn split_range(total: u64, connections: usize, save_path: &Path) -> Vec<Segment> {
    // Work-stealing needs more chunks than connections so free workers can take
    // over whatever a slow connection hasn't finished. Aim for ~4 chunks per
    // connection, keep each chunk between 1 MB and 32 MB (small tail), and cap
    // the total number of parts so resume state stays reasonable.
    let conns = connections.max(1) as u64;
    let max_chunks = 1024u64;
    let mut chunk = total.div_ceil(conns * 4).clamp(1024 * 1024, 32 * 1024 * 1024);
    if total.div_ceil(chunk) > max_chunks {
        chunk = total.div_ceil(max_chunks);
    }
    let mut out = Vec::new();
    let mut start = 0u64;
    let mut i = 0usize;
    while start < total {
        let end = (start + chunk - 1).min(total - 1);
        out.push(Segment { start, end, part: part_of(save_path, i) });
        start = end + 1;
        i += 1;
    }
    out
}

fn part_of(save: &Path, index: usize) -> PathBuf {
    let mut name = save.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{index}{PART_EXT}"));
    save.with_file_name(name)
}

fn cleanup_parts(task: Arc<Task>) {
    for s in task.segments.lock().unwrap().iter() {
        let _ = fs::remove_file(&s.part);
    }
}