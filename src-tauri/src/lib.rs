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
            let info = ytdlp::fetch_info(app.clone(), cap.url.clone()).await.ok()?;
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
) -> Result<download::DlView, String> {
    let settings = state::load_settings(&app);
    let opts = download::StartOpts {
        segments,
        filename,
        categorize: settings.categorize_folders,
        start_at,
        auto_retries: settings.auto_retries,
        proxy: settings.proxy.clone(),
        referer: None,
        cookies: None,
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
async fn pause_download(state: State<'_, Arc<DlManager>>, id: String) -> Result<(), String> {
    let m = state.http.lock().unwrap();
    if let Some(t) = m.get(&id) {
        t.paused.store(true, std::sync::atomic::Ordering::Relaxed);
        return Ok(());
    }
    drop(m);
    let m2 = state.yt.lock().unwrap();
    if let Some(t) = m2.get(&id) {
        t.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
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
async fn resume_download(state: State<'_, Arc<DlManager>>, id: String) -> Result<(), String> {
    let m = state.http.lock().unwrap();
    if let Some(t) = m.get(&id) {
        t.paused.store(false, std::sync::atomic::Ordering::Relaxed);
        let c = t.clone();
        drop(m);
        state.run_http(c);
    }
    // youtube tasks are process-bound; resuming restarts via the HTTP path only
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
    // Cancel: stop download but keep entry in history as cancelled
    let should_emit = {
        let m = state.http.lock().unwrap();
        if let Some(t) = m.get(&id) {
            t.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            t.paused.store(false, std::sync::atomic::Ordering::Relaxed);
            true
        } else {
            drop(m);
            let m2 = state.yt.lock().unwrap();
            if let Some(t) = m2.get(&id) {
                t.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                true
            } else { false }
        }
    };
    if should_emit { let _ = app.emit("downloads-changed", ()); }
    Ok(())
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

/// Delete every `*.vtx.part` file that belongs to the given final file.
fn cleanup_parts_for(save_path: &std::path::Path) {
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

fn remove_one(app: &tauri::AppHandle, state: &Arc<DlManager>, id: &str, delete_file: bool) {
    let settings = state::load_settings(app);
    // Grab the target path before the task is dropped from the lists.
    let save_path = state
        .http
        .lock()
        .unwrap()
        .get(id)
        .map(|t| t.save_path.clone())
        .or_else(|| state.yt.lock().unwrap().get(id).map(|t| t.save_path.lock().unwrap().clone()))
        .or_else(|| {
            state
                .history
                .lock()
                .unwrap()
                .iter()
                .find(|h| h.id == id)
                .map(|h| std::path::PathBuf::from(&h.save_path))
        });
    state.remove_with_file(id, delete_file);
    if settings.delete_part {
        if let Some(p) = save_path {
            cleanup_parts_for(&p);
        }
    }
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
async fn fetch_ytdl_info(app: tauri::AppHandle, url: String) -> Result<ytdlp::YtdlInfo, String> {
    ytdlp::fetch_info(app, url).await
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
) -> Result<download::DlView, String> {
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
    )
    .await?;
    let id = task.id.clone();
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
    state::open_file(path);
    Ok(())
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
        .manage(Arc::new(DlManager::new()))
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
            choose_folder,
            choose_cookies_file,
            get_download_path,
            open_folder,
            open_saved_file,
            read_urls,
            get_ws_token,
            window_action,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Vortex");
}