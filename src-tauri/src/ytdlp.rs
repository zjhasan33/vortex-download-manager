use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::download::{self, DlStatus};
use crate::tools;

#[derive(Clone, Serialize)]
pub struct YtdlFormat {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub quality: String,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub ext: String,
    pub size: u64,
    pub has_video: bool,
    pub has_audio: bool,
    pub note: String,
}

#[derive(Clone, Serialize)]
pub struct YtdlSub {
    pub lang: String,
    pub label: String,
    pub auto: bool,
}

#[derive(Clone, Serialize)]
pub struct YtdlInfo {
    pub title: String,
    pub thumbnail: String,
    pub duration: u64,
    pub uploader: String,
    pub view_count: u64,
    pub formats: Vec<YtdlFormat>,
    pub subtitles: Vec<YtdlSub>,
}

#[derive(Deserialize)]
struct RawFormat {
    format_id: String,
    ext: String,
    vcodec: Option<String>,
    acodec: Option<String>,
    height: Option<u32>,
    fps: Option<u32>,
    filesize: Option<u64>,
    filesize_approx: Option<u64>,
    #[allow(dead_code)]
    note: Option<String>,
}

fn mb(s: &str) -> u64 {
    let n: f64 = s.parse().unwrap_or(0.0);
    (n * 1024.0 * 1024.0) as u64
}

/// Parse "[download] 12.3% of ~25.00MiB at 2.4MiB/s ETA 00:10"
fn parse_progress(line: &str) -> Option<(f64, u64, u64)> {
    if !line.contains("[download]") || !line.contains('%') {
        return None;
    }
    let pct = {
        let mut it = line.split('%');
        let head = it.next()?;
        let n = head.split_whitespace().last()?.parse::<f64>().ok()?;
        n
    };
    let mut total = 0u64;
    let mut speed = 0u64;
    if let Some(idx) = line.find(" of ") {
        let rest = &line[idx + 4..];
        let tail = rest.split(" at ").next().unwrap_or(rest).trim();
        // tail like "~25.00MiB" or "25.00MiB"
        let t = tail.trim_start_matches('~');
        let dt = parse_size(t);
        if let Some(at) = rest.find(" at ") {
            let sp = &rest[at + 4..];
            let sp = sp.split_whitespace().next().unwrap_or("");
            let spnum = sp.trim_end_matches("/s");
            let (num, unit) = split_num_unit(spnum);
            speed = (num * unit_mult(unit)) as u64;
        }
        let _ = dt;
        total = if tail.starts_with('~') { 0 } else { parse_size(tail) };
    }
    // Ignore implausibly tiny totals (e.g. mis-parsed informational lines).
    if total > 0 && total < 1024 {
        total = 0;
    }
    Some((pct, total, speed))
}

fn parse_size(s: &str) -> u64 {
    let (num, unit) = split_num_unit(s);
    (num * unit_mult(unit)) as u64
}

fn split_num_unit(s: &str) -> (f64, &str) {
    let split = s.find(|c: char| c.is_ascii_alphabetic());
    match split {
        Some(i) => (s[..i].parse().unwrap_or(0.0), &s[i..]),
        None => (s.parse().unwrap_or(0.0), ""),
    }
}

fn unit_mult(u: &str) -> f64 {
    match u.trim() {
        "KiB" | "K" | "KB" => 1024.0,
        "MiB" | "M" | "MB" => 1024.0 * 1024.0,
        "GiB" | "G" | "GB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" | "T" | "TB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "iB" | "B" => 1.0,
        _ => 1.0,
    }
}

/// Build cookie-related yt-dlp args from settings.
/// Toggle ON + valid file -> use the cookies.txt; otherwise skip cookies entirely.
fn cookie_args(use_cookies: bool, path: &str) -> Vec<String> {
    if use_cookies {
        let p = path.trim();
        if !p.is_empty() && std::path::Path::new(p).is_file() {
            return vec!["--cookies".into(), p.to_string()];
        }
    }
    vec!["--no-cookies-from-browser".into()]
}

