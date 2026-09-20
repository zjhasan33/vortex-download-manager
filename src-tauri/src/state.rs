use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::download::{self, DlStatus, DlView};

pub struct DlManager {
    pub http: Mutex<HashMap<String, Arc<crate::download::Task>>>,
    pub yt: Mutex<HashMap<String, Arc<crate::ytdlp::YtTask>>>,
    pub limit: Arc<AtomicU64>,
    /// Concurrency gate state.
    pub max_active: AtomicUsize,
    pub active: AtomicUsize,
    pub slot: tokio::sync::Notify,
    /// Terminal entries shown in the list (survive restart).
    pub history: Mutex<Vec<DlView>>,
    /// Stop flag for the Site Grabber crawl (set by the Stop button).
    pub grab_cancel: Arc<AtomicBool>,
}

impl Default for DlManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DlManager {
    pub fn new() -> Self {
        DlManager {
            http: Mutex::new(HashMap::new()),
            yt: Mutex::new(HashMap::new()),
            limit: Arc::new(AtomicU64::new(0)),
            max_active: AtomicUsize::new(5),
            active: AtomicUsize::new(0),
            slot: tokio::sync::Notify::new(),
            history: Mutex::new(Vec::new()),
            grab_cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn remove(&self, id: &str) {
        {
            let m = self.http.lock().unwrap();
            if let Some(t) = m.get(id) {
                t.cancel.store(true, Ordering::Relaxed);
                t.paused.store(false, Ordering::Relaxed);
            }
        }
        {
            let m = self.yt.lock().unwrap();
            if let Some(t) = m.get(id) {
                t.cancel.store(true, Ordering::Relaxed);
            }
        }
        self.http.lock().unwrap().remove(id);
        self.yt.lock().unwrap().remove(id);
        self.history.lock().unwrap().retain(|v| v.id != id);
    }

    pub fn push_history(&self, v: DlView) {
        self.history.lock().unwrap().push(v);
    }

    /// Remove a task, optionally also deleting its finished file from disk.
    /// With `delete_file`, multi-file jobs (playlists, subtitles) wipe every
    /// produced file, and an emptied playlist sub-folder shell goes too.
    /// Every caller funnels here, so no path can leak files anymore.
    pub fn remove_with_file(&self, id: &str, delete_file: bool) {
        if delete_file {
            let mut paths: Vec<PathBuf> = Vec::new();
            // Playlist shell to remove afterwards (only when left empty).
            let mut playlist_dir: Option<PathBuf> = None;
            if let Some(t) = self.http.lock().unwrap().get(id) {
                paths.push(t.save_path.clone());
            }
            if let Some(t) = self.yt.lock().unwrap().get(id) {
                let tp = t.save_path.lock().unwrap().clone();
                for p in t.produced.lock().unwrap().iter() {
                    paths.push(p.clone());
                }
                if tp.is_dir() {
                    // Never touch the user's base download dir itself — only a
                    // real yt-dlp sub-folder left behind by a playlist job.
                    let base = t.save_base.lock().unwrap().clone();
                    if tp != base {
                        playlist_dir = Some(tp);
                    }
                } else {
                    paths.push(tp);
                }
            }
            for h in self.history.lock().unwrap().iter() {
                if h.id == id {
                    paths.push(PathBuf::from(&h.save_path));
                    for p in &h.produced {
                        paths.push(PathBuf::from(p));
                    }
                }
            }
            for p in &paths {
                if p.is_file() {
                    let _ = std::fs::remove_file(p);
                }
                cleanup_parts_for(p);
            }
            if let Some(dir) = playlist_dir {
                let empty = std::fs::read_dir(&dir).map(|mut e| e.next().is_none()).unwrap_or(false);
                if empty {
                    let _ = std::fs::remove_dir(&dir);
                }
            }
        }
        self.remove(id);
    }

    pub fn views(&self) -> Vec<DlView> {
        // Poison-tolerant: one panicked holder must never cascade-crash every
        // list/stats command afterwards; recover with the guarded data.
        let mut v: Vec<DlView> = Vec::new();
        for t in self.http.lock().unwrap_or_else(|e| e.into_inner()).values() {
            v.push(t.view());
        }
        for t in self.yt.lock().unwrap_or_else(|e| e.into_inner()).values() {
            v.push(t.view());
        }
        for h in self.history.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            v.push(h.clone());
        }
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v.dedup_by(|a, b| a.id == b.id);
        v
    }

    pub fn stats(&self) -> serde_json::Value {
        let mut total_speed = 0u64;
        let mut active = 0u64;
        let mut completed = 0u64;
        let mut downloaded = 0u64;
        let mut segments = 0u64;
        let mut connections = 0u64;
        for v in self.views() {
            if v.status == DlStatus::Downloading
                || v.status == DlStatus::Merging
                || v.status == DlStatus::Resolving
            {
                total_speed += v.speed;
                active += 1;
                segments += v.segments as u64;
                connections += v.live as u64;
            }
            if v.status == DlStatus::Completed {
                completed += 1;
                downloaded += v.total_size;
            }
        }
        serde_json::json!({
            "total_speed": total_speed,
            "active": active,
            "completed": completed,
            "total_downloaded": downloaded,
            "segments": segments,
            "connections": connections,
        })
    }

    pub fn add_http(&self, t: Arc<crate::download::Task>) {
        self.http.lock().unwrap().insert(t.id.clone(), t);
    }

    pub fn add_yt(&self, t: Arc<crate::ytdlp::YtTask>) {
        self.yt.lock().unwrap().insert(t.id.clone(), t);
    }

    /// Claim a download slot, waiting while at the cap. Returns false when unlimited.
    pub async fn acquire_slot(&self) -> bool {
        let max = self.max_active.load(Ordering::Relaxed);
        if max == 0 {
            return false;
        }
        loop {
            let cur = self.active.load(Ordering::Relaxed);
            if cur < max
                && self
                    .active
                    .compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                return true;
            }
            let _ = tokio::time::timeout(std::time::Duration::from_millis(300), self.slot.notified()).await;
        }
    }

