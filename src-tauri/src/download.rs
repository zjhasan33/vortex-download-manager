use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use reqwest::header::{ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use reqwest::{Client, Proxy, StatusCode};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

const PART_EXT: &str = ".vtx.part";

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
    pub status: DlStatus,
    pub error: Option<String>,
    pub thumbnail: Option<String>,
    pub source: String,
    pub save_path: String,
    pub created_at: u64,
    #[serde(default)]
    pub format_id: Option<String>,
}

#[derive(Clone)]
struct Segment {
    start: u64,
    end: u64,
    part: PathBuf,
}

#[derive(Clone)]
pub struct StartOpts {
    pub segments: usize,
    pub filename: Option<String>,
    pub categorize: bool,
    pub start_at: Option<u64>,
    pub auto_retries: u32,
    pub proxy: String,
}

impl Default for StartOpts {
    fn default() -> Self {
        StartOpts {
            segments: 8,
            filename: None,
            categorize: true,
            start_at: None,
            auto_retries: 3,
            proxy: String::new(),
        }
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
    segments: Mutex<Vec<Segment>>,
    done_flags: Mutex<Vec<bool>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
            status,
            error: self.error.lock().unwrap().clone(),
            thumbnail: self.thumbnail.clone(),
            source: self.source.clone(),
            save_path: self.save_path.display().to_string(),
            created_at: self.created_at,
            format_id: None,
        }
    }

    fn connections(&self) -> usize {
        if self.source == "youtube" { 1 } else { self.num_segments.max(1) }
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
}

pub async fn start(
    app: AppHandle,
    url: String,
    save_path: String,
    opts: StartOpts,
    limit: Arc<AtomicU64>,
) -> Result<Arc<Task>, String> {
    let base = PathBuf::from(save_path.trim());

    let mut cb = Client::builder();
    if !opts.proxy.trim().is_empty() {
        let proxy = Proxy::all(opts.proxy.trim()).map_err(|e| format!("Bad proxy: {e}"))?;
        cb = cb.proxy(proxy);
    }
    let client = cb.build().map_err(|e| format!("Client error: {e}"))?;

    let probe = client
        .get(&url)
        .header(RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|e| format!("Connection failed: {e}"))?;

    let accept_ranges = probe
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok().map(|s| s.to_lowercase() == "bytes"))
        .unwrap_or(false);

    let got_206 = probe.status() == StatusCode::PARTIAL_CONTENT;
    let total = if got_206 { parse_total(&probe) } else { 0 };

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
    if got_206 && total > 0 && accept_ranges && opts.segments > 1 {
        let n = opts.segments.clamp(1, 32);
        segs = split_range(total, n, &save_path_final);
    } else {
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
        num_segments,
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
        status: RwLock::new(status),
        error: Mutex::new(view.error.clone()),
        cancel: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        history: Mutex::new(VecDeque::new()),
        limit,
        app,
        client: Client::new(),
        retries: AtomicU64::new(0),
        auto_retries: 3,
        start_at: None,
        segments: Mutex::new(segs),
        done_flags: Mutex::new(vec![false; num_segments]),
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

        let segs = task.segments.lock().unwrap().clone();
        let mut workers = Vec::new();
        for (idx, seg) in segs.iter().enumerate() {
            if task.cancel.load(Ordering::Relaxed) || task.paused.load(Ordering::Relaxed) {
                break;
            }
            if segment_done(&task, seg, idx) {
                continue;
            }
            let t = task.clone();
            let s = seg.clone();
            workers.push(tokio::spawn(async move { download_segment(t, s, idx).await }));
            if segs.len() == 1 {
                break;
            }
        }

        if workers.is_empty() {
            // All segments already present; finalize.
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
                    match fs::read(p) {
                        Ok(data) => {
                            if std::io::Write::write_all(&mut f, &data).is_err() {
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
            break;
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

        if all_ok {
            // Loop again; remaining segments (if any) continue; when empty, finalize.
            task.retries.store(0, Ordering::Relaxed);
            continue;
        }

        // Some segment failed — retry after short backoff, capped by auto_retries.
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
    let per_conn = if limit > 0 { limit / task.num_segments.max(1) as u64 } else { 0 };

    let mut file = match OpenOptions::new().create(true).append(true).open(&seg.part) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let offset = file.metadata().map(|m| m.len()).unwrap_or(0);
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
        let resp = match client.get(&task.url).header(RANGE, range).send().await {
            Ok(r) => r,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(900)).await;
                continue;
            }
        };
        if !resp.status().is_success() && resp.status() != StatusCode::PARTIAL_CONTENT {
            task.set_error(&format!("HTTP {}", resp.status().as_u16()));
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
                    if std::io::Write::write_all(&mut file, &c).is_err() {
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
                Err(_) => {
                    dropped = true;
                    break;
                }
            }
        }
        let _ = file.flush();

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
            }),
        );

        let st = *task.status.read().unwrap();
        if st != DlStatus::Downloading && st != DlStatus::Merging {
            break;
        }
    }
}

fn split_range(total: u64, n: usize, save_path: &Path) -> Vec<Segment> {
    let mut chunk = total.div_ceil(n as u64);
    chunk = chunk.min(64 * 1024 * 1024).max(1024 * 1024); // 1MB min, 64MB max per segment
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