pub async fn fetch_info(app: AppHandle, url: String) -> Result<YtdlInfo, String> {
    let bin = tools::ensure_ytdlp(&app).await?;
    let settings = crate::state::load_settings(&app);
    let cooks = cookie_args(settings.use_cookies, &settings.cookies);
    let out = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let mut cmd = crate::tools::silent(Command::new(&bin));
        cmd.args(["--dump-single-json", "--no-warnings", "--no-playlist"]);
        cmd.args(&cooks);
        cmd.arg(&url);
        let o = cmd.output().map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    })
    .await
    .map_err(|e| e.to_string())??;

    let v: Value = serde_json::from_str(&out).map_err(|_| "Could not parse video info".to_string())?;

    let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("Unknown").to_string();
    let thumbnail = v
        .get("thumbnail")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let duration = v.get("duration").and_then(|x| x.as_u64()).unwrap_or(0);
    let uploader = v
        .get("uploader")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("channel").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    let view_count = v.get("view_count").and_then(|x| x.as_u64()).unwrap_or(0);

    let raw: Vec<RawFormat> = v
        .get("formats")
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter().filter_map(|f| serde_json::from_value(f.clone()).ok()).collect())
        .unwrap_or_default();

    let formats = build_formats(raw);
    if formats.is_empty() {
        return Err("No downloadable formats found.".into());
    }

    let mut subtitles: Vec<YtdlSub> = Vec::new();
    let mut manual_langs: Vec<String> = Vec::new();
    // Manual (uploader-provided) subtitles first.
    if let Some(map) = v.get("subtitles").and_then(|x| x.as_object()) {
        for (lang, _arr) in map {
            manual_langs.push(lang.clone());
            subtitles.push(YtdlSub { lang: lang.clone(), label: lang.to_uppercase(), auto: false });
        }
    }
    // Automatic captions (YT-generated) – add only when no manual sub for that language.
    if let Some(map) = v.get("automatic_captions").and_then(|x| x.as_object()) {
        for (lang, _arr) in map {
            if manual_langs.contains(lang) {
                continue;
            }
            subtitles.push(YtdlSub { lang: lang.clone(), label: format!("{} (auto)", lang.to_uppercase()), auto: true });
        }
    }
    subtitles.sort_by(|a, b| a.label.cmp(&b.label));

    Ok(YtdlInfo {
        title,
        thumbnail,
        duration,
        uploader,
        view_count,
        formats,
        subtitles,
    })
}