    pub fn release_slot(&self) {
        if self.active.fetch_sub(1, Ordering::SeqCst) > 0 {
            self.slot.notify_one();
        }
    }

    pub fn run_http(self: &Arc<DlManager>, t: Arc<crate::download::Task>) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let claimed = me.acquire_slot().await;
            crate::download::run(t).await;
            if claimed {
                me.release_slot();
            }
        });
    }

    pub fn run_yt(self: &Arc<DlManager>, t: Arc<crate::ytdlp::YtTask>) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let claimed = me.acquire_slot().await;
            crate::ytdlp::launch(t).await;
            if claimed {
                me.release_slot();
            }
        });
    }

    /// Retry every failed/cancelled HTTP task. Returns how many were restarted.
    pub fn retry_all(self: &Arc<DlManager>) -> usize {
        let ids: Vec<String> = self
            .http
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t)| {
                matches!(
                    *t.status.read().unwrap(),
                    DlStatus::Error | DlStatus::Cancelled | DlStatus::NeedsAuth
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut n = 0;
        for id in ids {
            if let Some(t) = self.http.lock().unwrap().get(&id).cloned() {
                t.cancel.store(false, Ordering::Relaxed);
                t.paused.store(false, Ordering::Relaxed);
                *t.error.lock().unwrap() = None;
                *t.status.write().unwrap() = DlStatus::Queued;
                self.run_http(t);
                n += 1;
            }
        }
        n
    }

    /// Resume every paused HTTP task. Returns how many were resumed.
    pub fn resume_all(self: &Arc<DlManager>) -> usize {
        let ids: Vec<String> = self
            .http
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t)| *t.status.read().unwrap() == DlStatus::Paused)
            .map(|(id, _)| id.clone())
            .collect();
        let mut n = 0;
        for id in ids {
            if let Some(t) = self.http.lock().unwrap().get(&id).cloned() {
                t.cancel.store(false, Ordering::Relaxed);
                t.paused.store(false, Ordering::Relaxed);
                *t.status.write().unwrap() = DlStatus::Queued;
                self.run_http(t);
                n += 1;
            }
        }
        n
    }

    /// Pause the given tasks (HTTP pause, yt-dlp process cancel).
    pub fn bulk_pause(self: &Arc<DlManager>, ids: &[String]) -> usize {
        let mut n = 0;
        for id in ids {
            let http = self.http.lock().unwrap().get(id).cloned();
            if let Some(t) = http {
                t.paused.store(true, Ordering::Relaxed);
                n += 1;
                continue;
            }
            if let Some(t) = self.yt.lock().unwrap().get(id) {
                t.cancel.store(true, Ordering::Relaxed);
                n += 1;
            }
        }
        n
    }

    /// Emergency stop: cancel + remove every currently downloading/queued/
    /// merging task (HTTP + youtube) so a bulk "Download Storm" is wiped
    /// instantly. Already-finished, paused and failed entries are left alone.
    /// Returns how many tasks were stopped.
    pub fn cancel_all_active(self: &Arc<DlManager>) -> usize {
        let active = |st: DlStatus| {
            matches!(
                st,
                DlStatus::Downloading | DlStatus::Queued | DlStatus::Merging | DlStatus::Resolving
            )
        };
        let mut ids: Vec<String> = Vec::new();
        for (_, t) in self.http.lock().unwrap().iter() {
            if active(*t.status.read().unwrap()) {
                ids.push(t.id.clone());
            }
        }
        for (_, t) in self.yt.lock().unwrap().iter() {
            if active(*t.status.read().unwrap()) {
                ids.push(t.id.clone());
            }
        }
        for id in &ids {
            self.remove(id);
        }
        ids.len()
    }

    /// Restart the given HTTP tasks. `only_failed` limits it to error/cancelled;
    /// otherwise paused tasks are resumed too.
    pub fn bulk_restart(self: &Arc<DlManager>, ids: &[String], only_failed: bool) -> usize {
        let mut n = 0;
        for id in ids {
            let t = self.http.lock().unwrap().get(id).cloned();
            if let Some(t) = t {
                let st = *t.status.read().unwrap();
                let wanted = if only_failed {
                    matches!(
                        st,
                        DlStatus::Error | DlStatus::Cancelled | DlStatus::NeedsAuth
                    )
                } else {
                    matches!(
                        st,
                        DlStatus::Paused | DlStatus::Error | DlStatus::Cancelled | DlStatus::NeedsAuth
                    )
                };
                if wanted {
                    t.cancel.store(false, Ordering::Relaxed);
                    t.paused.store(false, Ordering::Relaxed);
                    *t.error.lock().unwrap() = None;
                    *t.status.write().unwrap() = DlStatus::Queued;
                    self.run_http(t);
                    n += 1;
                }
            }
        }
        n
    }

    /// Store a login (optional) and (re)start every matching task waiting for it.
    /// Stored credentials are re-used automatically on future downloads.
    pub fn apply_credentials(
        self: &Arc<DlManager>,
        id: &str,
        cred: crate::auth::Cred,
        remember: bool,
        app: &AppHandle,
    ) -> usize {
        if remember {
            let mut settings = load_settings(app);
            settings.credentials.retain(|c| c.host != cred.host);
            settings.credentials.push(cred.clone());
            save_settings(app, &settings);
        }
        let ids: Vec<String> = {
            let map = self.http.lock().unwrap();
            map.iter()
                .filter(|(tid, t)| {
                    let st = *t.status.read().unwrap();
                    *tid == id
                        || (st == DlStatus::NeedsAuth && crate::auth::host_of(&t.url) == cred.host)
                })
                .map(|(tid, _)| tid.clone())
                .collect()
        };
        let mut restarted = 0;
        for tid in ids {
            if let Some(t) = self.http.lock().unwrap().get(&tid).cloned() {
                t.set_auth_ctx(cred.clone());
                self.run_http(t);
                restarted += 1;
            }
        }
        restarted
    }

    /// Forget a saved site login.
    pub fn remove_credential(&self, host: &str, app: &AppHandle) {
        let mut settings = load_settings(app);
        settings.credentials.retain(|c| c.host != host);
        save_settings(app, &settings);
    }
}

