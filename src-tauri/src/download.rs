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
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::auth::{self, Cred};

const PART_EXT: &str = ".vtx.part";
const MAX_DOWNLOAD_BYTES: u64 = 1 << 40;

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

pub fn build_client_with_headers(proxy: &str, headers: &[(String, String)]) -> Result<Client, String> {
    build_client_with_ua(proxy, crate::tools::BROWSER_UA, headers)
}

pub fn build_tool_client(proxy: &str) -> Result<Client, String> {
    build_client_with_ua(proxy, crate::tools::TOOL_UA, &[])
}

pub fn build_tool_client_with_headers(proxy: &str, headers: &[(String, String)]) -> Result<Client, String> {
    build_client_with_ua(proxy, crate::tools::TOOL_UA, headers)
}

fn build_client_with_ua(proxy: &str, ua: &str, extra: &[(String, String)]) -> Result<Client, String> {
    let mut cb = Client::builder()
        .user_agent(ua)
        .tcp_nodelay(true)
        .tcp_keepalive(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(20))
        .pool_max_idle_per_host(64)
        .http2_adaptive_window(true)
        .http2_initial_stream_window_size(8 * 1024 * 1024)
        .http2_initial_connection_window_size(16 * 1024 * 1024);
    if !proxy.trim().is_empty() {
        let p = Proxy::all(proxy.trim()).map_err(|e| format!("Bad proxy: {e}"))?;
        cb = cb.proxy(p);
    } else {
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

fn challenge_of(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

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
    NeedsAuth,
    Resolving,
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
            Self::Resolving => "resolving",
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
    #[serde(default)]
    pub produced: Vec<String>,
}

#[derive(Clone)]
pub struct Segment {
    start: u64,
    end: u64,
    part: PathBuf,
}

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
    pub referer: Option<String>,
    pub cookies: Option<String>,
    pub on_exists: Option<String>,
}

impl Default for StartOpts {
    fn default() -> Self {
        Self::new(16)
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
            on_exists: None,
        }
    }

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
    // Persisted so a restarted session keeps the tunables the user chose
    // instead of silently resetting them to the hardcoded defaults.
    #[serde(default)]
    pub auto_retries: u32,
    #[serde(default)]
    pub start_at: Option<u64>,
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
    pub max_conns: AtomicUsize,
    pub penalty_until: AtomicU64,
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
    claimed: Mutex<Vec<bool>>,
    split_at: Mutex<Vec<u64>>,
    completed_at: Mutex<Option<u64>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn retry_backoff(attempt: u64) -> Duration {
    let shift = attempt.saturating_sub(1).min(5);
    let base_ms = 1000u64.saturating_mul(1 << shift).min(30_000);
    let mut x = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0x9E3779B9)
        .max(1);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    let pct = 75 + (x % 51) as u64;
    Duration::from_millis((base_ms * pct / 100).max(200))
}

fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|n| n.min(120))
}

fn is_fatal_status(s: StatusCode) -> bool {
    matches!(
        s,
        StatusCode::BAD_REQUEST
            | StatusCode::NOT_FOUND
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::GONE
            | StatusCode::RANGE_NOT_SATISFIABLE
            | StatusCode::NOT_IMPLEMENTED
    )
}

const MAX_CHUNK_ATTEMPTS: u64 = 15;
const STALL_TIMEOUT: Duration = Duration::from_secs(30);