fn build_formats(raw: Vec<RawFormat>) -> Vec<YtdlFormat> {
    let has_v = |f: &RawFormat| f.vcodec.as_deref().is_some_and(|v| v != "none");
    let has_a = |f: &RawFormat| f.acodec.as_deref().is_some_and(|a| a != "none");

    let mut out: Vec<YtdlFormat> = Vec::new();
    let maxh = raw.iter().filter(|f| has_v(f)).filter_map(|f| f.height).max().unwrap_or(0);
    let audio_size = raw
        .iter()
        .filter(|f| has_a(f) && !has_v(f))
        .map(|f| f.filesize.or(f.filesize_approx).unwrap_or(0))
        .max()
        .unwrap_or(0);

    if maxh > 0 {
        let mut targets: Vec<u32> = Vec::new();
        for h in [maxh, 2160, 1440, 1080, 720, 480, 360] {
            if h > 0 && !targets.contains(&h) {
                targets.push(h);
            }
        }
        for h in targets {
            let prog = raw
                .iter()
                .filter(|f| has_v(f) && has_a(f) && f.height == Some(h))
                .max_by_key(|f| f.filesize.or(f.filesize_approx).unwrap_or(0));
            let dashv = raw
                .iter()
                .filter(|f| has_v(f) && !has_a(f) && f.height == Some(h))
                .max_by_key(|f| f.filesize.or(f.filesize_approx).unwrap_or(0));
            let size = if let Some(p) = prog {
                p.filesize.or(p.filesize_approx).unwrap_or(0)
            } else if let Some(d) = dashv {
                d.filesize.or(d.filesize_approx).unwrap_or(0).saturating_add(audio_size)
            } else {
                0
            };
            let pick = prog.or(dashv);
            let id = if let Some(p) = prog {
                p.format_id.clone()
            } else {
                format!("bestvideo[height<={h}]+bestaudio/best[height<={h}]")
            };
            out.push(YtdlFormat {
                id,
                label: format!("Video • {}p{}", h, if h == maxh { " (Best)" } else { "" }),
                kind: "video".into(),
                quality: format!("{}p", h),
                height: Some(h),
                fps: pick.and_then(|f| f.fps),
                ext: prog.map(|p| p.ext.clone()).unwrap_or_else(|| "mp4".into()),
                size,
                has_video: true,
                has_audio: true,
                note: if h == maxh { "Recommended".into() } else { "".into() },
            });
        }
    }

    // Audio only (original stream, no re-encode)
    let audio_est = audio_size.max(mb("3"));
    out.push(YtdlFormat {
        id: "bestaudio/best".into(),
        label: "Audio • Best (original)".into(),
        kind: "audio".into(),
        quality: "Best".into(),
        height: None,
        fps: None,
        ext: "m4a".into(),
        size: audio_est,
        has_video: false,
        has_audio: true,
        note: "".into(),
    });

    // Audio conversions (ffmpeg). id: ba-audio-<container>-<quality>
    // (container, quality code, short quality, size ratio, full label)
    let convs: &[(&str, &str, &str, f64, &str)] = &[
        ("m4a", "256", "256 kbps", 0.16, "M4A • 256 kbps (AAC)"),
        ("mp3", "320", "320 kbps", 0.17, "MP3 • 320 kbps"),
        ("mp3", "192", "192 kbps", 0.10, "MP3 • 192 kbps"),
        ("mp3", "128", "128 kbps", 0.07, "MP3 • 128 kbps"),
        ("opus", "160", "160 kbps", 0.10, "Opus • 160 kbps"),
        ("flac", "0", "Lossless", 0.55, "FLAC • Lossless"),
        ("wav", "0", "Lossless", 0.90, "WAV • Lossless"),
    ];
    for (container, q, shortq, ratio, full) in convs {
        out.push(YtdlFormat {
            id: format!("ba-audio-{container}-{q}"),
            label: (*full).into(),
            kind: "audio".into(),
            quality: (*shortq).into(),
            height: None,
            fps: None,
            ext: (*container).into(),
            size: (audio_est as f64 * ratio) as u64,
            has_video: false,
            has_audio: true,
            note: "".into(),
        });
    }

    let _ = mb("0");
    out
}

/// Parse an audio-conversion format id -> (container, quality code).
/// Accepts both `ba-audio-<container>-<q>` and the legacy `ba-mp3-<q>`.
fn parse_audio_fmt(format_id: &str) -> Option<(String, String)> {
    if let Some(rest) = format_id.strip_prefix("ba-audio-") {
        if let Some((container, q)) = rest.split_once('-') {
            if !container.is_empty() {
                return Some((container.to_string(), q.to_string()));
            }
        }
    }
    if let Some(q) = format_id.strip_prefix("ba-mp3-") {
        return Some(("mp3".into(), q.to_string()));
    }
    None
}

pub struct YtTask {
    pub id: String,
    pub url: String,
    pub format_id: String,
    pub include_playlist: bool,
    pub playlist_items: String,
    pub proxy: String,
    pub start_at: Option<u64>,
    pub embed_subs: bool,
    pub sub_langs: String,
    pub title: Mutex<String>,
    pub filename: Mutex<String>,
    pub category: String,
    pub save_path: Mutex<PathBuf>,
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub speed: AtomicU64,
    pub last_pct: AtomicU64,
    pub status: RwLock<DlStatus>,
    pub error: Mutex<Option<String>>,
    pub cancel: AtomicBool,
    pub thumb: Option<String>,
    pub app: AppHandle,
    pub created_at: u64,
}