/// Delete `*.vtx.part` segment siblings of a finished file path.
pub(crate) fn cleanup_parts_for(save_path: &std::path::Path) {
    let dir = match save_path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => return,
    };
    let base = match save_path.file_name() {
        Some(b) => b.to_string_lossy().into_owned(),
        None => return,
    };
    let prefix = format!("{base}.");
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name.ends_with(".vtx.part") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

// ---------------- Settings ----------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub path: String,
    pub segments: usize,
    pub speed_limit: u64,
    pub notifications: bool,
    /// Play chimes on completion / error / batch-finish (Settings toggle).
    #[serde(default = "default_true")]
    pub sounds: bool,
    pub auto_start: bool,
    pub delete_part: bool,
    pub categorize_folders: bool,
    pub max_active: usize,
    pub auto_retries: u32,
    pub proxy: String,
    pub use_cookies: bool,
    pub cookies: String,
    /// Action when all downloads finish: "none" | "shutdown" | "sleep" | "hibernate" | "exit".
    pub on_complete: String,
    /// Optional epoch-ms: pause all HTTP downloads once reached.
    pub stop_at: Option<u64>,
    /// Daily queue scheduler: resume everything at `sched_start`, pause at
    /// `sched_stop` ("HH:MM", local time). Empty string = unset.
    #[serde(default)]
    pub sched_enabled: bool,
    #[serde(default)]
    pub sched_start: String,
    #[serde(default)]
    pub sched_stop: String,
    /// Show the floating always-on-top drop box.
    pub show_dropbox: bool,
    /// Watch the clipboard for URLs and auto-start download (like IDM).
    pub clipboard_monitor: bool,
    /// Auto-embed one subtitle track into downloaded videos.
    pub embed_subs: bool,
    /// Preferred subtitle language(s) for embedding, e.g. "en" or "en,bn".
    pub sub_langs: String,
    /// Auto-embed the video's thumbnail / cover art into downloads (MP4, MKV, MP3).
    pub embed_thumbnail: bool,
    /// Saved site logins used to auto-authenticate HTTP downloads (Basic/Digest).
    #[serde(default)]
    pub credentials: Vec<crate::auth::Cred>,
    /// Per-site proxy overrides, first enabled match wins. Empty = global only.
    #[serde(default)]
    pub per_site_proxies: Vec<PerSiteProxyRule>,
    /// Show the IDM-style "Download File Info" dialog before starting
    /// downloads (user can still start directly from the dialog).
    #[serde(default = "default_true")]
    pub show_download_info: bool,
    /// Remembered save folder per category id ("video", "zip", …).
    #[serde(default)]
    pub category_paths: std::collections::HashMap<String, String>,
}