pub fn url_is_downloadable(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    const EXTS: &[&str] = &[
        "mp4", "mkv", "webm", "avi", "mov", "flv", "m4v", "wmv", "mpg", "mpeg", "3gp", "m4a", "aac", "flac", "wav", "ogg", "opus", "mp3",
        "zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "dmg", "cab", "exe", "msi", "apk", "appimage", "whl", "deb", "rpm", "pdf", "epub", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "csv", "ttf", "otf", "bin", "img", "ipa", "torrent",
        "srt", "vtt",
    ];
    if let Some(dot) = path.rfind('.') {
        let ext = &path[dot + 1..];
        let ext: &str = ext.trim_end_matches('/');
        if EXTS.contains(&ext) {
            return true;
        }
    }
    // Also scan query params (e.g. ?file=setup.zip)
    if let Some(q) = lower.split('?').nth(1) {
        for part in q.split('&') {
            if let Some(v) = part.split('=').nth(1) {
                if let Some(dot) = v.rfind('.') {
                    let ext = &v[dot + 1..];
                    if EXTS.contains(&ext) {
                        return true;
                    }
                }
            }
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

pub(crate) fn resolve_filename(resp: &reqwest::Response, url: &str) -> String {
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

pub(crate) fn percent_decode(s: &str) -> String {
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

pub(crate) fn parse_total(resp: &reqwest::Response) -> u64 {
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
        let status = *self.status.read().unwrap_or_else(|e| e.into_inner());
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
            error: self.error.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            thumbnail: self.thumbnail.clone(),
            source: self.source.clone(),
            save_path: self.save_path.display().to_string(),
            created_at: self.created_at,
            completed_at: *self.completed_at.lock().unwrap_or_else(|e| e.into_inner()),
            format_id: None,
            produced: Vec::new(),
        }
    }

    fn connections(&self) -> usize {
        if self.source == "youtube" {
            1
        } else {
            self.max_conns.load(Ordering::Relaxed).max(1)
        }
    }

    fn peek_speed(&self) -> u64 {
        let h = self.history.lock().unwrap();
        if h.len() < 2 {
            return 0;
        }
        let (t0, b0) = *h.front().unwrap();
        let (t1, b1) = *h.back().unwrap();
        let dt = t1.duration_since(t0).as_secs_f64().max(0.05);
        (b1.saturating_sub(b0) as f64 / dt) as u64
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

    pub fn note_congestion(&self) {
        let cur = self.max_conns.load(Ordering::Relaxed);
        let next = (cur / 2).max(4);
        if next < cur {
            self.max_conns.store(next, Ordering::Relaxed);
            eprintln!("[vortex-net] congestion (429/503): connections {cur} -> {next}");
        }
        self.penalty_until.store(now_ms() + 30_000, Ordering::Relaxed);
    }

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

    if crate::ftp::is_ftp_url(&url) {
        return start_ftp(app, url, base, opts, limit).await;
    }

    let settings = crate::state::load_settings(&app);
    let eff_proxy = crate::state::proxy_for_url(&settings.per_site_proxies, &opts.proxy, &url);
    let extra = opts.extra_headers();
    let mut client = if extra.is_empty() {
        build_client(&eff_proxy)?
    } else {
        build_client_with_headers(&eff_proxy, &extra)?
    };

    let mut auth = auth::find_cred(&settings.credentials, &url).map(|cred| AuthCtx {
        hdr: auth::basic_auth_value(&cred.username, &cred.password),
        cred,
    });

    let probe = match send_authorized(&client, &url, Some("bytes=0-0"), &mut auth).await {
        Ok((p, a)) => {
            auth = a;
            p
        }
        Err(e)
            if e.is_redirect()
                || e.is_request()
                || e.is_connect()
                || e.is_body()
                || e.is_decode()
                || e.is_timeout() =>
        {
            log_net_err(&e, "probe failed; retrying with tool UA");
            client = if extra.is_empty() {
                build_tool_client(&eff_proxy)?
            } else {
                build_tool_client_with_headers(&eff_proxy, &extra)?
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

    {
        let st = probe.status();
        if (st.is_client_error() || st.is_server_error())
            && st != StatusCode::UNAUTHORIZED
            && st != StatusCode::PROXY_AUTHENTICATION_REQUIRED
        {
            return Err(format!("Server refused the download (HTTP {})", st.as_u16()));
        }
    }

    let accept_ranges = probe
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok().map(|s| s.to_lowercase().contains("bytes")))
        .unwrap_or(false);

    let final_url = probe.url().to_string();
    if final_url != url {
        eprintln!("[vortex-net] url resolved: {url} -> {final_url}");
        url = final_url;
    }

    let got_206 = probe.status() == StatusCode::PARTIAL_CONTENT;
    let total = parse_total(&probe);
    if total > MAX_DOWNLOAD_BYTES {
        return Err(format!("Server claims an absurd file size ({total} bytes) — refused"));
    }

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

    let candidate = eff_dir.join(&name);
    let save_path_final = match opts.on_exists.as_deref() {
        Some("prompt") if candidate.exists() => {
            return Err(format!("EXISTS::{}", candidate.display()));
        }
        Some("replace") => {
            if candidate.exists() {
                let _ = fs::remove_file(&candidate);
                crate::state::cleanup_parts_for(&candidate);
            }
            candidate
        }
        _ => unique_path(&eff_dir, &name),
    };

    let id = uuid::Uuid::new_v4().to_string();

    // Isolated Temporary Staging Directory for parts (No white files in Downloads!)
    let parts_staging_dir = std::env::temp_dir().join("vortex").join("parts").join(&id);
    let _ = fs::create_dir_all(&parts_staging_dir);

    // Auto connection resolution while strictly preserving Golden Speed Rules:
    let target_conns = if opts.segments == 0 {
        if total > 250 * 1024 * 1024 { 32 } else { 16 }
    } else {
        opts.segments
    };

    let segs;
    let max_conns;
    if ranged && total > 0 && target_conns > 1 {
        const MB: u64 = 1024 * 1024;
        if total < 10 * MB {
            max_conns = 1;
            segs = vec![Segment { start: 0, end: total.saturating_sub(1), part: part_of(&parts_staging_dir, 0) }];
        } else if total <= 250 * MB {
            max_conns = target_conns.clamp(2, 32).min(8);
            segs = split_range_contiguous(total, max_conns as usize, &parts_staging_dir);
        } else {
            max_conns = target_conns.clamp(2, 32);
            segs = split_range(total, max_conns, &parts_staging_dir);
        }
    } else {
        max_conns = 1;
        segs = vec![Segment { start: 0, end: u64::MAX, part: part_of(&parts_staging_dir, 0) }];
    }
    let num_segments = segs.len();

    let mut done0 = 0u64;
    for s in &segs {
        if let Ok(m) = s.part.metadata() {
            done0 += m.len();
        }
    }

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
        split_at: Mutex::new(vec![u64::MAX; num_segments]),
        num_segments,
        max_conns: AtomicUsize::new(max_conns),
        penalty_until: AtomicU64::new(0),
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

async fn start_ftp(
    app: AppHandle,
    url: String,
    base: PathBuf,
    opts: StartOpts,
    limit: Arc<AtomicU64>,
) -> Result<Arc<Task>, String> {
    let info = crate::ftp::probe(&url).await.map_err(|e| format!("Connection failed: {e}"))?;
    let total = info.size.unwrap_or(0);
    if total > MAX_DOWNLOAD_BYTES {
        return Err(format!("Server claims an absurd file size ({total} bytes) — refused"));
    }

    let name = opts
        .filename
        .as_deref()
        .map(|f| sanitize(f))
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| crate::ftp::filename_of(&url));
    let category = category_of(&name).to_string();

    let eff_dir = crate::state::save_dir_for(&base, &name, opts.categorize);
    fs::create_dir_all(&eff_dir).map_err(|e| format!("Cannot create folder: {e}"))?;

    let candidate = eff_dir.join(&name);
    let save_path_final = match opts.on_exists.as_deref() {
        Some("prompt") if candidate.exists() => {
            return Err(format!("EXISTS::{}", candidate.display()));
        }
        Some("replace") => {
            if candidate.exists() {
                let _ = fs::remove_file(&candidate);
                crate::state::cleanup_parts_for(&candidate);
            }
            candidate
        }
        _ => unique_path(&eff_dir, &name),
    };

    let id = uuid::Uuid::new_v4().to_string();
    let parts_staging_dir = std::env::temp_dir().join("vortex").join("parts").join(&id);
    let _ = fs::create_dir_all(&parts_staging_dir);

    let (segs, max_conns) = if info.resume && total > 0 && opts.segments > 1 {
        let mc = opts.segments.clamp(2, 32);
        (split_range(total, mc, &parts_staging_dir), mc)
    } else if total > 0 {
        (vec![Segment { start: 0, end: total - 1, part: part_of(&parts_staging_dir, 0) }], 1)
    } else {
        (vec![Segment { start: 0, end: u64::MAX, part: part_of(&parts_staging_dir, 0) }], 1)
    };
    let num_segments = segs.len();

    let mut done0 = 0u64;
    for s in &segs {
        if let Ok(m) = s.part.metadata() {
            done0 += m.len();
        }
    }

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
        source: "ftp".into(),
        thumbnail: None,
        created_at: now_ms(),
        total: AtomicU64::new(total),
        done: AtomicU64::new(done0),
        segments: Mutex::new(segs),
        done_flags: Mutex::new(vec![false; num_segments]),
        claimed: Mutex::new(vec![false; num_segments]),
        split_at: Mutex::new(vec![u64::MAX; num_segments]),
        num_segments,
        max_conns: AtomicUsize::new(max_conns),
        penalty_until: AtomicU64::new(0),
        live: AtomicUsize::new(0),
        status: RwLock::new(DlStatus::Queued),
        error: Mutex::new(None),
        cancel: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        history: Mutex::new(VecDeque::new()),
        limit,
        client: build_client("").unwrap_or_else(|_| Client::new()),
        retries: AtomicU64::new(0),
        auto_retries: opts.auto_retries,
        start_at: opts.start_at,
        auth: Mutex::new(None),
        app,
        completed_at: Mutex::new(None),
    });

    Ok(task)
}

pub fn restore(
    app: AppHandle,
    view: DlView,
    segments: Vec<[u64; 2]>,
    limit: Arc<AtomicU64>,
    auto_retries: u32,
    start_at: Option<u64>,
) -> Result<Arc<Task>, String> {
    let save_path = PathBuf::from(view.save_path.clone());
    let num_segments = segments.len();
    let parts_staging_dir = std::env::temp_dir().join("vortex").join("parts").join(&view.id);
    let _ = fs::create_dir_all(&parts_staging_dir);
    let segs = segments
        .iter()
        .enumerate()
        .map(|(i, [s, e])| Segment { start: *s, end: *e, part: part_of(&parts_staging_dir, i) })
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

    let mut disk_done = 0u64;
    for s in &segs {
        if let Ok(m) = s.part.metadata() {
            disk_done += m.len();
        }
    }
    let synced_downloaded = if disk_done > 0 { disk_done.min(view.downloaded) } else { view.downloaded };

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
        done: AtomicU64::new(synced_downloaded),
        num_segments,
        max_conns: AtomicUsize::new(view.connections.clamp(1, 32)),
        penalty_until: AtomicU64::new(0),
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
        auto_retries,
        start_at,
        auth: Mutex::new(auth),
        segments: Mutex::new(segs),
        done_flags: Mutex::new(vec![false; num_segments]),
        claimed: Mutex::new(vec![false; num_segments]),
        split_at: Mutex::new(vec![u64::MAX; num_segments]),
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
        TaskSnapshot { view: self.view(), segments: segs, auto_retries: self.auto_retries, start_at: self.start_at }
    }
}

pub async fn run(task: Arc<Task>) {
    task.push_history();

    if let Some(ta) = task.start_at {
        let now = now_ms();
        if now < ta {
            task.set_status(DlStatus::Queued);
            while !task.cancel.load(Ordering::Relaxed) && now_ms() < ta {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if task.cancel.load(Ordering::Relaxed) {
                task.set_status(DlStatus::Cancelled);
                return;
            }
        }
    }

    task.set_status(DlStatus::Downloading);
    let monitor = tokio::spawn(monitor_task(task.clone()));
    let mut last_round_done = task.done.load(Ordering::Relaxed);

    loop {
        if task.cancel.load(Ordering::Relaxed) {
            task.set_status(DlStatus::Cancelled);
            break;
        }
        if task.paused.load(Ordering::Relaxed) {
            task.set_status(DlStatus::Paused);
            break;
        }

        reset_claims(&task);

        let mut workers = Vec::new();
        for _ in 0..task.max_conns.load(Ordering::Relaxed).max(1) {
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
            finalize(&task).await;
            break;
        }

        if all_ok {
            task.retries.store(0, Ordering::Relaxed);
            continue;
        }

        if *task.status.read().unwrap() == DlStatus::NeedsAuth {
            break;
        }

        let round_done = task.done.load(Ordering::Relaxed);
        if round_done > last_round_done {
            task.retries.store(0, Ordering::Relaxed);
        }
        last_round_done = round_done;

        let r = task.retries.fetch_add(1, Ordering::Relaxed) + 1;
        if r > task.auto_retries.max(1) as u64 {
            task.set_error("Too many consecutive failures");
            task.set_status(DlStatus::Error);
            break;
        }
        tokio::time::sleep(retry_backoff(r)).await;
    }

    monitor.abort();
}

async fn worker_loop(task: Arc<Task>) -> bool {
    loop {
        if task.cancel.load(Ordering::Relaxed) || task.paused.load(Ordering::Relaxed) {
            return true;
        }
        let (idx, seg) = match claim_chunk(&task) {
            Some(c) => c,
            None => match try_steal(&task) {
                Some(c) => c,
                None => {
                    if all_chunks_done(&task) {
                        return true;
                    }
                    if task.live.load(Ordering::Relaxed) > 1 {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        continue;
                    }
                    return true;
                }
            },
        };
        if !download_segment(task.clone(), seg, idx).await {
            return false;
        }
    }
}

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

// Golden Rule: strictly 8MB steal threshold to avoid high-latency request loops!
const MIN_SPLIT_REMAINING: u64 = 8 * 1024 * 1024;

fn split_cut(vstart: u64, vend: u64, written: u64) -> Option<u64> {
    if vend < vstart {
        return None;
    }
    let total = vend - vstart + 1;
    let have = written.min(total);
    let remaining = total - have;
    if remaining < MIN_SPLIT_REMAINING {
        return None;
    }
    let done_upto = vstart + have;
    if done_upto > vend {
        return None;
    }
    let cut = done_upto + (vend - done_upto) / 2;
    if cut <= done_upto || cut >= vend {
        return None;
    }
    Some(cut)
}

fn try_steal(task: &Task) -> Option<(usize, Segment)> {
    if task.live.load(Ordering::Relaxed) < 2 {
        return None;
    }
    if crate::ftp::is_ftp_url(&task.url) {
        return None;
    }
    let mut segs = task.segments.lock().unwrap();
    let mut done_flags = task.done_flags.lock().unwrap();
    let mut claimed = task.claimed.lock().unwrap();
    let mut split_at = task.split_at.lock().unwrap();

    let mut victim: Option<(usize, u64)> = None;
    for (i, seg) in segs.iter().enumerate() {
        if done_flags.get(i).copied().unwrap_or(true) {
            continue;
        }
        if !claimed.get(i).copied().unwrap_or(false) {
            continue;
        }
        if seg.end == u64::MAX {
            continue;
        }
        if split_at.get(i).copied().unwrap_or(0) != u64::MAX {
            continue;
        }
        let written = seg.part.metadata().map(|m| m.len()).unwrap_or(0);
        let have = written.min(seg.end - seg.start + 1);
        let remaining = (seg.end - seg.start + 1) - have;
        if remaining < MIN_SPLIT_REMAINING {
            continue;
        }
        if victim.map(|(_, r)| remaining > r).unwrap_or(true) {
            victim = Some((i, remaining));
        }
    }
    let (idx, _) = victim?;

    let vstart = segs[idx].start;
    let vend = segs[idx].end;
    let written = segs[idx].part.metadata().map(|m| m.len()).unwrap_or(0);
    let cut = match split_cut(vstart, vend, written) {
        Some(c) => c,
        None => return None,
    };

    let new_idx = segs.len();
    let parts_staging_dir = std::env::temp_dir().join("vortex").join("parts").join(&task.id);
    let new_seg = Segment {
        start: cut + 1,
        end: vend,
        part: part_of(&parts_staging_dir, new_idx),
    };
    segs[idx].end = cut;
    segs.push(new_seg.clone());
    done_flags.push(false);
    claimed.push(true);
    split_at[idx] = cut;
    split_at.push(u64::MAX);
    Some((new_idx, new_seg))
}

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

async fn finalize(task: &Arc<Task>) {
    let t = task.clone();
    let r = tokio::task::spawn_blocking(move || {
        finalize_blocking(&t);
    })
    .await;
    if r.is_err() {
        task.set_error("Merge task failed");
        task.set_status(DlStatus::Error);
    }
}

fn finalize_blocking(task: &Arc<Task>) {
    task.set_status(DlStatus::Merging);
    let parts_staging_dir = std::env::temp_dir().join("vortex").join("parts").join(&task.id);
    let mut parts: Vec<PathBuf> = {
        let mut segs = task.segments.lock().unwrap().clone();
        segs.retain(|s| s.end != u64::MAX);
        segs.sort_by_key(|s| s.start);
        segs.into_iter().map(|s| s.part).collect()
    };
    if parts.is_empty() {
        parts = vec![part_of(&parts_staging_dir, 0)];
    }

    if parts.len() == 1 {
        let moved = fs::rename(&parts[0], &task.save_path).is_ok() || copy_one(&parts[0], &task.save_path);
        if moved {
            let _ = fs::remove_file(&parts[0]);
            finish_ok(task);
        } else {
            task.set_error("Failed to merge parts");
            task.set_status(DlStatus::Error);
        }
        return;
    }
    let out = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&task.save_path);
    if let Ok(mut f) = out {
        let total = task.total.load(Ordering::Relaxed);
        if total > 0 {
            let _ = f.set_len(total);
        }
        let mut ok = true;
        for p in &parts {
            match fs::File::open(p) {
                Ok(src) => {
                    let mut r = std::io::BufReader::with_capacity(4 * 1024 * 1024, src);
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
            finish_ok(task);
        } else {
            task.set_error("Failed to merge parts");
            task.set_status(DlStatus::Error);
        }
    }
}

fn copy_one(src: &Path, dst: &Path) -> bool {
    let out = OpenOptions::new().create(true).truncate(true).write(true).open(dst);
    if let (Ok(s), Ok(mut d)) = (fs::File::open(src), out) {
        let mut r = std::io::BufReader::with_capacity(4 * 1024 * 1024, s);
        if std::io::copy(&mut r, &mut d).is_ok() {
            let _ = d.flush();
            return true;
        }
    }
    false
}

fn finish_ok(task: &Arc<Task>) {
    cleanup_parts(task.clone());
    let sz = fs::metadata(&task.save_path).map(|m| m.len()).unwrap_or(0);
    task.total.store(sz.max(task.total.load(Ordering::Relaxed)), Ordering::Relaxed);
    task.done.store(sz, Ordering::Relaxed);
    task.set_status(DlStatus::Completed);
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
    if crate::ftp::is_ftp_url(&task.url) {
        return download_segment_ftp(task, seg, idx).await;
    }
    let expected = if seg.end == u64::MAX { u64::MAX } else { seg.end - seg.start + 1 };
    if seg.end != u64::MAX && seg.part.metadata().map(|m| m.len()).unwrap_or(0) >= expected {
        mark_done(&task, idx);
        return true;
    }

    let limit = task.limit.load(Ordering::Relaxed);
    let max_conns = task.max_conns.load(Ordering::Relaxed).max(1) as u64;
    let per_conn = if limit > 0 { limit / max_conns } else { 0 };

    let std_file = match OpenOptions::new().create(true).append(true).open(&seg.part) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let offset = std_file.metadata().map(|m| m.len()).unwrap_or(0);
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
    let mut attempts: u64 = 0;
    let mut since_check: usize = 0; // Throttle lock contention

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
                attempts += 1;
                if attempts > MAX_CHUNK_ATTEMPTS {
                    task.set_error(&format!("Connection failed ({attempts} tries): {e}"));
                    return false;
                }
                tokio::time::sleep(retry_backoff(attempts)).await;
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
        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
            task.note_congestion();
            attempts += 1;
            if attempts > MAX_CHUNK_ATTEMPTS {
                task.set_error(&format!("HTTP {} (server keeps failing)", status.as_u16()));
                return false;
            }
            let wait = retry_after_secs(&resp)
                .map(Duration::from_secs)
                .unwrap_or_else(|| retry_backoff(attempts));
            eprintln!("[vortex-net] segment {idx}: HTTP {} — waiting {}s", status.as_u16(), wait.as_secs());
            tokio::time::sleep(wait).await;
            continue;
        }
        if status.is_server_error() {
            attempts += 1;
            if attempts > MAX_CHUNK_ATTEMPTS {
                task.set_error(&format!("HTTP {} (server keeps failing)", status.as_u16()));
                return false;
            }
            let wait = retry_after_secs(&resp)
                .map(Duration::from_secs)
                .unwrap_or_else(|| retry_backoff(attempts));
            eprintln!("[vortex-net] segment {idx}: HTTP {} — waiting {}s", status.as_u16(), wait.as_secs());
            tokio::time::sleep(wait).await;
            continue;
        }
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            let expected = seg.end - seg.start + 1;
            if seg.end != u64::MAX
                && seg.part.metadata().map(|m| m.len()).unwrap_or(0) >= expected
            {
                mark_done(&task, idx);
                return true;
            }
            task.set_error("HTTP 416 (range past end of file)");
            return false;
        }
        if is_fatal_status(status) {
            task.set_error(&format!("HTTP {} (not retryable)", status.as_u16()));
            return false;
        }
        if !status.is_success() && status != StatusCode::PARTIAL_CONTENT {
            task.set_error(&format!("HTTP {}", status.as_u16()));
            return false;
        }
        if seg.end != u64::MAX && status != StatusCode::PARTIAL_CONTENT {
            task.set_error("Server ignored range request");
            return false;
        }

        let mut stream = resp.bytes_stream();
        let mut dropped = false;
        loop {
            let chunk = match tokio::time::timeout(STALL_TIMEOUT, stream.next()).await {
                Ok(c) => c,
                Err(_) => {
                    eprintln!("[vortex-net] segment {idx}: stalled, reconnecting at {cursor}");
                    dropped = true;
                    break;
                }
            };
            let Some(chunk) = chunk else { break };
            match chunk {
                Ok(c) => {
                    if task.cancel.load(Ordering::Relaxed) || task.paused.load(Ordering::Relaxed) {
                        break;
                    }
                    if c.is_empty() {
                        continue;
                    }
                    if file.write_all(&c).await.is_err() {
                        let _ = file.flush().await;
                        return false;
                    }
                    attempts = 0;
                    task.done.fetch_add(c.len() as u64, Ordering::Relaxed);
                    cursor += c.len() as u64;

                    if seg.end != u64::MAX && cursor >= seg.end + 1 {
                        break;
                    }

                    // Throttled lock check: only check split_at every 512KB written to stop 16-thread lock contention!
                    since_check += c.len();
                    if since_check >= 512 * 1024 {
                        since_check = 0;
                        let cut = task.split_at.lock().unwrap().get(idx).copied().unwrap_or(u64::MAX);
                        if cut != u64::MAX && cut >= seg.start && cursor >= cut.saturating_add(1) {
                            let want = cut - seg.start + 1;
                            let _ = file.flush().await;
                            let have = seg.part.metadata().map(|m| m.len()).unwrap_or(0);
                            if have == want {
                                mark_done(&task, idx);
                                let _ = file.flush().await;
                                return true;
                            }
                            if have > want {
                                let cut_ok = std::fs::OpenOptions::new()
                                    .write(true)
                                    .open(&seg.part)
                                    .and_then(|f| f.set_len(want))
                                    .is_ok();
                                let _ = file.flush().await;
                                if cut_ok {
                                    task.done.fetch_sub(have - want, Ordering::Relaxed);
                                    mark_done(&task, idx);
                                    return true;
                                }
                                let _ = std::fs::remove_file(&seg.part);
                                return false;
                            }
                            return false;
                        }
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

        // Done only when the range ACTUALLY landed. A graceful EOF (server
        // closing the socket, !dropped) does NOT imply completion: if the
        // stream ended before seg.end was reached the chunk is incomplete, and
        // marking it done would truncate/corrupt the merged file. Incomplete =
        // retry from the cursor with backoff below; unknown-size segments end
        // naturally at a clean EOF.
        let complete = (seg.end == u64::MAX && !dropped) || (seg.end != u64::MAX && cursor >= seg.end + 1);
        if complete {
            mark_done(&task, idx);
            return true;
        }
        attempts += 1;
        if attempts > MAX_CHUNK_ATTEMPTS {
            task.set_error("Connection keeps dropping (chunk gave up)");
            return false;
        }
        tokio::time::sleep(retry_backoff(attempts)).await;
    }

    true
}

async fn download_segment_ftp(task: Arc<Task>, seg: Segment, idx: usize) -> bool {
    let expected = if seg.end == u64::MAX { u64::MAX } else { seg.end - seg.start + 1 };
    if seg.end != u64::MAX && seg.part.metadata().map(|m| m.len()).unwrap_or(0) >= expected {
        mark_done(&task, idx);
        return true;
    }

    let limit = task.limit.load(Ordering::Relaxed);
    let max_conns = task.max_conns.load(Ordering::Relaxed).max(1) as u64;
    let per_conn = if limit > 0 { limit / max_conns } else { 0 };

    let std_file = match OpenOptions::new().create(true).append(true).open(&seg.part) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut cursor = seg.start + std_file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut file = tokio::io::BufWriter::with_capacity(1024 * 1024, tokio::fs::File::from_std(std_file));
    if seg.end != u64::MAX && cursor >= seg.end + 1 {
        mark_done(&task, idx);
        return true;
    }

    let mut last_throttle = Instant::now();
    let mut throttle_bytes = 0u64;
    let mut attempts: u64 = 0;
    let mut no_resume = false;

    while !task.cancel.load(Ordering::Relaxed) && !task.paused.load(Ordering::Relaxed) {
        if no_resume && cursor > seg.start {
            no_resume = false;
            drop(file);
            let lost = cursor - seg.start;
            let _ = fs::remove_file(&seg.part);
            let f = match OpenOptions::new().create(true).append(true).open(&seg.part) {
                Ok(f) => f,
                Err(_) => return false,
            };
            task.done.fetch_sub(lost, Ordering::Relaxed);
            cursor = seg.start;
            file = tokio::io::BufWriter::with_capacity(1024 * 1024, tokio::fs::File::from_std(f));
        }

        let mut xf = match crate::ftp::open_transfer(&task.url, cursor - seg.start).await {
            Ok(x) => x,
            Err(e) => {
                no_resume = e.contains(crate::ftp::NO_RESUME);
                eprintln!("[vortex-net] ftp segment {idx} open: {e}");
                attempts += 1;
                if attempts > MAX_CHUNK_ATTEMPTS {
                    task.set_error(&format!("FTP: {e}"));
                    return false;
                }
                tokio::time::sleep(retry_backoff(attempts)).await;
                continue;
            }
        };

        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if task.cancel.load(Ordering::Relaxed) || task.paused.load(Ordering::Relaxed) {
                break;
            }
            let n = match tokio::time::timeout(STALL_TIMEOUT, xf.data.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => n,
                Ok(Ok(_)) => {
                    if seg.end != u64::MAX && cursor < seg.end + 1 {
                        attempts += 1;
                    }
                    break;
                }
                Ok(Err(e)) => {
                    eprintln!("[vortex-net] ftp segment {idx} read: {e}");
                    attempts += 1;
                    break;
                }
                Err(_) => {
                    eprintln!("[vortex-net] ftp segment {idx}: stalled, reconnecting at {cursor}");
                    attempts += 1;
                    break;
                }
            };
            if file.write_all(&buf[..n]).await.is_err() {
                let _ = file.flush().await;
                return false;
            }
            attempts = 0;
            task.done.fetch_add(n as u64, Ordering::Relaxed);
            cursor += n as u64;

            if seg.end != u64::MAX && cursor >= seg.end + 1 {
                break;
            }

            if per_conn > 0 {
                throttle_bytes += n as u64;
                let since = last_throttle.elapsed().as_secs_f64();
                if throttle_bytes as f64 > per_conn as f64 * since && since > 0.0 {
                    let excess = throttle_bytes as f64 - per_conn as f64 * since;
                    tokio::time::sleep(Duration::from_secs_f64(excess / per_conn as f64)).await;
                    last_throttle = Instant::now();
                    throttle_bytes = 0;
                }
            }
        }
        let _ = file.flush().await;

        if task.cancel.load(Ordering::Relaxed) || task.paused.load(Ordering::Relaxed) {
            break;
        }

        let range_complete = seg.end == u64::MAX || cursor >= seg.end + 1;
        if range_complete {
            match xf.finish().await {
                Ok(()) => {
                    mark_done(&task, idx);
                    return true;
                }
                Err(e) => {
                    eprintln!("[vortex-net] ftp segment {idx} finish: {e}");
                    attempts += 1;
                    if attempts > MAX_CHUNK_ATTEMPTS {
                        task.set_error(&format!("FTP: {e}"));
                        return false;
                    }
                    tokio::time::sleep(retry_backoff(attempts)).await;
                    continue;
                }
            }
        }

        attempts += 1;
        if attempts > MAX_CHUNK_ATTEMPTS {
            task.set_error("FTP connection keeps dropping (chunk gave up)");
            return false;
        }
        tokio::time::sleep(retry_backoff(attempts)).await;
    }

    true
}

fn should_grow(prev_bps: f64, recent_bps: f64, max_conns: usize, remaining: u64) -> bool {
    if max_conns >= 32 || remaining < 32 * 1024 * 1024 {
        return false;
    }
    if prev_bps <= 0.0 {
        return recent_bps > 0.0;
    }
    recent_bps >= prev_bps * 0.9
}

async fn monitor_task(task: Arc<Task>) {
    let mut last = Instant::now();
    let mut last_done = task.done.load(Ordering::Relaxed);
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut sent_done = u64::MAX;
    let mut win_bytes = 0u64;
    let mut win_ticks = 0u32;
    let mut prev_bps = 0.0f64;
    let mut ema_speed: f64 = 0.0;
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let now = task.done.load(Ordering::Relaxed);
        let t = Instant::now();
        {
            let mut h = task.history.lock().unwrap();
            h.push_back((t, now));
            while h.front().map(|(t0, _)| t.duration_since(*t0) > Duration::from_secs(3)).unwrap_or(false) {
                h.pop_front();
            }
        }
        let dt = t.duration_since(last).as_secs_f64().max(0.05);
        let tick_bytes = now.saturating_sub(last_done);
        let raw = tick_bytes as f64 / dt;
        ema_speed = if ema_speed == 0.0 { raw } else { raw * 0.3 + ema_speed * 0.7 };
        let speed = ema_speed as u64;
        last = t;
        last_done = now;
        let total = task.total.load(Ordering::Relaxed);
        let progress = if total > 0 { (now as f64 / total as f64) * 100.0 } else { 0.0 };
        let eta = if speed > 0 && total > now { (total - now) / speed } else { 0 };
        if now != sent_done || last_emit.elapsed() >= Duration::from_secs(1) {
            last_emit = Instant::now();
            sent_done = now;
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
        }

        win_bytes += tick_bytes;
        win_ticks += 1;
        if win_ticks >= 20 {
            let recent_bps = win_bytes as f64 / 5.0;
            let remaining = total.saturating_sub(now);
            let cur = task.max_conns.load(Ordering::Relaxed);
            let penalized = now_ms() < task.penalty_until.load(Ordering::Relaxed);
            if !penalized && should_grow(prev_bps, recent_bps, cur, remaining) {
                task.max_conns.store((cur + 4).min(32), Ordering::Relaxed);
            }
            prev_bps = recent_bps;
            win_bytes = 0;
            win_ticks = 0;
        }

        let st = *task.status.read().unwrap();
        if st != DlStatus::Downloading && st != DlStatus::Merging {
            break;
        }
    }
}

fn split_range(total: u64, connections: usize, save_path: &Path) -> Vec<Segment> {
    const MB: u64 = 1024 * 1024;
    if total < 10 * MB {
        let mut out = Vec::new();
        out.push(Segment { start: 0, end: total.saturating_sub(1), part: part_of(save_path, 0) });
        return out;
    }
    let conns = connections.max(1) as u64;
    let max_chunks = 1024u64;
    let (min_chunk, max_chunk) = if total <= 100 * MB {
        (8 * MB, 12 * MB + 512 * 1024)
    } else if total <= 1024 * MB {
        (16 * MB, 32 * MB)
    } else {
        (32 * MB, 64 * MB)
    };
    let tier_chunk = (total.div_ceil(conns * 2)).clamp(min_chunk, max_chunk);
    let required = total.div_ceil(conns * 2);
    let mut chunk = tier_chunk.max(required);
    if total.div_ceil(chunk) > max_chunks {
        chunk = total.div_ceil(max_chunks);
    }
    let mut out = Vec::new();
    let mut start = 0u64;
    let mut i = 0usize;
    while start < total {
        let end = start.saturating_add(chunk).saturating_sub(1).min(total.saturating_sub(1));
        out.push(Segment { start, end, part: part_of(save_path, i) });
        start = end.saturating_add(1);
        i += 1;
        if out.len() as u64 > max_chunks + 1 {
            break;
        }
    }
    out
}

fn split_range_contiguous(total: u64, connections: usize, save_path: &Path) -> Vec<Segment> {
    let n = connections.max(1);
    let base = total / n as u64;
    let rem = total % n as u64;
    let mut out = Vec::with_capacity(n);
    let mut start = 0u64;
    for i in 0..n {
        let extra = if (i as u64) < rem { 1 } else { 0 };
        let len = base + extra;
        let end = start.saturating_add(len).saturating_sub(1).min(total.saturating_sub(1));
        out.push(Segment { start, end, part: part_of(save_path, i) });
        start = end.saturating_add(1);
        if start >= total {
            break;
        }
    }
    out
}

fn part_of(staging_dir: &Path, index: usize) -> PathBuf {
    staging_dir.join(format!("{index}{PART_EXT}"))
}

fn cleanup_parts(task: Arc<Task>) {
    let parts_staging_dir = std::env::temp_dir().join("vortex").join("parts").join(&task.id);
    let _ = fs::remove_dir_all(&parts_staging_dir);
}

#[cfg(test)]
mod step_tests {
    use super::*;

    #[test]
    fn split_cut_partitions_exactly() {
        for (vs, ve, w) in [
            (0u64, 32 * 1024 * 1024 - 1, 0u64),
            (0, 32 * 1024 * 1024 - 1, 16 * 1024 * 1024),
            (0, 32 * 1024 * 1024 - 1, 20 * 1024 * 1024),
            (100 * 1024 * 1024, 132 * 1024 * 1024 - 1, 5 * 1024 * 1024),
            (0, 16 * 1024 * 1024 - 1, 4 * 1024 * 1024),
        ] {
            let cut = split_cut(vs, ve, w).expect("should split");
            assert!(cut >= vs && cut < ve);
            assert_eq!((cut - vs + 1) + (ve - cut), ve - vs + 1);
            assert!(ve - cut >= 1024 * 1024, "steal half too small");
        }
    }

    #[test]
    fn split_cut_refuses_gracefully() {
        assert!(split_cut(0, 32 * 1024 * 1024 - 1, 31 * 1024 * 1024).is_none());
        assert!(split_cut(0, 0, 0).is_none());
        assert!(split_cut(0, 100, 0).is_none());
        assert!(split_cut(5, 4, 0).is_none());
        assert!(split_cut(0, 1024 * 1024 * 8, 1024 * 1024 * 8).is_none());
        assert!(split_cut(0, 8 * 1024 * 1024, u64::MAX).is_none());
    }

    #[test]
    fn backoff_grows_and_caps() {
        let ms = |a: u64| retry_backoff(a).as_millis() as u64;
        let bases = [1000u64, 2000, 4000, 8000, 16000, 30000, 30000];
        for (i, base) in bases.iter().enumerate() {
            for _ in 0..10 {
                let m = ms(i as u64 + 1);
                assert!(m >= base * 75 / 100 && m <= base * 125 / 100 + 1, "a={} m={m}", i + 1);
            }
        }
        assert!(ms(0) >= 750);
        assert!(ms(u64::MAX) <= 30000 * 125 / 100 + 1);
    }

    #[test]
    fn fatal_status_table() {
        for code in [400u16, 404, 405, 410, 416, 501] {
            assert!(is_fatal_status(StatusCode::from_u16(code).unwrap()), "{code} should be fatal");
        }
        for code in [200u16, 206, 403, 408, 429, 500, 502, 503] {
            assert!(!is_fatal_status(StatusCode::from_u16(code).unwrap()), "{code} should be transient");
        }
    }

    #[test]
    fn should_grow_table() {
        assert!(!should_grow(1e6, 1e6, 32, 1 << 30));
        assert!(!should_grow(1e6, 1e6, 8, 1024));
        assert!(!should_grow(1e6, 0.0, 8, 1 << 30));
        assert!(!should_grow(1e6, 5e5, 8, 1 << 30));
        assert!(should_grow(1e6, 1.2e6, 8, 1 << 30));
        assert!(should_grow(1e6, 0.95e6, 8, 1 << 30));
        assert!(should_grow(0.0, 1e5, 8, 1 << 30));
        assert!(should_grow(0.0, 0.0, 8, 1 << 30) == false);
        assert!(should_grow(1e6, 1e6, 31, 32 * 1024 * 1024));
    }

    #[test]
    fn split_range_tiles_without_gaps() {
        let dir = std::env::temp_dir();
        for total in [1u64, 1024, 1024 * 1024, 40 * 1024 * 1024 + 7, 1024 * 1024 * 1024, u64::MAX] {
            let chunks = split_range(total, 8, &dir.join("probe.bin"));
            assert!(!chunks.is_empty());
            let mut cursor = 0u64;
            for c in &chunks {
                assert_eq!(c.start, cursor, "gap/overlap at total={total}");
                cursor = c.end + 1;
            }
            assert_eq!(cursor, total, "tail loss at total={total}");
        }
    }
}