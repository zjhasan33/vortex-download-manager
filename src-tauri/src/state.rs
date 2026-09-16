use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::download::{DlStatus, DlView};

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
    pub fn remove_with_file(&self, id: &str, delete_file: bool) {
        if delete_file {
            let mut paths: Vec<PathBuf> = Vec::new();
            if let Some(t) = self.http.lock().unwrap().get(id) {
                paths.push(t.save_path.clone());
            }
            if let Some(t) = self.yt.lock().unwrap().get(id) {
                paths.push(t.save_path.lock().unwrap().clone());
            }
            for h in self.history.lock().unwrap().iter() {
                if h.id == id {
                    paths.push(PathBuf::from(&h.save_path));
                }
            }
            for p in paths {
                if p.is_file() {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        self.remove(id);
    }

    pub fn views(&self) -> Vec<DlView> {
        let mut v: Vec<DlView> = Vec::new();
        for t in self.http.lock().unwrap().values() {
            v.push(t.view());
        }
        for t in self.yt.lock().unwrap().values() {
            v.push(t.view());
        }
        for h in self.history.lock().unwrap().iter() {
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
        for v in self.views() {
            if v.status == DlStatus::Downloading || v.status == DlStatus::Merging {
                total_speed += v.speed;
                active += 1;
                segments += v.connections as u64;
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

    pub fn is_running(&self) -> bool {
        self.views().iter().any(|v| {
            matches!(
                v.status,
                DlStatus::Downloading | DlStatus::Merging | DlStatus::Queued
            )
        })
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
                    DlStatus::Error | DlStatus::Cancelled
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
}

// ---------------- Settings ----------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub path: String,
    pub segments: usize,
    pub speed_limit: u64,
    pub notifications: bool,
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
    /// Show the floating always-on-top drop box.
    pub show_dropbox: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            path: default_download_dir(),
            segments: 8,
            speed_limit: 0,
            notifications: true,
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
            show_dropbox: false,
        }
    }
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

pub fn open_file(path: String) {
    thread::spawn(move || {
        // Avoid shell injection: use ShellExecute via `start` with properly quoted path
        // or directly use `cmd /c start "" "path"` with quoting.
        // We validate the path exists and quote it to prevent & | > injection.
        let p = std::path::Path::new(&path);
        if !p.exists() {
            return;
        }
        let quoted = format!("\"{}\"", path.replace('"', "\"\""));
        let _ = crate::tools::silent(std::process::Command::new("cmd"))
            .args(["/c", "start", "", &quoted])
            .spawn();
        // Fallback: try direct open via explorer if cmd fails
        // (explorer handles paths without shell interpretation)
    });
}

// ---------------- Folder categorization ----------------

pub fn category_folder(cat: &str) -> &'static str {
    match cat {
        "video" => "Videos",
        "audio" => "Audio",
        "document" => "Documents",
        "program" => "Programs",
        "zip" => "Archives",
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
        let busy = mgr
            .views()
            .iter()
            .any(|v| v.status == DlStatus::Downloading || v.status == DlStatus::Merging);
        #[cfg(windows)]
        unsafe {
            SetThreadExecutionState(if busy { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
        }
        std::thread::sleep(std::time::Duration::from_secs(20));
    });
}

/// Watch for queue-idle transitions to run the on-complete action
/// (shutdown / sleep / hibernate / exit) and the stop_at scheduler.
pub fn completion_watch_loop(app: AppHandle, mgr: Arc<DlManager>) {
    tauri::async_runtime::spawn(async move {
        let mut was_busy = false;
        let mut hist_len = mgr.history.lock().unwrap().len();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let views = mgr.views();
            let busy = views.iter().any(|v| {
                v.status == DlStatus::Downloading
                    || v.status == DlStatus::Merging
                    || v.status == DlStatus::Queued
            });
            let hl = mgr.history.lock().unwrap().len();
            if busy || hl > hist_len {
                was_busy = true;
            }
            hist_len = hl;
            let mut settings = load_settings(&app);
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
                        app.exit(0);
                    }
                    _ => {}
                }
            }
        }
    });
}