/// One per-site proxy override: route a domain (or wildcard) through its own
/// proxy, or "DIRECT" to bypass the global proxy for it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PerSiteProxyRule {
    pub id: String,
    pub domain_pattern: String,
    pub proxy_url: String,
    pub enabled: bool,
}

impl Default for PerSiteProxyRule {
    fn default() -> Self {
        Self {
            id: String::new(),
            domain_pattern: String::new(),
            proxy_url: String::new(),
            enabled: true,
        }
    }
}

/// Resolve the effective proxy for `url`: first enabled matching rule wins;
/// "DIRECT" (any case) bypasses the global proxy; no match → global.
/// Returns the proxy URL to use, or "" for direct.
pub fn proxy_for_url(rules: &[PerSiteProxyRule], global_proxy: &str, url: &str) -> String {
    let host = crate::auth::host_of(url).to_lowercase();
    // Strip a trailing :port for matching (patterns are bare domains).
    // Bracketed IPv6 literals ([::1]:8080) are left intact.
    let host_bare: &str = if host.starts_with('[') {
        &host
    } else {
        host.split(':').next().unwrap_or(&host)
    };
    for r in rules {
        if !r.enabled {
            continue;
        }
        let pat = r.domain_pattern.trim().to_lowercase();
        if pat.is_empty() {
            continue;
        }
        let hit = if let Some(suffix) = pat.strip_prefix("*.") {
            !suffix.is_empty()
                && (host_bare == suffix || host_bare.ends_with(&format!(".{suffix}")))
        } else {
            host_bare == pat
        };
        if hit {
            return if r.proxy_url.trim().eq_ignore_ascii_case("direct") {
                String::new()
            } else {
                r.proxy_url.trim().to_string()
            };
        }
    }
    global_proxy.trim().to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            path: default_download_dir(),
            // Start at 16; the adaptive scaler in monitor_task grows toward 32
            // while per-connection throttling leaves bandwidth on the table.
            // 16 saturates typical broadband out of the box; servers that cap
            // per-connection speed still gain from the wider fan-out.
            segments: 16,
            speed_limit: 0,
            notifications: true,
            sounds: true,
            auto_start: false,
            delete_part: true,
            categorize_folders: true,
            max_active: 5,
            auto_retries: 3,
            proxy: String::new(),
            use_cookies: false,
            cookies: String::new(),
            on_complete: "none".into(),
            stop_at: None,
            sched_enabled: false,
            sched_start: String::new(),
            sched_stop: String::new(),
            show_dropbox: false,
            clipboard_monitor: false,
            embed_subs: true,
            sub_langs: "all".into(),
            embed_thumbnail: true,
            credentials: Vec::new(),
            per_site_proxies: Vec::new(),
            show_download_info: true,
            category_paths: std::collections::HashMap::new(),
        }
    }
}

/// Serde default for opt-in-true flags: old settings.json files without the
/// key must keep the feature ON (plain `bool` would default to false).
fn default_true() -> bool {
    true
}

fn default_download_dir() -> String {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ".".into())
}

pub fn settings_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

pub fn load_settings(app: &tauri::AppHandle) -> Settings {
    let p = match settings_path(app) {
        Ok(p) => p,
        Err(_) => return Settings::default(),
    };
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| Settings::default()),
        Err(_) => Settings::default(),
    }
}