impl YtTask {
    pub fn view(&self) -> download::DlView {
        let status = *self.status.read().unwrap();
        let total = self.total.load(Ordering::Relaxed);
        let done = self.done.load(Ordering::Relaxed);
        let speed = self.speed.load(Ordering::Relaxed);
        let progress = if total > 0 { (done as f64 / total as f64) * 100.0 } else { 0.0 };
        let eta = if speed > 0 && total > done { (total - done) / speed } else { 0 };
        let title = self.title.lock().unwrap().clone();
        let filename = self.filename.lock().unwrap().clone();
        let path = self.save_path.lock().unwrap();
        download::DlView {
            id: self.id.clone(),
            url: self.url.clone(),
            filename,
            title: title.clone(),
            category: self.category.clone(),
            total_size: total,
            downloaded: done,
            speed,
            progress: progress.min(100.0),
            eta,
            segments: 1,
            connections: 1,
            status,
            error: self.error.lock().unwrap().clone(),
            thumbnail: self.thumb.clone(),
            source: "youtube".into(),
            save_path: path.display().to_string(),
            created_at: self.created_at,
            format_id: Some(self.format_id.clone()),
        }
    }

    fn set_status(&self, s: DlStatus) {
        *self.status.write().unwrap() = s;
        let err = self.error.lock().unwrap().clone();
        let _ = self.app.emit(
            "download-status",
            serde_json::json!({ "id": self.id, "status": format!("{:?}", s).to_lowercase(), "error": err }),
        );
        let _ = self.app.emit("downloads-changed", ());
        if s == DlStatus::Completed {
            let settings = crate::state::load_settings(&self.app);
            if settings.notifications {
                let filename = self.filename.lock().unwrap().clone();
                crate::state::notify_done(&self.app, &format!("{filename} — completed"), "Download finished");
            }
        }
    }
}

/// Prepare a YouTube download task (does NOT spawn; use launch()).
pub async fn start(
    app: AppHandle,
    url: String,
    format_id: String,
    save_path: String,
    categorize: bool,
    proxy: String,
    include_playlist: bool,
    playlist_items: String,
    start_at: Option<u64>,
    embed_subs: bool,
    sub_langs: String,
) -> Result<Arc<YtTask>, String> {
    let bin = tools::ensure_ytdlp(&app).await?;
    let _ = &bin;
    let is_subs = format_id.starts_with("subs:");
    let audio_fmt = parse_audio_fmt(&format_id);
    let use_merge = !is_subs
        && audio_fmt.is_none()
        && (format_id.contains("+") || format_id.starts_with("bestvideo") || format_id.contains("[height"));
    let is_audio = audio_fmt.is_some() || format_id.starts_with("bestaudio");
    let is_video = !is_subs && !is_audio;
    let wants_embed = is_video && embed_subs && !sub_langs.trim().is_empty();
    if use_merge || audio_fmt.is_some() || wants_embed {
        let _ = tools::ensure_ffmpeg(&app).await?;
    }
    // mp3 conversion also needs a final container rename to .mp3; ensure we handle it.

    let id = uuid::Uuid::new_v4().to_string();
    let base = PathBuf::from(&save_path);
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    // Subtitle downloads go into a "Subtitles" subfolder when categorization is on.
    let cat = if is_subs { "subtitle" } else if is_audio { "audio" } else { "video" };
    let eff_dir = if categorize {
        base.join(crate::state::category_folder(cat))
    } else {
        base
    };
    std::fs::create_dir_all(&eff_dir).map_err(|e| e.to_string())?;
    let prefix = format!("vx_{}", id);
    let tmpl = if include_playlist || !playlist_items.trim().is_empty() {
        eff_dir.join(format!("{0}_%(playlist_index)03d_%(title).80s [%(id)s].%(ext)s", prefix))
    } else if is_subs {
        // Subs are per-language; keep the language in the filename template.
        eff_dir.join(format!("{0}_%(title).80s [%(id)s].%(ext)s", prefix))
    } else {
        eff_dir.join(format!("{0}_%(title).80s [%(id)s].%(ext)s", prefix))
    };
    let title_hint = if is_subs {
        "YouTube subtitles".to_string()
    } else if is_audio {
        "YouTube audio".to_string()
    } else {
        "YouTube video".to_string()
    };

    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let task = Arc::new(YtTask {
        title: Mutex::new(title_hint.clone()),
        filename: Mutex::new("downloading…".into()),
        category: cat.into(),
        save_path: Mutex::new(tmpl),
        total: AtomicU64::new(0),
        done: AtomicU64::new(0),
        speed: AtomicU64::new(0),
        last_pct: AtomicU64::new(0),
        status: RwLock::new(DlStatus::Queued),
        error: Mutex::new(None),
        cancel: AtomicBool::new(false),
        thumb: None,
        id,
        url,
        format_id,
        include_playlist,
        playlist_items,
        proxy,
        start_at,
        embed_subs,
        sub_langs,
        app,
        created_at,
    });

    Ok(task)
}

