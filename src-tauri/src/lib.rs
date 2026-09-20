use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

mod auth;
pub mod download;
mod grabber;
mod md5;
mod state;
mod tools;
mod ws_server;
mod ytdlp;

use crate::state::{DlManager, Settings};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

fn set_autostart(enabled: bool) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let quoted = format!("\"{}\" --autostart", exe.display());
    #[cfg(windows)]
    {
        if enabled {
            let _ = std::process::Command::new("reg")
                .args([
                    "add",
                    "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                    "/v",
                    "Vortex",
                    "/t",
                    "REG_SZ",
                    "/d",
                    &quoted,
                    "/f",
                ])
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .spawn();
        } else {
            let _ = std::process::Command::new("reg")
                .args([
                    "delete",
                    "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                    "/v",
                    "Vortex",
                    "/f",
                ])
                .creation_flags(0x08000000)
                .spawn();
        }
    }
    #[cfg(not(windows))]
    let _ = quoted;
}

fn register_vortex_protocol() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    #[cfg(windows)]
    {
        let cmd = format!("\"{}\" \"%1\"", exe.display());
        let _ = std::process::Command::new("reg")
            .args(["add", "HKCU\\Software\\Classes\\vortex", "/f", "/t", "REG_SZ", "/d", "URL:Vortex Protocol"])
            .creation_flags(0x08000000)
            .spawn();
        let _ = std::process::Command::new("reg")
            .args(["add", "HKCU\\Software\\Classes\\vortex", "/f", "/v", "URL Protocol", "/t", "REG_SZ", "/d", ""])
            .creation_flags(0x08000000)
            .spawn();
        let _ = std::process::Command::new("reg")
            .args(["add", "HKCU\\Software\\Classes\\vortex\\shell\\open\\command", "/f", "/t", "REG_SZ", "/d", &cmd])
            .creation_flags(0x08000000)
            .spawn();
    }
    #[cfg(not(windows))]
    let _ = exe;
}

struct CapturePayload {
    url: String,
    filename: Option<String>,
    via_yt: bool,
    format_id: Option<String>,
}