pub fn save_settings(app: &tauri::AppHandle, s: &Settings) {
    if let Ok(p) = settings_path(app) {
        if let Ok(json) = serde_json::to_string_pretty(s) {
            let _ = std::fs::write(p, json);
        }
    }
}

/// Apply speed limit to the manager.
pub fn apply_speed_limit(manager: &DlManager, limit_bytes: u64) {
    manager.limit.store(limit_bytes, Ordering::Relaxed);
}

pub fn open_in_folder(path: &str) {
    let p = std::path::Path::new(path);
    let dir = if p.is_dir() {
        p.to_path_buf()
    } else {
        p.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| p.to_path_buf())
    };
    thread::spawn(move || {
        let _ = std::process::Command::new("explorer")
            .arg(dir.display().to_string())
            .spawn();
    });
}

pub fn open_file(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err("File not found on disk".into());
    }
    let owned = path.clone();
    thread::spawn(move || {
        // `cmd /c start` mangles non-ASCII paths (emoji etc.) through the
        // console codepage and `url.dll,FileProtocolHandler` chokes on them
        // too (verified live). explorer.exe is the shell itself: wide args,
        // no quoting pitfalls, opens with the default app (Play for media).
        let _ = crate::tools::silent(std::process::Command::new("explorer.exe"))
            .arg(&owned)
            .spawn();
    });
    Ok(())
}

// ---------------- Folder categorization ----------------

pub fn category_folder(cat: &str) -> &'static str {
    match cat {
        "video" => "Videos",
        "audio" => "Audio",
        "document" => "Documents",
        "program" => "Programs",
        "zip" => "Archives",
        "subtitle" => "Subtitles",
        _ => "Other",
    }
}

/// Effective save directory: base, or base/<Type> when categorization is on.
pub fn save_dir_for(base: &PathBuf, name: &str, categorize: bool) -> PathBuf {
    if !categorize {
        base.clone()
    } else {
        base.join(category_folder(crate::download::category_of(name)))
    }
}

// ---------------- OS notifications ----------------

pub fn notify_done(app: &AppHandle, title: &str, body: &str) {
    let _ = app
        .notification()
        .builder()
        .title(title.to_string())
        .body(body.to_string())
        .show();
}

// ---------------- Session persistence ----------------

pub fn history_file(app: &AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    dir.join("history.json")
}

#[derive(Clone, Serialize, Deserialize)]
struct PersistEntry {
    view: DlView,
    segments: Option<Vec<[u64; 2]>>,
}

pub fn persist_history(app: &AppHandle, mgr: &DlManager) {
    let mut recs: Vec<PersistEntry> = Vec::new();
    for h in mgr.history.lock().unwrap().iter() {
        recs.push(PersistEntry { view: h.clone(), segments: None });
    }
    for t in mgr.http.lock().unwrap().values() {
        let s = t.snapshot();
        let view = s.view.clone();
        recs.push(PersistEntry { view, segments: Some(s.segments) });
    }
    for t in mgr.yt.lock().unwrap().values() {
        recs.push(PersistEntry { view: t.view(), segments: None });
    }
    if let Ok(json) = serde_json::to_string(&serde_json::json!({ "records": recs })) {
        let _ = std::fs::write(history_file(app), json);
    }
}

/// Rebuild in-memory state from the persisted file, on startup.
pub fn load_history(app: &AppHandle, mgr: &DlManager) {
    let Ok(data) = std::fs::read_to_string(history_file(app)) else { return };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&data) else { return };
    let Some(records) = root.get("records").and_then(|r| r.as_array()) else { return };
    for r in records {
        let Ok(view) = serde_json::from_value::<DlView>(r.get("view").cloned().unwrap_or_default()) else { continue };
        let segs = r
            .get("segments")
            .and_then(|s| s.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| serde_json::from_value::<[u64; 2]>(v.clone()).ok())
                    .collect::<Vec<[u64; 2]>>()
            });
        let file_exists = std::path::Path::new(&view.save_path).exists();
        let resumable = !file_exists
            && matches!(
                view.status,
                DlStatus::Queued
                    | DlStatus::Downloading
                    | DlStatus::Paused
                    | DlStatus::Error
                    | DlStatus::Cancelled
            )
            && segs.as_ref().map(|s| !s.is_empty()).unwrap_or(false);

        if resumable {
            match crate::download::restore(
                app.clone(),
                view.clone(),
                segs.unwrap_or_default(),
                mgr.limit.clone(),
            ) {
                Ok(task) => {
                    mgr.add_http(task);
                }
                Err(_) => {
                    mgr.push_history(view);
                }
            }
        } else {
            let mut v = view;
            if v.status == DlStatus::Completed {
                if let Ok(md) = std::fs::metadata(&v.save_path) {
                    let sz = md.len();
                    if sz > 0 {
                        v.total_size = sz;
                        v.downloaded = sz;
                    }
                }
            }
            mgr.push_history(v);
        }
    }
}