/// Actually run the yt-dlp job (respects the concurrency gate from outside).
pub async fn launch(task: Arc<YtTask>) {
    if let Some(ta) = task.start_at {
        let now = download::now_ms();
        if now < ta {
            task.set_status(DlStatus::Queued);
            while !task.cancel.load(Ordering::Relaxed) && download::now_ms() < ta {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if task.cancel.load(Ordering::Relaxed) {
                task.set_status(DlStatus::Cancelled);
                return;
            }
        }
    }
    task.set_status(DlStatus::Downloading);
    let bin = match tools::ensure_ytdlp(&task.app).await {
        Ok(b) => b,
        Err(e) => {
            *task.error.lock().unwrap() = Some(e);
            task.set_status(DlStatus::Error);
            return;
        }
    };
    let _ = ytdlp_run(task.clone(), &bin).await;
}

/// Run yt-dlp and stream progress from its stdout.
pub(crate) async fn ytdlp_run(task: Arc<YtTask>, bin: &std::path::Path) -> Result<(), String> {
    let watch = tokio::spawn(progress_watch(task.clone()));
    let audio_fmt = parse_audio_fmt(&task.format_id);
    let is_subs = task.format_id.starts_with("subs:");

    let mut args: Vec<String> = vec![
        "--newline".into(),
        "--progress-delta".into(),
        "0.2".into(),
        "--no-warnings".into(),
    ];
    if task.include_playlist {
        args.push("--yes-playlist".into());
        let items = task.playlist_items.trim().to_string();
        if !items.is_empty() {
            args.push("--playlist-items".into());
            args.push(items);
        }
    } else {
        args.push("--no-playlist".into());
    }
    if is_subs {
        // Subtitle-only download. format_id = "subs:<fmt>:<lang>"; fmt optional (default srt).
        let spec = task.format_id.trim_start_matches("subs:").to_string();
        let (sub_fmt, langs) = match spec.split_once(':') {
            Some((f, l)) => (f.to_string(), l.to_string()),
            None => ("srt".to_string(), spec),
        };
        let sub_fmt = if sub_fmt.trim().is_empty() { "srt".to_string() } else { sub_fmt };
        args.push("--skip-download".into());
        args.push("--write-subs".into());
        args.push("--write-auto-subs".into());
        args.push("--sub-langs".into());
        args.push(if langs.is_empty() || langs == "all" { "all".into() } else { langs });
        args.push("--sub-format".into());
        args.push("srt/vtt/ass/best".into());
        args.push("--convert-subs".into());
        args.push(sub_fmt);
        args.push("-f".into());
        args.push("bestvideo/best".into());
    } else {
        args.push("-N".into());
        args.push("16".into());
        if let Some((container, quality)) = &audio_fmt {
            // Convert the best audio stream to the requested container.
            args.push("--extract-audio".into());
            args.push("--audio-format".into());
            args.push(container.clone());
            if container != "flac" && container != "wav" {
                args.push("--audio-quality".into());
                args.push(if quality.is_empty() { "0".into() } else { quality.clone() });
            }
            args.push("-f".into());
            args.push("bestaudio/best".into());
        } else {
            args.push("--merge-output-format".into());
            args.push("mp4".into());
            args.push("-f".into());
            args.push(task.format_id.clone());
            // Auto-embed one subtitle track so the video is self-contained (IDM-like).
            if task.embed_subs && !task.sub_langs.trim().is_empty() {
                args.push("--sub-format".into());
                args.push("srt/vtt/ass/best".into());
                args.push("--embed-subs".into());
                args.push("--sub-langs".into());
                args.push(task.sub_langs.trim().to_string());
                args.push("--convert-subs".into());
                args.push("srt".into());
            }
        }
    }
    if !task.proxy.trim().is_empty() {
        args.push("--proxy".into());
        args.push(task.proxy.trim().to_string());
    }
    let settings = crate::state::load_settings(&task.app);
    args.extend(cookie_args(settings.use_cookies, &settings.cookies));
    args.push("-o".into());
    args.push(task.save_path.lock().unwrap().to_string_lossy().into_owned());
    args.push(task.url.clone());

    let mut child = crate::tools::silent(std::process::Command::new(bin))
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    // read stdout for progress until EOF
    if let Some(stdout) = child.stdout.take() {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            if task.cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                break;
            }
            let Ok(line) = line else { continue };
            if let Some((pct, total, speed)) = parse_progress(&line) {
                if total > 0 {
                    task.total.store(total, Ordering::Relaxed);
                    let done = (total as f64 * pct / 100.0) as u64;
                    task.done.store(done, Ordering::Relaxed);
                } else {
                    task.done.store(task.done.load(Ordering::Relaxed).max(task.total.load(Ordering::Relaxed)), Ordering::Relaxed);
                }
                task.speed.store(speed, Ordering::Relaxed);
                task.last_pct.store(pct as u64, Ordering::Relaxed);
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    watch.abort();

    if task.cancel.load(Ordering::Relaxed) {
        task.set_status(DlStatus::Cancelled);
        let sp = task.save_path.lock().unwrap();
        if let Some(f) = sp.parent().map(|p| p.join(format!("{}.part", sp.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()))) {
            let _ = std::fs::remove_file(f);
        }
        return Ok(());
    }

    if status.success() {
        // Locate the built output file(s) via the unique prefix and record the real path.
        let dir = task.save_path.lock().unwrap().parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let prefix = format!("vx_{}_", task.id);
        let outputs = find_outputs(&dir, &prefix);
        if !outputs.is_empty() {
            // Strip the temporary prefix from final filename(s).
            let mut real_names: Vec<String> = Vec::new();
            for real in &outputs {
                let clean = real.file_name().map(|s| s.to_string_lossy()[prefix.len()..].to_string()).unwrap_or_default();
                if !clean.is_empty() {
                    let new_path = real.with_file_name(&clean);
                    if std::fs::rename(&real, &new_path).is_ok() {
                        real_names.push(new_path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
                    }
                }
            }
            let first = dir.join(real_names.first().cloned().unwrap_or_default());
            if real_names.len() == 1 {
                let fname = real_names[0].clone();
                *task.filename.lock().unwrap() = fname.clone();
                *task.title.lock().unwrap() = first
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                *task.save_path.lock().unwrap() = first.clone();
            } else {
                *task.filename.lock().unwrap() = format!("{} subtitles", outputs.len());
                *task.title.lock().unwrap() = dir
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                *task.save_path.lock().unwrap() = dir.join(real_names.join(", "));
            }
            let _ = task.app.emit("downloads-changed", ());
        } else {
            *task.error.lock().unwrap() = Some("Output file not found".into());
        }
        let sz = std::fs::metadata(&*task.save_path.lock().unwrap())
            .map(|m| m.len())
            .unwrap_or(0);
        if sz > 0 {
            task.total.store(sz, Ordering::Relaxed);
            task.done.store(sz, Ordering::Relaxed);
        } else {
            task.done.store(task.total.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        task.set_status(DlStatus::Completed);
    } else {
        *task.error.lock().unwrap() = Some("Download failed".into());
        task.set_status(DlStatus::Error);
    }
    Ok(())
}

fn find_outputs(dir: &std::path::Path, prefix: &str) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.starts_with(prefix))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

pub async fn progress_watch(task: Arc<YtTask>) {
    let mut last = Instant::now();
    let mut last_done = task.done.load(Ordering::Relaxed);
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = task.done.load(Ordering::Relaxed);
        let speed = ((now.saturating_sub(last_done)) as f64 / last.elapsed().as_secs_f64().max(0.2)) as u64;
        task.speed.store(speed, Ordering::Relaxed);
        last = Instant::now();
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
        if st == DlStatus::Completed || st == DlStatus::Cancelled || st == DlStatus::Error {
            break;
        }
    }
}