fn youtube_id(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    // youtu.be/<id>
    if let Some(p) = lower.find("youtu.be/") {
        let id = url[p + 9..].split(['?', '&', '#', '/']).next().unwrap_or("").trim().to_string();
        if id.len() >= 6 {
            return Some(id);
        }
    }
    // youtube.com/watch?v=<id> or &v=<id>
    for key in ["?v=", "&v="] {
        if let Some(p) = lower.find(key) {
            let id = url[p + key.len()..].split(['?', '&', '#', '/']).next().unwrap_or("").trim().to_string();
            if id.len() >= 6 {
                return Some(id);
            }
        }
    }
    // youtube.com/embed/<id> or /v/<id> or /shorts/<id>
    for tag in ["/embed/", "/v/", "/shorts/"] {
        if let Some(p) = lower.find(tag) {
            let id = url[p + tag.len()..].split(['?', '&', '#', '/']).next().unwrap_or("").trim().to_string();
            if id.len() >= 6 {
                return Some(id);
            }
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_capture_arg(arg: &str) -> Option<CapturePayload> {
    let rest = arg.strip_prefix("vortex://")?.strip_prefix("capture?")?;
    let mut p = CapturePayload { url: String::new(), filename: None, via_yt: false, format_id: None };
    for pair in rest.split('&') {
        let mut it = pair.splitn(2, '=');
        let (k, v) = (it.next()?, it.next()?);
        match k {
            "url" => p.url = percent_decode(v),
            "filename" => p.filename = Some(percent_decode(v)),
            "via" => p.via_yt = v == "yt",
            "format" => p.format_id = Some(percent_decode(v)),
            _ => {}
        }
    }
    if p.url.is_empty() {
        None
    } else {
        Some(p)
    }
}

fn handle_capture(app: &tauri::AppHandle, cap: CapturePayload) {
    let mgr = app.state::<Arc<DlManager>>().inner().clone();
    let settings = state::load_settings(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let launched = if cap.via_yt {
            launch_yt_from_capture(
                app.clone(),
                mgr.clone(),
                cap,
                settings.path,
                settings.categorize_folders,
                settings.proxy.clone(),
            )
            .await
        } else {
            let opts = download::StartOpts {
                segments: settings.segments,
                filename: cap.filename,
                categorize: settings.categorize_folders,
                start_at: None,
                auto_retries: settings.auto_retries,
                proxy: settings.proxy.clone(),
                referer: None,
                cookies: None,
                on_exists: None,
            };
            match download::start(app.clone(), cap.url, settings.path, opts, mgr.limit.clone()).await {
                Ok(task) => {
                    let id = task.id.clone();
                    let view = task.view();
                    mgr.add_http(task.clone());
                    mgr.run_http(task);
                    Some((id, view))
                }
                Err(_) => None,
            }
        };
        if let Some((_id, _view)) = launched {
            let _ = app.emit("downloads-changed", ());
        }
    });
}

async fn launch_yt_from_capture(
    app: tauri::AppHandle,
    mgr: Arc<DlManager>,
    cap: CapturePayload,
    save_path: String,
    categorize: bool,
    proxy: String,
) -> Option<(String, download::DlView)> {
    let fmt = match cap.format_id {
        Some(fid) => Some(fid),
        None => {
            let info = ytdlp::fetch_info(app.clone(), cap.url.clone(), ytdlp::StreamCtx::default()).await.ok()?;
            info.formats
                .iter()
                .find(|f| f.kind == "video" && f.has_video && f.has_audio && f.height == Some(720))
                .or_else(|| info.formats.iter().find(|f| f.has_video && f.has_audio))
                .or_else(|| info.formats.iter().find(|f| f.has_video))
                .map(|f| f.id.clone())
        }
    }?;
    let settings = crate::state::load_settings(&app);
    let task = ytdlp::start(
        app.clone(),
        cap.url,
        fmt,
        save_path,
        categorize,
        proxy,
        false,
        String::new(),
        None,
        settings.embed_subs,
        settings.sub_langs,
        settings.embed_thumbnail,
        false,
        ytdlp::StreamCtx::default(),
    )
    .await
    .ok()?;
    let id = task.id.clone();
    let view = task.view();
    mgr.add_yt(task.clone());
    mgr.run_yt(task);
    Some((id, view))
}

// ---------------- Commands ----------------

#[tauri::command]
async fn start_download(
    app: tauri::AppHandle,
    state: State<'_, Arc<DlManager>>,
    url: String,
    save_path: String,
    segments: usize,
    filename: Option<String>,
    start_at: Option<u64>,
    start_paused: Option<bool>,
    on_exists: Option<String>,
    referer: Option<String>,
    cookies: Option<String>,
) -> Result<download::DlView, String> {
    // IDM parity: same file already queued/downloading → warn instead of silent duplicate.
    // Covers the "0 B still downloading" case (file not on disk yet) and YouTube ID variants (youtu.be vs watch?v=).
    let views = state.views();
    let dup = views.iter().find(|v| {
        if matches!(v.status, download::DlStatus::Completed | download::DlStatus::Cancelled) {
            return false;
        }
        if v.url == url {
            return true;
        }
        if let (Some(a), Some(b)) = (youtube_id(&v.url), youtube_id(&url)) {
            return a.eq_ignore_ascii_case(&b);
        }
        if let Some(fname) = &filename {
            if !fname.is_empty() && v.filename == *fname {
                return true;
            }
        }
        false
    });
    if on_exists.as_deref() == Some("prompt") {
        if let Some(dup) = dup {
            return Err(format!("EXISTS::{} (already in list: {:?})", dup.save_path, dup.status));
        }
    } else if dup.is_some() && filename.is_none() {
        // Even without prompt (grabber batch etc.), don't silently queue a 4th copy of the same video.
        if let Some(d) = dup {
            return Err(format!("EXISTS::{} (already in list: {:?})", d.save_path, d.status));
        }
    }
    let settings = state::load_settings(&app);
    let opts = download::StartOpts {
        segments,
        filename,
        categorize: settings.categorize_folders,
        start_at,
        auto_retries: settings.auto_retries,
        proxy: settings.proxy.clone(),
        referer,
        cookies,
        on_exists,
    };
    let task = download::start(app.clone(), url, save_path, opts, state.limit.clone()).await?;
    // Add in "Paused" state (Grabber "Start immediately" OFF) so the batch
    // doesn't flood bandwidth at once; the user resumes individually/from toolbar.
    if start_paused.unwrap_or(false) {
        task.paused.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let id = task.id.clone();
    state.add_http(task.clone());
    state.run_http(task);
    Ok(state.http.lock().unwrap().get(&id).map(|t| t.view()).ok_or("task not found")?)
}

#[tauri::command]
async fn pause_download(_app: tauri::AppHandle, state: State<'_, Arc<DlManager>>, id: String) -> Result<(), String> {
    // Scoped lookups: std Mutex guards must not live across awaits (not Send).
    let ht = state.http.lock().unwrap().get(&id).cloned();
    if let Some(t) = ht {
        t.paused.store(true, std::sync::atomic::Ordering::Relaxed);
        return Ok(());
    }
    let yt = state.yt.lock().unwrap().get(&id).cloned();
    if let Some(t) = yt {
        t.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        return Ok(());
    }
    Ok(())
}

/// Supply login credentials for downloads that hit HTTP 401/407.
/// `remember` saves the login for this host so future downloads auto-authenticate.
#[tauri::command]
async fn set_auth(
    app: tauri::AppHandle,
    state: State<'_, Arc<DlManager>>,
    id: String,
    host: String,
    username: String,
    password: String,
    remember: bool,
) -> Result<usize, String> {
    let mut realm_host = host.trim().to_string();
    if realm_host.is_empty() {
        let u = state
            .http
            .lock()
            .unwrap()
            .get(&id)
            .map(|t| t.url.clone())
            .unwrap_or_default();
        realm_host = crate::auth::host_of(&u);
    }
    let cred = crate::auth::Cred { host: realm_host, username, password };
    Ok(state.apply_credentials(&id, cred, remember, &app))
}

/// Forget a saved site login.
#[tauri::command]
async fn remove_credential(
    state: State<'_, Arc<DlManager>>,
    app: tauri::AppHandle,
    host: String,
) -> Result<(), String> {
    state.remove_credential(&host, &app);
    Ok(())
}

#[tauri::command]
async fn resume_download(_app: tauri::AppHandle, state: State<'_, Arc<DlManager>>, id: String) -> Result<(), String> {
    let m = state.http.lock().unwrap();
    if let Some(t) = m.get(&id) {
        // Never double-run an already live task (two loops would corrupt).
        let st = *t.status.read().unwrap();
        if matches!(
            st,
            crate::download::DlStatus::Downloading | crate::download::DlStatus::Merging
        ) {
            return Ok(());
        }
        // Clear BOTH flags so resume works after pause AND after cancel/error:
        // part files were kept, so the task continues from partial bytes.
        t.cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        t.paused.store(false, std::sync::atomic::Ordering::Relaxed);
        *t.error.lock().unwrap() = None;
        *t.status.write().unwrap() = crate::download::DlStatus::Queued;
        let c = t.clone();
        drop(m);
        state.run_http(c);
    }
    // youtube tasks are process-bound; resuming restarts via the HTTP path only
    let yt = state.yt.lock().unwrap().get(&id).cloned();
    if let Some(t) = yt {
        // Launch paused/queued yt tasks (Download Later). Never double-run a
        // live one — same status guard as the HTTP branch above.
        let st = *t.status.read().unwrap();
        if matches!(
            st,
            crate::download::DlStatus::Paused | crate::download::DlStatus::Queued
        ) {
            *t.status.write().unwrap() = crate::download::DlStatus::Queued;
            state.run_yt(t);
        }
    }
    Ok(())
}

#[tauri::command]
async fn retry_all_downloads(app: tauri::AppHandle, state: State<'_, Arc<DlManager>>) -> Result<usize, String> {
    let n = state.retry_all();
    if n > 0 {
        let _ = app.emit("downloads-changed", ());
    }
    Ok(n)
}

#[tauri::command]
async fn resume_all_downloads(app: tauri::AppHandle, state: State<'_, Arc<DlManager>>) -> Result<usize, String> {
    let n = state.resume_all();
    if n > 0 {
        let _ = app.emit("downloads-changed", ());
    }
    Ok(n)
}

/// Emergency "Stop & Cancel All": instantly halts + removes every active and
/// queued task (grabber batches, multi-downloads) to wipe a bandwidth storm.
#[tauri::command]
async fn cancel_all_active(app: tauri::AppHandle, state: State<'_, Arc<DlManager>>) -> Result<usize, String> {
    let n = state.cancel_all_active();
    let _ = app.emit("downloads-changed", ());
    Ok(n)
}

/// Pause every active (downloading/merging/queued) HTTP + youtube task.
async fn pause_all_inner(app: &tauri::AppHandle, mgr: Arc<DlManager>) -> usize {
    let ids: Vec<String> = mgr
        .views()
        .iter()
        .filter(|v| {
            matches!(
                v.status,
                download::DlStatus::Downloading
                    | download::DlStatus::Merging
                    | download::DlStatus::Queued
                    | download::DlStatus::Resolving
            )
        })
        .map(|v| v.id.clone())
        .collect();
    let n = mgr.bulk_pause(&ids);
    if n > 0 {
        let _ = app.emit("downloads-changed", ());
    }
    n
}

#[tauri::command]
async fn pause_all_downloads(app: tauri::AppHandle, state: State<'_, Arc<DlManager>>) -> Result<usize, String> {
    Ok(pause_all_inner(&app, state.inner().clone()).await)
}

#[tauri::command]
async fn cancel_download(app: tauri::AppHandle, state: State<'_, Arc<DlManager>>, id: String) -> Result<(), String> {
    // Cancel: stop download but keep entry + part files so Resume continues
    // from partial bytes. Idle (paused/queued) tasks flip to Cancelled right
    // away; running ones transition via their run loop.
    // (Scoped clones: std Mutex guards must not live across awaits.)
    enum Target {
        Http(Arc<crate::download::Task>),
        Yt(Arc<crate::ytdlp::YtTask>),
        None,
    }
    let target = {
        if let Some(t) = state.http.lock().unwrap().get(&id).cloned() {
            Target::Http(t)
        } else if let Some(t) = state.yt.lock().unwrap().get(&id).cloned() {
            Target::Yt(t)
        } else {
            Target::None
        }
    };
    let should_emit = match target {
        Target::Http(t) => {
            t.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            t.paused.store(false, std::sync::atomic::Ordering::Relaxed);
            let st = *t.status.read().unwrap();
            if !matches!(
                st,
                crate::download::DlStatus::Downloading | crate::download::DlStatus::Merging
            ) {
                *t.status.write().unwrap() = crate::download::DlStatus::Cancelled;
            }
            true
        }
        Target::Yt(t) => {
            t.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            true
        }
        Target::None => false,
    };
    if should_emit { let _ = app.emit("downloads-changed", ()); }
    Ok(())
}

/// Abort a pending Vortex-scheduled shutdown (`shutdown /s /t 60` grace window).
#[tauri::command]
async fn cancel_shutdown(app: tauri::AppHandle) -> Result<(), String> {
    let out = crate::tools::silent(std::process::Command::new("shutdown"))
        .args(["/a"])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        crate::state::notify_done(&app, "Scheduled shutdown cancelled", "Shutdown aborted");
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

#[tauri::command]
async fn remove_download(app: tauri::AppHandle, state: State<'_, Arc<DlManager>>, id: String, delete_file: Option<bool>) -> Result<(), String> {
    remove_one(&app, &state, &id, delete_file.unwrap_or(false));
    let _ = app.emit("downloads-changed", ());
    Ok(())
}

/// Apply a bulk action to a set of task ids. `action` is one of
/// "pause" | "resume" | "retry" | "remove". Returns how many were affected.
#[tauri::command]
async fn downloads_action(
    app: tauri::AppHandle,
    state: State<'_, Arc<DlManager>>,
    action: String,
    ids: Vec<String>,
    delete_file: Option<bool>,
) -> Result<usize, String> {
    let n = match action.as_str() {
        "remove" => {
            let del = delete_file.unwrap_or(false);
            for id in &ids {
                remove_one(&app, &state, id, del);
            }
            ids.len()
        }
        "pause" => state.bulk_pause(&ids),
        "resume" => state.bulk_restart(&ids, false),
        "retry" => state.bulk_restart(&ids, true),
        other => return Err(format!("unknown action: {other}")),
    };
    let _ = app.emit("downloads-changed", ());
    Ok(n)
}

/// Lightweight pre-download probe for the "Download File Info" dialog:
/// filename + size + category without starting anything. Never fails hard —
/// unknown size/filename just comes back empty for the dialog to display.
#[tauri::command]
async fn probe_download_info(url: String) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Vortex/1.1")
        .build()
        .map_err(|e| e.to_string())?;
    // Range 0-0: many servers answer 206 with the total; plain HEAD is often
    // ignored. Failures just mean "Unknown" in the dialog, not an error.
    let (filename, size) = match client
        .get(&url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .await
    {
        Ok(resp) => {
            let total = crate::download::parse_total(&resp);
            let name = crate::download::resolve_filename(&resp, &url);
            (name, total)
        }
        Err(_) => {
            let fallback = url
                .rsplit(['/', '?', '#'])
                .next()
                .unwrap_or("download")
                .to_string();
            (fallback, 0)
        }
    };
    // Refuse absurd sizes here too (same 1 TiB guard as the download path).
    let size = if size > (1 << 40) { 0 } else { size };
    Ok(serde_json::json!({
        "filename": filename,
        "size": size,
        "category": crate::download::category_of(&filename),
    }))
}

/// One-shot handoff for the standalone "Download File Info" dialog window:
/// the opener stores the payload, the dialog takes it at boot (race-free,
/// no event-timing dependency).
#[tauri::command]
async fn take_dialog_payload(
    state: State<'_, std::sync::Mutex<Option<serde_json::Value>>>,
) -> Result<Option<serde_json::Value>, String> {
    Ok(state.lock().unwrap_or_else(|e| e.into_inner()).take())
}

/// Best-effort duplicate warning for YouTube jobs: guess the final output
/// path the same way the completion step does (yt-dlp auto-renames on real
/// collisions, so this is informational, never a gate).
#[tauri::command]
async fn ytdl_expected_path(
    dir: String,
    title: String,
    ext: String,
    playlist: bool,
) -> Result<serde_json::Value, String> {
    let dir = std::path::PathBuf::from(dir.trim());
    let (path, is_dir) = if playlist {
        let folder = dir.join(crate::download::sanitize(&title));
        (folder, true)
    } else {
        let stem = crate::download::sanitize(&title);
        let ext = ext.trim().trim_start_matches('.');
        let name = if ext.is_empty() { stem.clone() } else { format!("{stem}.{ext}") };
        (dir.join(name), false)
    };
    let exists = if is_dir {
        path.is_dir()
            && std::fs::read_dir(&path).map(|mut e| e.next().is_some()).unwrap_or(false)
    } else {
        path.is_file()
    };
    Ok(serde_json::json!({ "exists": exists, "path": path.display().to_string(), "is_dir": is_dir }))
}

#[tauri::command]
async fn delete_file_at(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(path.trim());
    if p.is_file() {
        std::fs::remove_file(&p).map_err(|e| e.to_string())?;
    } else if p.is_dir() {
        std::fs::remove_dir_all(&p).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Open (or focus) the standalone "Download File Info" dialog window with a
/// stored payload. The main dashboard is never raised for these prompts.
fn open_info_window(app: &tauri::AppHandle, payload: serde_json::Value) {
    if let Some(pending) = app.try_state::<std::sync::Mutex<Option<serde_json::Value>>>() {
        *pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(payload.clone());
    }
    // A dialog is already open: nudge it with the new job (it stacks a modal).
    if app.get_webview_window("download-info").is_some() {
        let _ = app.emit_to("download-info", "dialog-update", &payload);
        return;
    }
    let win = match tauri::WebviewWindowBuilder::new(
        app,
        "download-info",
        tauri::WebviewUrl::App("index.html#/download-info".into()),
    )
    .title("Download File Info")
    .inner_size(540.0, 390.0)
    .min_inner_size(540.0, 380.0)
    .decorations(false)
    .transparent(false)
    .always_on_top(true)
    .center()
    .build()
    {
        Ok(w) => w,
        Err(_) => return,
    };
    let _ = win.show();
    let _ = win.set_focus();
}

/// Delete every `*.vtx.part` file that belongs to the given final file.
fn remove_one(_app: &tauri::AppHandle, state: &Arc<DlManager>, id: &str, delete_file: bool) {
    // Final-file + part-file cleanup both live inside remove_with_file now.
    state.remove_with_file(id, delete_file);
}

#[tauri::command]
async fn list_downloads(state: State<'_, Arc<DlManager>>) -> Result<Vec<download::DlView>, String> {
    Ok(state.views())
}

#[tauri::command]
async fn get_stats(state: State<'_, Arc<DlManager>>) -> Result<serde_json::Value, String> {
    Ok(state.stats())
}

#[tauri::command]
async fn fetch_ytdl_info(
    app: tauri::AppHandle,
    url: String,
    referer: Option<String>,
    user_agent: Option<String>,
) -> Result<ytdlp::YtdlInfo, String> {
    ytdlp::fetch_info(
        app,
        url,
        ytdlp::StreamCtx { referer, user_agent, cookies: None },
    )
    .await
}

#[tauri::command]
async fn grab_site(
    state: State<'_, Arc<DlManager>>,
    url: String,
    max_pages: Option<usize>,
    kinds: Option<Vec<String>>,
) -> Result<Vec<grabber::GrabItem>, String> {
    // Each grab starts with a fresh cancel flag (the Stop button flips it).
    let cancel = state.inner().grab_cancel.clone();
    cancel.store(false, std::sync::atomic::Ordering::Relaxed);
    grabber::grab_site(
        &url,
        max_pages.unwrap_or(10),
        kinds.unwrap_or_else(|| vec!["video".into(), "audio".into(), "document".into(), "archive".into(), "image".into()]),
        cancel.as_ref(),
    )
    .await
}

/// Abort the in-progress Site Grabber crawl.
#[tauri::command]
async fn grab_stop(state: State<'_, Arc<DlManager>>) -> Result<(), String> {
    state.inner().grab_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
async fn start_ytdl(
    app: tauri::AppHandle,
    state: State<'_, Arc<DlManager>>,
    url: String,
    format_id: String,
    save_path: String,
    include_playlist: Option<bool>,
    playlist_items: Option<String>,
    start_at: Option<u64>,
    embed_subs: Option<bool>,
    sub_langs: Option<String>,
    embed_thumbnail: Option<bool>,
    auto_subs: Option<bool>,
    referer: Option<String>,
    user_agent: Option<String>,
    cookies: Option<String>,
    start_paused: Option<bool>,
    allow_dup: Option<bool>,
) -> Result<download::DlView, String> {
    // Same video already in list → warn (covers 0% + youtu.be vs watch?v= ID variants).
    // In-flight guard: rapid double-click before the first task is visible in views().
    // Keep Both (allow_dup) bypasses all duplicate gates by design.
    let is_audio_new = format_id.starts_with("ba-") || format_id.starts_with("bestaudio");
    let allow = allow_dup == Some(true);
    if !allow {
        {
            use std::collections::HashSet;
            use std::sync::{LazyLock, Mutex};
            static PENDING_YTDL: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
            if let Some(id) = youtube_id(&url) {
                let key = format!("{}:{}", id.to_ascii_lowercase(), format_id);
                let mut pending = PENDING_YTDL.lock().unwrap();
                if pending.contains(&key) {
                    return Err(format!("EXISTS::pending:{key} (already starting)"));
                }
                pending.insert(key.clone());
                // Auto-clear after 15s so a failed start doesn't block forever.
                let key2 = key.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                    PENDING_YTDL.lock().unwrap().remove(&key2);
                });
            }
        }
    let views = state.views();
    if let Some(dup) = views.iter().find(|v| {
        if matches!(v.status, download::DlStatus::Completed | download::DlStatus::Cancelled) {
            return false;
        }
        // Same exact URL → always duplicate.
        if v.url == url {
            return true;
        }
        // Same YouTube ID but different format (MP4 vs MP3) is NOT duplicate — allow it.
        if let (Some(a), Some(b)) = (youtube_id(&v.url), youtube_id(&url)) {
            if !a.eq_ignore_ascii_case(&b) {
                return false;
            }
            // Same video ID: only block if exact same format/extension (MP4 vs MP3, 720p vs 1080p are different).
            let dup_fmt = v.format_id.as_deref().unwrap_or("");
            return dup_fmt == format_id;
        }
        false
    }) {
        return Err(format!("EXISTS::{} (already in list: {:?})", dup.save_path, dup.status));
    }
    // Already on disk (even if list says Completed) → warn like IDM, but only for exact same format/extension.
    // Different format (MP4 vs MP3, 720p vs 1080p) is a different file — don't warn.
    if let Some(id) = youtube_id(&url) {
        let has_same_format = views.iter().any(|v| {
            youtube_id(&v.url).map(|id2| id2.eq_ignore_ascii_case(&id)).unwrap_or(false)
                && v.format_id.as_deref().unwrap_or("") == format_id
        });
        // Only check disk if same format already exists in history; otherwise different format → allow.
        if has_same_format {
            let expect_ext = if is_audio_new { "mp3" } else { "mp4" };
            let base = std::path::PathBuf::from(save_path.trim());
            let eff = crate::state::save_dir_for(&base, &format!("[{id}].{expect_ext}"), true);
            let mut stack = vec![eff.clone(), base.clone()];
            let mut seen = std::collections::HashSet::new();
            while let Some(dir) = stack.pop() {
                if !seen.insert(dir.clone()) { continue; }
                let Ok(rd) = std::fs::read_dir(&dir) else { continue };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                        continue;
                    }
                    let name = e.file_name().to_string_lossy().to_string();
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                    if ext != expect_ext {
                        continue;
                    }
                    if name.contains(&format!("[{id}]")) || name.contains(&id) {
                        return Err(format!("EXISTS::{} (already on disk)", p.display()));
                    }
                }
            }
        }
    }
    }
    let settings = state::load_settings(&app);
    let task = ytdlp::start(
        app.clone(),
        url,
        format_id,
        save_path,
        settings.categorize_folders,
        settings.proxy.clone(),
        include_playlist.unwrap_or(false),
        playlist_items.unwrap_or_default(),
        start_at,
        embed_subs.unwrap_or(settings.embed_subs),
        sub_langs.unwrap_or_else(|| settings.sub_langs.clone()),
        embed_thumbnail.unwrap_or(settings.embed_thumbnail),
        auto_subs.unwrap_or(false),
        ytdlp::StreamCtx { referer, user_agent, cookies },
    )
    .await?;
    let id = task.id.clone();
    // "Download Later": register paused, launch on resume (resume_download).
    if start_paused.unwrap_or(false) {
        *task.status.write().unwrap() = crate::download::DlStatus::Paused;
        state.add_yt(task);
        let _ = app.emit("downloads-changed", ());
        return Ok(state.yt.lock().unwrap().get(&id).map(|t| t.view()).ok_or("task not found")?);
    }
    state.add_yt(task.clone());
    state.run_yt(task);
    Ok(state.yt.lock().unwrap().get(&id).map(|t| t.view()).ok_or("task not found")?)
}

#[tauri::command]
async fn get_tools_status(app: tauri::AppHandle) -> serde_json::Value {
    let (y, f, yv, fv) = tools::status(&app);
    serde_json::json!({
        "ytdlp": y,
        "ffmpeg": f,
        "ytdlp_version": yv,
        "ffmpeg_version": fv,
    })
}

/// Check for yt-dlp updates (fast; self-updater with a 15s cap). ffmpeg is a
/// ~111MB download that rarely changes, so it is only re-fetched when it is
/// entirely missing or `force_ffmpeg` is explicitly set.
#[tauri::command]
async fn update_tools(app: tauri::AppHandle, force_ffmpeg: bool) -> serde_json::Value {
    let y_old = tools::ytdlp_version(&app);
    let f_old = tools::ffmpeg_version(&app);
    let mut changed = false;
    let mut timed_out = false;
    let mut msgs: Vec<String> = Vec::new();

    // 1) yt-dlp self-update (the only thing that changes frequently).
    match tools::update_ytdlp(&app).await {
        Ok(()) => {
            let v = tools::ytdlp_version(&app);
            if y_old == v {
                if let Some(n) = &v {
                    msgs.push(format!("Tools are already up to date! (yt-dlp v{n})"));
                }
            } else {
                changed = true;
                msgs.push(match v {
                    Some(n) if y_old.is_none() => format!("yt-dlp installed (v{n})"),
                    Some(n) => format!("yt-dlp successfully updated to v{n}"),
                    None => "yt-dlp updated (version not reported)".into(),
                });
            }
        }
        Err(e) => {
            if e.to_lowercase().contains("timed out") {
                timed_out = true;
            }
            msgs.push(format!("yt-dlp update FAILED: {e}"));
        }
    }

    // 2) ffmpeg: skip unless missing or explicitly forced.
    if force_ffmpeg || f_old.is_none() {
        match tools::update_ffmpeg(&app).await {
            Ok(()) => {
                let v = tools::ffmpeg_version(&app);
                if f_old == v {
                    if let Some(n) = &v {
                        msgs.push(format!("ffmpeg is up to date (v{n})"));
                    }
                } else {
                    changed = true;
                    msgs.push(match v {
                        Some(n) => format!("ffmpeg updated to v{n}"),
                        None => "ffmpeg updated (version not reported)".into(),
                    });
                }
            }
            Err(e) => msgs.push(format!("ffmpeg update FAILED: {e}")),
        }
    }

    let (y, f, yv, fv) = tools::status(&app);
    let message = if timed_out {
        "Update check timed out. Please try again.".to_string()
    } else if changed {
        msgs.join(" • ")
    } else {
        match &yv {
            Some(n) => format!("Tools are already up to date! (yt-dlp v{n})"),
            None => "Tools are already up to date!".to_string(),
        }
    };
    serde_json::json!({
        "ytdlp": y,
        "ffmpeg": f,
        "ytdlp_version": yv,
        "ffmpeg_version": fv,
        "updated": changed,
        "message": message,
    })
}

#[tauri::command]
async fn get_settings(app: tauri::AppHandle) -> Settings {
    state::load_settings(&app)
}

#[tauri::command]
async fn save_settings(app: tauri::AppHandle, state: State<'_, Arc<DlManager>>, settings: Settings) -> Result<(), String> {
    state::save_settings(&app, &settings);
    state::apply_speed_limit(&state, settings.speed_limit);
    state.max_active.store(settings.max_active, std::sync::atomic::Ordering::Relaxed);
    set_autostart(settings.auto_start);
    Ok(())
}

#[tauri::command]
async fn choose_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
        .await
        .map_err(|e| e.to_string())?;
    Ok(picked.map(|p| p.to_string()))
}

/// Pick a Netscape-format cookies.txt file for yt-dlp.
#[tauri::command]
async fn choose_cookies_file(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("Cookie file", &["txt"])
            .add_filter("All files", &["*"])
            .blocking_pick_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(picked.map(|p| p.to_string()))
}

#[tauri::command]
async fn get_download_path(app: tauri::AppHandle) -> String {
    state::load_settings(&app).path
}

#[tauri::command]
async fn open_folder(path: String) -> Result<(), String> {
    state::open_in_folder(&path);
    Ok(())
}

#[tauri::command]
async fn open_saved_file(path: String) -> Result<(), String> {
    state::open_file(path)
}

/// Batch import: pick a .txt file and read its URLs (one per line).
#[tauri::command]
async fn read_urls(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("Text files", &["txt"])
            .blocking_pick_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = picked else { return Ok(vec![]) };
    let content = std::fs::read_to_string(path.to_string()).map_err(|e| e.to_string())?;
    let urls: Vec<String> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
        .map(|l| l.to_string())
        .collect();
    Ok(urls)
}

#[tauri::command]
fn get_ws_token(app: tauri::AppHandle) -> String {
    ws_server::ws_token(&app)
}

#[tauri::command]
fn window_action(app: tauri::AppHandle, action: String) {
    if let Some(win) = app.get_webview_window("main") {
        match action.as_str() {
            "minimize" => {
                let _ = win.minimize();
            }
            "toggle" => {
                if win.is_maximized().unwrap_or(false) {
                    let _ = win.unmaximize();
                } else {
                    let _ = win.maximize();
                }
            }
            "hide" => {
                let _ = win.hide();
            }
            "show" => {
                let _ = win.show();
                let _ = win.set_focus();
            }
            "close" => {
                let _ = win.close();
            }
            _ => {}
        }
    }
}

// ---------------- App entry ----------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        // Second launch focuses the running window instead of starting a
        // ghost instance whose downloads would be invisible (empty list).
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let show = |w: tauri::WebviewWindow| {
                let _ = w.show();
                #[cfg(desktop)]
                let _ = w.unminimize();
                let _ = w.set_focus();
            };
            if let Some(w) = app.get_webview_window("main") {
                show(w);
            } else if let Some((_, w)) = app.webview_windows().into_iter().next() {
                show(w);
            }
        }))
        .manage(Arc::new(DlManager::new()))
        // Pending payload for the standalone "Download File Info" dialog
        // window (taken once at boot via take_dialog_payload — race-free).
        .manage(std::sync::Mutex::new(None::<serde_json::Value>))
        .setup(|app| {
            let handle = app.handle();
            let settings = state::load_settings(handle);
            let mgr = app.state::<Arc<DlManager>>();
            state::apply_speed_limit(mgr.inner(), settings.speed_limit);
            mgr.max_active.store(settings.max_active, std::sync::atomic::Ordering::Relaxed);
            if settings.auto_start {
                set_autostart(true);
            }
            register_vortex_protocol();

            // Restore persisted session (terminal -> history, unfinished -> paused HTTP tasks).
            state::load_history(handle, mgr.inner());
            state::persist_loop(handle.clone(), mgr.inner().clone());
            state::sleep_block_loop(mgr.inner().clone());
            state::completion_watch_loop(handle.clone(), mgr.inner().clone());
            state::clipboard_monitor_loop(handle.clone(), mgr.inner().clone());

            // Minimize-to-tray + tray menu.
            if let Some(icon) = app.default_window_icon() {
                let show = MenuItem::with_id(app, "show", "Show Vortex", true, None::<&str>)?;
                let pause_all = MenuItem::with_id(app, "pause_all", "Pause All Downloads", true, None::<&str>)?;
                let resume_all = MenuItem::with_id(app, "resume_all", "Resume All Downloads", true, None::<&str>)?;
                let sep = MenuItem::with_id(app, "sep", "—", false, None::<&str>)?;
                let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show, &pause_all, &resume_all, &sep, &quit])?;
                let tray = TrayIconBuilder::with_id("vortex-tray")
                    .icon(icon.clone())
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .tooltip("Vortex — Download Manager")
                    .on_menu_event(|app, event| match event.id().as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.unminimize();
                                let _ = w.set_focus();
                            }
                        }
                        "pause_all" => {
                            let mgr = app.state::<Arc<DlManager>>().inner().clone();
                            let app2 = app.clone();
                            tauri::async_runtime::spawn(async move {
                                pause_all_inner(&app2, mgr).await;
                            });
                        }
                        "resume_all" => {
                            let mgr = app.state::<Arc<DlManager>>().inner().clone();
                            let app2 = app.clone();
                            tauri::async_runtime::spawn(async move {
                                let n = mgr.resume_all();
                                if n > 0 {
                                    let _ = app2.emit("downloads-changed", ());
                                }
                            });
                        }
                        "quit" => {
                            // Flush in-flight progress to disk before exiting
                            // (the 2 s persist timer alone would lose the tail).
                            let mgr = app.state::<Arc<DlManager>>().inner().clone();
                            state::persist_history(&app, &mgr);
                            app.exit(0);
                        }
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            let app = tray.app_handle();
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.unminimize();
                                let _ = w.set_focus();
                            }
                        }
                    })
                    .build(app)?;
                let _ = app.manage(tray);
            }

            // Close = hide to tray (quit via tray menu).
            if let Some(win) = app.get_webview_window("main") {
                let hide_win = win.clone();
                win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = hide_win.hide();
                    }
                });
            }

            if let Some(arg) = std::env::args().nth(1) {
                if arg.starts_with("vortex://") {
                    if let Some(cap) = parse_capture_arg(&arg) {
                        handle_capture(handle, cap);
                    }
                }
            }
            let h = handle.clone();
            tauri::async_runtime::spawn(async move {
                ws_server::run(h).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_download,
            pause_download,
            resume_download,
            set_auth,
            remove_credential,
            retry_all_downloads,
            resume_all_downloads,
            pause_all_downloads,
            cancel_shutdown,
            cancel_all_active,
            cancel_download,
            remove_download,
            downloads_action,
            list_downloads,
            get_stats,
            fetch_ytdl_info,
            start_ytdl,
            grab_site,
            grab_stop,
            get_tools_status,
            update_tools,
            get_settings,
            save_settings,
            delete_file_at,
            ytdl_expected_path,
            probe_download_info,
            choose_folder,
            choose_cookies_file,
            get_download_path,
            open_folder,
            open_saved_file,
            read_urls,
            get_ws_token,
            window_action,
            probe_download_info,
            take_dialog_payload,
            ytdl_expected_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Vortex");
}