pub fn persist_loop(app: AppHandle, mgr: Arc<DlManager>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            persist_history(&app, &mgr);
        }
    });
}

// ---------------- Clipboard monitoring (IDM-style) ----------------

#[cfg(windows)]
mod win_clip {
    #![allow(non_camel_case_types)]
    use std::ffi::c_void;
    use std::ptr;

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn GetClipboardData(u_format: u32) -> *mut c_void;
        fn IsClipboardFormatAvailable(u_format: u32) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalLock(h_mem: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(h_mem: *mut c_void) -> i32;
        fn GlobalSize(h_mem: *mut c_void) -> usize;
    }

    const CF_UNICODETEXT: u32 = 13;
    const MAX_LEN: usize = 4096;

    /// Read the current clipboard text (UTF-8), or None if unavailable.
    pub fn get_clipboard_text() -> Option<String> {
        unsafe {
            if OpenClipboard(ptr::null_mut()) == 0 {
                return None;
            }
            let result = (|| {
                if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
                    return None;
                }
                let h = GetClipboardData(CF_UNICODETEXT);
                if h.is_null() {
                    return None;
                }
                let lock = GlobalLock(h);
                if lock.is_null() {
                    return None;
                }
                let size = GlobalSize(h);
                let byte_len = size.min(MAX_LEN * 2);
                if byte_len < 2 {
                    GlobalUnlock(h);
                    return None;
                }
                let slice = std::slice::from_raw_parts(lock as *const u16, byte_len / 2);
                let s = String::from_utf16_lossy(slice);
                GlobalUnlock(h);
                Some(s.trim_end_matches('\0').to_string())
            })();
            CloseClipboard();
            result
        }
    }
}

/// Extract http(s) URLs from a clipboard text blob.
fn urls_in_text(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        let mut t = token.trim();
        while (t.starts_with('(') || t.starts_with('[') || t.starts_with('{')) && t.len() > 1 {
            t = &t[1..];
        }
        let mut end = t.len();
        while end > 0
            && matches!(t.as_bytes()[end - 1], b')' | b']' | b'}' | b'.' | b',' | b';' | b'"' | b'\'' | b'!' | b'?')
        {
            end -= 1;
        }
        let t = &t[..end];
        let low = t.to_ascii_lowercase();
        if low.starts_with("http://") || low.starts_with("https://") || low.starts_with("ftp://") {
            if !out.contains(&t.to_string()) {
                out.push(t.to_string());
            }
        }
    }
    out
}

/// True when `url` is a YouTube/YouTube-mirror playlist link.
/// Matches with or without `www` (e.g. `https://youtube.com/playlist?list=…`,
/// `https://m.youtube.com/playlist?list=…`, `https://youtu.be/abc?list=…`).
fn is_youtube_playlist_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    // Host is a YouTube family domain (www / m. / no-www all contain the bare domain).
    if !(lower.contains("youtube.com")
        || lower.contains("youtu.be")
        || lower.contains("youtube-nocookie.com"))
    {
        return false;
    }
    // Bare playlist pages: /playlist or /playlists (with or without query).
    if lower.contains("/playlist") {
        return true;
    }
    // A real `list=` query parameter: watch?v=…&list=…, youtu.be/…?list=….
    let Some(qi) = lower.find('?') else { return false };
    lower[qi + 1..].split('&').any(|kv| kv.starts_with("list="))
}

/// Spawn the clipboard watcher. While `clipboard_monitor` is enabled it polls the
/// clipboard and auto-queues any new http(s) URL it finds (like IDM).
pub fn clipboard_monitor_loop(app: AppHandle, mgr: Arc<DlManager>) {
    std::thread::spawn(move || {
        #[cfg(windows)]
        {
            let mut last: Option<String> = None;
            let mut last_seen_ms: u64 = 0;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(700));
                let settings = load_settings(&app);
                if !settings.clipboard_monitor {
                    continue;
                }
                let Some(text) = win_clip::get_clipboard_text() else { continue };
                let now = crate::download::now_ms();
                let urls = urls_in_text(&text);
                if urls.is_empty() {
                    // remember recent non-URL text so a repeat copy of the same text still works
                    last_seen_ms = now;
                    continue;
                }
                let url = urls[0].clone();
                // Debounce: same URL copied again within 4s is treated as a re-copy trigger.
                if last.as_deref() == Some(url.as_str()) && now.saturating_sub(last_seen_ms) < 4000 {
                    last_seen_ms = now;
                    continue;
                }
                last = Some(url.clone());
                last_seen_ms = now;

                if is_youtube_playlist_url(&url) {
                    let app = app.clone();
                    let url = url.clone();
                    tauri::async_runtime::spawn(async move {
                        // Bring Vortex to the front so the playlist modal is immediately visible.
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.unminimize();
                            let _ = win.set_focus();
                        }
                        let _ = app.emit("playlist-clip", serde_json::json!({ "url": url }));
                    });
                    continue;
                }

                let app = app.clone();
                let mgr = mgr.clone();
                let settings = settings.clone();
                tauri::async_runtime::spawn(async move {
                    if !crate::download::url_is_downloadable(&url) {
                        return;
                    }
                    // Dialog mode: standalone File Info window instead of
                    // auto-starting (OFF keeps today's direct path below).
                    // The main dashboard is never raised for these prompts.
                    if settings.show_download_info {
                        crate::open_info_window(
                            &app,
                            serde_json::json!({ "url": url, "filename": "", "referer": "", "cookies": "" }),
                        );
                        return;
                    }
                        let opts = download::StartOpts {
                            segments: settings.segments,
                            filename: None,
                            categorize: settings.categorize_folders,
                            start_at: None,
                            auto_retries: settings.auto_retries,
                            proxy: settings.proxy.clone(),
                            referer: None,
                            cookies: None,
                            on_exists: None,
                        };
                        if let Ok(task) = crate::download::start(app.clone(), url.clone(), settings.path.clone(), opts, mgr.limit.clone()).await {
                            let id = task.id.clone();
                            let view = task.view();
                            mgr.add_http(task.clone());
                            mgr.run_http(task);
                            let _ = app.emit("downloads-changed", ());
                            let _ = app.emit(
                                "clips", 
                                serde_json::json!({ "url": url, "id": id, "filename": view.title }),
                            );
                        }
                });
            }
        }
    });
}

// ---- Keep the PC awake while any download is active (Windows) ----
#[cfg(windows)]
#[link(name = "kernel32")]
extern "C" {
    fn SetThreadExecutionState(es_flags: u32) -> u32;
}

#[cfg(windows)]
const ES_CONTINUOUS: u32 = 0x8000_0000;
#[cfg(windows)]
const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;

/// Background loop: block system sleep while busy, release when idle.
pub fn sleep_block_loop(mgr: Arc<DlManager>) {
    std::thread::spawn(move || loop {
        let busy = mgr.views().iter().any(|v| {
            v.status == DlStatus::Downloading
                || v.status == DlStatus::Merging
                || v.status == DlStatus::Resolving
        });
        #[cfg(windows)]
        unsafe {
            SetThreadExecutionState(if busy { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
        }
        std::thread::sleep(std::time::Duration::from_secs(20));
    });
}

/// Watch for queue-idle transitions to run the on-complete action
/// (shutdown / sleep / hibernate / exit) and the stop_at scheduler.
/// Parse a daily "HH:MM" scheduler time into (day, epoch-ms) for today (local).
/// None when blank or malformed.
fn sched_today_ms(hhmm: &str) -> Option<(String, u64)> {
    let (h, m) = hhmm.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    let now = chrono::Local::now();
    let target = now
        .date_naive()
        .and_hms_opt(h, m, 0)?
        .and_local_timezone(chrono::Local)
        .single()?
        .timestamp_millis()
        .max(0) as u64;
    Some((now.format("%Y-%m-%d").to_string(), target))
}

pub fn completion_watch_loop(app: AppHandle, mgr: Arc<DlManager>) {
    tauri::async_runtime::spawn(async move {
        let mut was_busy = false;
        let mut hist_len = mgr.history.lock().unwrap().len();
        let mut last_start_day = String::new();
        let mut last_stop_day = String::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let views = mgr.views();
            let busy = views.iter().any(|v| {
                v.status == DlStatus::Downloading
                    || v.status == DlStatus::Merging
                    || v.status == DlStatus::Queued
                    || v.status == DlStatus::Resolving
            });
            let hl = mgr.history.lock().unwrap().len();
            if busy || hl > hist_len {
                was_busy = true;
            }
            hist_len = hl;
            let mut settings = load_settings(&app);
            // Daily queue scheduler: resume everything at sched_start, pause at
            // sched_stop (local "HH:MM", fire-once-per-day so it never loops).
            if settings.sched_enabled {
                let now_ms = crate::download::now_ms();
                if let Some((day, at)) = sched_today_ms(settings.sched_start.trim()) {
                    if now_ms >= at && last_start_day != day {
                        last_start_day = day;
                        let n = mgr.resume_all();
                        let _ = app.emit("downloads-changed", ());
                        notify_done(&app, &format!("Scheduler: queue started ({n} resumed)"), "Start time reached");
                    }
                }
                if let Some((day, at)) = sched_today_ms(settings.sched_stop.trim()) {
                    if now_ms >= at && last_stop_day != day {
                        last_stop_day = day;
                        for task in mgr.http.lock().unwrap().values() {
                            task.paused.store(true, Ordering::Relaxed);
                        }
                        let _ = app.emit("downloads-changed", ());
                        notify_done(&app, "Scheduler: downloads paused", "Stop time reached");
                    }
                }
            }
            // Scheduler: pause all HTTP downloads once the stop time is reached.
            if let Some(t) = settings.stop_at {
                if crate::download::now_ms() >= t {
                    for task in mgr.http.lock().unwrap().values() {
                        task.paused.store(true, Ordering::Relaxed);
                    }
                    let _ = app.emit("downloads-changed", ());
                    notify_done(&app, "Scheduler: downloads paused", "Stop time reached");
                    settings.stop_at = None;
                    save_settings(&app, &settings);
                    was_busy = false;
                    continue;
                }
            }
            if !busy && was_busy {
                was_busy = false;
                match settings.on_complete.trim() {
                    "shutdown" => {
                        notify_done(&app, "All downloads complete", "PC will shut down in 60s (run shutdown /a to abort)");
                        let _ = crate::tools::silent(std::process::Command::new("shutdown"))
                            .args(["/s", "/t", "60", "/c", "Vortex: downloads complete"])
                            .spawn();
                    }
                    "sleep" => {
                        notify_done(&app, "All downloads complete", "Putting PC to sleep");
                        let _ = crate::tools::silent(std::process::Command::new("rundll32.exe"))
                            .args(["powrprof.dll,SetSuspendState", "0,1,0"])
                            .spawn();
                    }
                    "hibernate" => {
                        notify_done(&app, "All downloads complete", "Hibernating PC");
                        let _ = crate::tools::silent(std::process::Command::new("shutdown"))
                            .args(["/h"])
                            .spawn();
                    }
                    "exit" => {
                        notify_done(&app, "All downloads complete", "Closing Vortex");
                        persist_history(&app, &mgr);
                        app.exit(0);
                    }
                    _ => {}
                }
            }
        }
    });
}

#[cfg(test)]
mod proxy_tests {
    use super::*;

    fn rule(pattern: &str, proxy: &str, enabled: bool) -> PerSiteProxyRule {
        PerSiteProxyRule {
            id: "t".into(),
            domain_pattern: pattern.into(),
            proxy_url: proxy.into(),
            enabled,
        }
    }

    #[test]
    fn proxy_for_url_matching() {
        let rules = vec![
            rule("github.com", "http://127.0.0.1:8080", true),
            rule("*.example.com", "socks5://127.0.0.1:1080", true),
            rule("blocked.test", "DIRECT", true),
            rule("off.test", "http://127.0.0.1:9999", false),
        ];
        // exact
        assert_eq!(
            proxy_for_url(&rules, "", "https://github.com/a/b.zip"),
            "http://127.0.0.1:8080"
        );
        // wildcard: sub + apex
        assert_eq!(
            proxy_for_url(&rules, "", "https://a.b.example.com/f"),
            "socks5://127.0.0.1:1080"
        );
        assert_eq!(
            proxy_for_url(&rules, "", "https://example.com/f"),
            "socks5://127.0.0.1:1080"
        );
        // lookalike must NOT match wildcard
        assert_eq!(proxy_for_url(&rules, "http://g:1", "https://notexample.com/f"), "http://g:1");
        // DIRECT bypasses global
        assert_eq!(proxy_for_url(&rules, "http://g:1", "https://blocked.test/f"), "");
        assert_eq!(proxy_for_url(&rules, "http://g:1", "https://BLOCKED.test:8080/f"), "");
        // disabled rule ignored → global
        assert_eq!(proxy_for_url(&rules, "http://g:1", "https://off.test/f"), "http://g:1");
        // no match → global / direct
        assert_eq!(proxy_for_url(&rules, "http://g:1", "https://other.io/f"), "http://g:1");
        assert_eq!(proxy_for_url(&rules, "", "https://other.io/f"), "");
        assert_eq!(proxy_for_url(&[], "  http://g:1  ", "https://other.io/f"), "http://g:1");
    }
}