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
    /// True when the analyzed URL refers to a YouTube playlist.
    #[serde(default)]
    pub playlist: bool,
    /// Number of videos in the playlist (best-effort).
    #[serde(default)]
    pub playlist_count: Option<u64>,
    /// Playlist title (best-effort).
    #[serde(default)]
    pub playlist_title: Option<String>,
}

/// True when `url` is a YouTube/YouTube-mirror playlist link:
/// a `/playlist` page or any watch URL carrying the `list` query param.
fn is_playlist_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !(lower.contains("youtube.com")
        || lower.contains("youtu.be")
        || lower.contains("youtube-nocookie.com"))
    {
        return false;
    }
    lower.contains("/playlist?") || lower.contains("&list=") || lower.contains("?list=")
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
        // tail like "~25.00MiB" (approximate) or "25.00MiB" — the leading `~`
        // is informational; parse the size either way.
        total = parse_size(tail.trim_start_matches('~'));
        if let Some(at) = rest.find(" at ") {
            let sp = &rest[at + 4..];
            let sp = sp.split_whitespace().next().unwrap_or("");
            let spnum = sp.trim_end_matches("/s");
            let (num, unit) = split_num_unit(spnum);
            speed = (num * unit_mult(unit)) as u64;
        }
    }
    // Ignore implausibly tiny totals (e.g. mis-parsed informational lines).
    if total > 0 && total < 1024 {
        total = 0;
    }
    Some((pct, total, speed))
}

/// Parse a `--progress-template` line:
///   `__VX_PROG__:<downloaded>:<total>:<speed>:<eta>`
/// yt-dlp prints unresolvable fields as `NA`/`None`, speed may be a raw number
/// or a human string (`2.39MiB/s`), and eta may be seconds or `MM:SS`/`HH:MM:SS`.
/// Returns (downloaded, total, speed_bps, eta_secs); unparseable fields are 0.
fn parse_progress_vx(line: &str) -> Option<(u64, u64, u64, u64)> {
    let rest = line.strip_prefix("__VX_PROG__:")?;
    // ETA itself may contain colons (MM:SS), so split off only the first 3 fields.
    let mut it = rest.splitn(4, ':');
    let nx = |s: &str| -> u64 {
        let t = s.trim();
        if t.is_empty() {
            return 0;
        }
        if let Ok(v) = t.parse::<f64>() {
            return v.max(0.0) as u64;
        }
        parse_size(t.trim_end_matches("/s"))
    };
    let downloaded = nx(it.next()?);
    let total = nx(it.next().unwrap_or(""));
    let speed = nx(it.next().unwrap_or(""));
    let eta = parse_eta(it.next().unwrap_or(""));
    Some((downloaded, total, speed, eta))
}

/// Extract the video id from a per-item marker: per video yt-dlp logs
/// `[info] <id>: Downloading N format(s)` right before fetching it. Counting
/// UNIQUE ids gives a live "k / N videos" indicator for playlist jobs (one
/// line per stream would otherwise double-count merged video+audio).
fn playlist_item_id(line: &str) -> Option<&str> {
    let s = line.strip_prefix("[info]")?.trim_start();
    let (id, rest) = s.split_once(':')?;
    let id = id.trim();
    if id.is_empty() || id.contains(char::is_whitespace) {
        return None;
    }
    // Note: real lines carry a format suffix ("...format(s): 248+251"), so
    // match containment, not end-of-line.
    if rest.contains("Downloading ") && rest.contains("format(s)") {
        Some(id)
    } else {
        None
    }
}

/// Parse `[download] Downloading video 3 of 12` → (3, 12). yt-dlp logs one
/// per playlist item, carrying BOTH numbers — so jobs whose total was never
/// learned upfront (no playlist_title template) still show live "k / N".
fn playlist_progress(line: &str) -> Option<(u64, u64)> {
    let s = line.strip_prefix("[download]")?.trim_start();
    let s = s.strip_prefix("Downloading video")?.trim_start();
    let (k, rest) = s.split_once("of")?;
    let k = k.trim().parse::<u64>().ok()?;
    let n = rest.trim().split_whitespace().next()?.parse::<u64>().ok()?;
    if k == 0 || n == 0 || k > n {
        return None;
    }
    Some((k, n))
}

/// Parse an ETA that yt-dlp may render as seconds (`42`), `MM:SS`, or
/// `HH:MM:SS`. Returns 0 for `NA`/`None`/empty.
fn parse_eta(s: &str) -> u64 {
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("na") || t.eq_ignore_ascii_case("none") {
        return 0;
    }
    if !t.contains(':') {
        return t.parse::<f64>().map(|v| v.max(0.0) as u64).unwrap_or(0);
    }
    let mut acc = 0u64;
    for part in t.split(':') {
        let v = part.trim().parse::<f64>().map(|v| v.max(0.0) as u64).unwrap_or(0);
        acc = acc.saturating_mul(60).saturating_add(v);
    }
    acc
}

/// Whether the bundled yt-dlp supports `--progress-template` (added in 2023.05)
/// and the `progress.downloaded_bytes` template fields (added in 2023.11).
/// Checked fresh on every download: caching a negative result would permanently
/// disable live progress for the whole session when the app starts before
/// yt-dlp has been auto-downloaded on first run. One `--version` spawn per
/// download is negligible.
fn ytdlp_has_progress_template(app: &AppHandle) -> bool {
    let Some(v) = tools::ytdlp_version(app) else {
        return false;
    };
    let mut it = v.split('.');
    let y: u32 = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let m: u32 = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let d: u32 = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    (y, m, d) >= (2023, 11, 1)
}

fn parse_size(s: &str) -> u64 {
    let (num, unit) = split_num_unit(s);
    (num * unit_mult(unit)) as u64
}

/// Parse a size yt-dlp printed as either raw bytes or a human string like
/// "250.50MiB" / "~250.50MiB" (approximate). None when empty/unparseable.
fn parse_size_or_bytes(s: &str) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(b) = t.parse::<u64>() {
        return Some(b);
    }
    let v = parse_size(t.trim_start_matches('~'));
    if v > 0 {
        Some(v)
    } else {
        None
    }
}

/// Drop a trailing " [<video-id>]" disambiguator ("Title [AbC123XYZ].mp4" ->
/// "Title.mp4") so the final file name is a clean title. The temporary
/// `vx_<id>_` prefix is expected to already be stripped by the caller.
fn clean_output_name(name: &str) -> String {
    let dot = name.rfind('.');
    let (stem, ext) = match dot {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    if let Some(rest) = stem.strip_suffix(']') {
        if let Some(open) = rest.rfind(" [") {
            let id = &rest[open + 2..];
            if !id.is_empty()
                && id.len() <= 64
                && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return format!("{}{}", rest[..open].trim_end(), ext);
            }
        }
    }
    name.to_string()
}

/// Move `src` into `dir` under `clean`, appending " (n)" when the target exists.
fn finalize_file(src: &std::path::Path, dir: &std::path::Path, clean: &str) -> Option<std::path::PathBuf> {
    let dot = clean.rfind('.');
    let mut target = dir.join(clean);
    let mut n = 1;
    while target.exists() {
        let (stem, ext) = match dot {
            Some(i) if i > 0 => (&clean[..i], &clean[i..]),
            _ => (clean, ""),
        };
        target = dir.join(format!("{} ({}){}", stem, n, ext));
        n += 1;
    }
    if move_file(src, &target) {
        Some(target)
    } else {
        None
    }
}

/// Rename, with a copy+delete fallback for cross-volume moves
/// (TEMP and Downloads may live on different drives).
fn move_file(src: &std::path::Path, dst: &std::path::Path) -> bool {
    if std::fs::rename(src, dst).is_ok() {
        return true;
    }
    if std::fs::copy(src, dst).is_ok() {
        let _ = std::fs::remove_file(src);
        return true;
    }
    false
}

/// True for finished yt-dlp products; false for in-progress/intermediate
/// artifacts (`.part`, `-FragN` pieces, `.ytdl` manifests, `.temp`/`.tmp`).
/// yt-dlp's `-N` fragment parts land next to `-o` even with `--paths temp:`,
/// so completion must tell them apart from the real output.
fn is_final_output(name: &str) -> bool {
    if name.ends_with(".ytdl") || name.ends_with(".temp") || name.ends_with(".tmp") || name.ends_with(".part") {
        return false;
    }
    if name.contains(".part.") || name.contains(".part-") || name.contains("-Frag") {
        return false;
    }
    true
}

fn collect_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Move every finished file under `src_root` to the mirrored path under
/// `dst_root` (creating folders as needed, never overwriting: a " (n)"
/// suffix is added on collision). Intermediates are left behind for the
/// staging-dir cleanup. Returns how many files were moved.
fn move_staged_tree(src_root: &std::path::Path, dst_root: &std::path::Path) -> usize {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_files(src_root, &mut files);
    let mut n = 0;
    for src in files {
        let Ok(rel) = src.strip_prefix(src_root) else { continue };
        let name = src.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if !is_final_output(name) {
            continue;
        }
        let mut dst = dst_root.join(rel);
        if dst.exists() {
            let stem = dst
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = dst
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy()))
                .unwrap_or_default();
            let mut i = 1;
            while dst.exists() {
                dst = dst.with_file_name(format!("{stem} ({i}){ext}"));
                i += 1;
            }
        }
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if move_file(&src, &dst) {
            n += 1;
        }
    }
    n
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
    let eff_proxy = crate::state::proxy_for_url(&settings.per_site_proxies, &settings.proxy, &url);
    let playlist = is_playlist_url(&url);
    let out = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let mut cmd = crate::tools::silent(Command::new(&bin));
        cmd.arg("--newline").arg("--dump-single-json").arg("--no-warnings");
        if playlist {
            // Pull the FIRST entry of the playlist so the video information
            // (esp. the full format ladder) is actually present. The old
            // --no-playlist call returned the playlist wrapper which has no
            // per-video `formats`, so only audio-only entries were offered.
            cmd.args(["--playlist-items", "1"]);
        } else {
            cmd.arg("--no-playlist");
        }
        cmd.args(&cooks);
        if !eff_proxy.trim().is_empty() {
            cmd.arg("--proxy").arg(eff_proxy.trim());
        }
        cmd.arg(&url);
        let o = cmd.output().map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    })
    .await
    .map_err(|e| e.to_string())??;

    let root: Value = serde_json::from_str(&out).map_err(|_| "Could not parse video info".to_string())?;

    // --playlist-items 1 usually unwraps to the entry dict, but for bare
    // playlist URLs yt-dlp may still wrap it in { "entries": [...], "title": ... }.
    let entries = root.get("entries").and_then(|x| x.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
    let v: &Value = if entries { &root["entries"][0] } else { &root };

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
        playlist,
        playlist_count: v
            .get("playlist_count")
            .and_then(|x| x.as_u64())
            .or_else(|| root.get("playlist_count").and_then(|x| x.as_u64())),
        playlist_title: v
            .get("playlist_title")
            .and_then(|x| x.as_str())
            .or_else(|| v.get("playlist").and_then(|x| x.get("title")).and_then(|x| x.as_str()))
            // For wrapped playlist dumps the wrapper's own `title` is the playlist title.
            .or_else(|| if entries { root.get("title").and_then(|x| x.as_str()) } else { None })
            .map(|s| s.to_string()),
    })
}

/// Lightweight info fetch used to resolve dynamic output templates
/// (e.g. `%(playlist_title)s`) right before a download starts.
async fn dynamic_info(app: &AppHandle, bin: &std::path::Path, url: &str) -> YtdlInfo {
    let settings = crate::state::load_settings(app);
    let cooks = cookie_args(settings.use_cookies, &settings.cookies);
    let url = url.to_string();
    let eff_proxy = crate::state::proxy_for_url(&settings.per_site_proxies, &settings.proxy, &url);
    let bin = bin.to_path_buf();
    let out = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let mut cmd = crate::tools::silent(Command::new(&bin));
        cmd.arg("--newline").arg("--dump-single-json").arg("--no-warnings");
        cmd.args(["--playlist-items", "1"]);
        cmd.args(&cooks);
        if !eff_proxy.trim().is_empty() {
            cmd.arg("--proxy").arg(eff_proxy.trim());
        }
        cmd.arg(&url);
        let o = cmd.output().map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or_default();

    let Ok(root) = serde_json::from_str::<Value>(&out) else {
        return YtdlInfo {
            title: "YouTube playlist".into(),
            thumbnail: String::new(),
            duration: 0,
            uploader: String::new(),
            view_count: 0,
            formats: vec![],
            subtitles: vec![],
            playlist: true,
            playlist_count: None,
            playlist_title: None,
        };
    };
    let entries = root.get("entries").and_then(|x| x.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
    let v = if entries { &root["entries"][0] } else { &root };
    YtdlInfo {
        title: v.get("title").and_then(|x| x.as_str()).unwrap_or("YouTube playlist").to_string(),
        thumbnail: v.get("thumbnail").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        duration: v.get("duration").and_then(|x| x.as_u64()).unwrap_or(0),
        uploader: v.get("uploader").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        view_count: v.get("view_count").and_then(|x| x.as_u64()).unwrap_or(0),
        formats: vec![],
        subtitles: vec![],
        playlist: true,
        playlist_count: v
            .get("playlist_count")
            .and_then(|x| x.as_u64())
            .or_else(|| root.get("playlist_count").and_then(|x| x.as_u64())),
        playlist_title: v
            .get("playlist_title")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                if entries {
                    root.get("title").and_then(|x| x.as_str()).map(|s| s.to_string())
                } else {
                    None
                }
            }),
    }
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
    pub embed_thumbnail: bool,
    pub title: Mutex<String>,
    pub filename: Mutex<String>,
    pub category: String,
    pub save_path: Mutex<PathBuf>,
    /// Base output directory (before any sub-folder template like the playlist title).
    pub save_base: Mutex<PathBuf>,
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
    /// Epoch ms when the job reached Completed (None until then).
    pub completed_at: Mutex<Option<u64>>,
    /// True when this job merges/remuxes (needs ffmpeg). Decided here so
    /// `launch()` can fetch it in the background; `start()` itself stays fast
    /// (pure task construction, no network) so WS callers get a real result.
    pub needs_ffmpeg: bool,
    /// Playlist size (videos) for whole-playlist jobs; 0 = unknown/single.
    /// The run loop counts finished items so the row shows "3 / 10 videos".
    pub playlist_total: AtomicU64,
    /// Final on-disk output files (playlist videos, subtitles). Recorded at
    /// completion so remove-with-delete wipes exactly what the job produced.
    pub produced: Mutex<Vec<std::path::PathBuf>>,
    /// Fetch auto-generated captions when no official track matches.
    pub auto_subs: bool,
}

impl YtTask {
    pub fn view(&self) -> download::DlView {
        let status = *self.status.read().unwrap_or_else(|e| e.into_inner());
        let total = self.total.load(Ordering::Relaxed);
        let done = self.done.load(Ordering::Relaxed);
        let speed = self.speed.load(Ordering::Relaxed);
        let progress = if total > 0 { (done as f64 / total as f64) * 100.0 } else { 0.0 };
        let eta = if speed > 0 && total > done { (total - done) / speed } else { 0 };
        let title = self.title.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let filename = self.filename.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let path = self.save_path.lock().unwrap_or_else(|e| e.into_inner());
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
            live: 1,
            status,
            error: self.error.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            thumbnail: self.thumb.clone(),
            source: "youtube".into(),
            save_path: path.display().to_string(),
            created_at: self.created_at,
            completed_at: *self.completed_at.lock().unwrap_or_else(|e| e.into_inner()),
            format_id: Some(self.format_id.clone()),
            produced: self
                .produced
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        }
    }

    fn set_status(&self, s: DlStatus) {
        if s == DlStatus::Completed {
            *self.completed_at.lock().unwrap() = Some(download::now_ms());
        }
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
    embed_thumbnail: bool,
    // Also fetch auto-generated captions (for videos with no official
    // subtitle track). Off by default: auto captions are machine quality.
    auto_subs: bool,
) -> Result<Arc<YtTask>, String> {
    // NOTE: no tool downloads here — `start()` must stay fast and synchronous
    // (extension WS calls time out on slow fetches). Tools are ensured in
    // `launch()`, which runs in the background after the task is registered.
    let is_subs = format_id.starts_with("subs:");
    let audio_fmt = parse_audio_fmt(&format_id);
    let use_merge = !is_subs
        && audio_fmt.is_none()
        && (format_id.contains("+") || format_id.starts_with("bestvideo") || format_id.contains("[height"));
    let is_audio = audio_fmt.is_some() || format_id.starts_with("bestaudio");
    let is_video = !is_subs && !is_audio;
    let wants_embed = is_video && embed_subs && !sub_langs.trim().is_empty();
    // Embedding a thumbnail also remuxes with ffmpeg (mp4/mkv/mp3 cover art).
    let needs_ffmpeg =
        use_merge || audio_fmt.is_some() || wants_embed || (embed_thumbnail && !is_subs);
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
    // Whole-playlist jobs get an IDM/Parabolic-style layout:
    //   <Videos>/Downloads/<Playlist Title>/NN - Title.mp4
    // Hidden (non-playlist) single-video downloads keep the vx_ prefix so the
    // completion step can find and rename the produced files.
    let tmpl = if include_playlist {
        eff_dir.join("%(playlist_title)s").join("%(playlist_index)02d - %(title)s.%(ext)s")
    } else if !playlist_items.trim().is_empty() {
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
        save_base: Mutex::new(eff_dir),
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
        embed_thumbnail,
        app,
        created_at,
        completed_at: Mutex::new(None),
        needs_ffmpeg,
        playlist_total: AtomicU64::new(0),
        produced: Mutex::new(Vec::new()),
        auto_subs,
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
    // ffmpeg is fetched here (background) so a slow/missing binary shows as a
    // row Error instead of failing task creation (and lying "Added" to WS).
    if task.needs_ffmpeg {
        if let Err(e) = tools::ensure_ffmpeg(&task.app).await {
            *task.error.lock().unwrap() = Some(e);
            task.set_status(DlStatus::Error);
            return;
        }
    }
    let _ = ytdlp_run(task.clone(), &bin).await;
}

/// Run yt-dlp and stream progress from its stdout.
pub(crate) async fn ytdlp_run(task: Arc<YtTask>, bin: &std::path::Path) -> Result<(), String> {
    let started = std::time::SystemTime::now();
    // Stage every intermediate artifact (fragments, .part, .ytdl, thumbnails,
    // unmerged streams) in a task-specific dir under the system temp folder so
    // nothing clutters the user's Downloads; it is deleted at the end.
    let tmp_root = std::env::temp_dir().join("vortex").join(&task.id);
    let _ = std::fs::create_dir_all(&tmp_root);
    let watch = tokio::spawn(progress_watch(task.clone()));
    let audio_fmt = parse_audio_fmt(&task.format_id);
    let is_subs = task.format_id.starts_with("subs:");

    let mut args: Vec<String> = vec![
        "--newline".into(),
        "--progress-delta".into(),
        "0.2".into(),
        "--no-warnings".into(),
        // NOTE: do NOT add `--print before_dl:…` here. Verified against
        // yt-dlp 2026.08.19: any `--print` with a `before_dl` stage silences
        // ALL progress output (both `[download]` lines and
        // `--progress-template`), freezing the row at 0% until completion.
        // Title resolves at completion; total arrives with the first progress
        // line instead.
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
        let sub_fmt = match sub_fmt.trim().to_lowercase().as_str() {
            "vtt" => "vtt".to_string(),
            _ => "srt".to_string(),
        };
        args.push("--skip-download".into());
        args.push("--write-subs".into());
        // Auto-generated captions only when the chosen track is an (auto) one.
        if task.auto_subs {
            args.push("--write-auto-subs".into());
        } else {
            args.push("--no-write-auto-subs".into());
        }
        args.push("--sub-langs".into());
        args.push(if langs.is_empty() || langs == "all" { "all".into() } else { langs });
        args.push("--sub-format".into());
        args.push(format!("{sub_fmt}/best"));
        args.push("--convert-subs".into());
        args.push(sub_fmt);
    } else {
        // Stream progress as raw numbers on dedicated lines instead of parsing
        // fragile terminal strings (avoids the "0% for ages then 100%" jump).
        if ytdlp_has_progress_template(&task.app) {
            args.push("--progress-template".into());
            args.push(
                "download:__VX_PROG__:%(progress.downloaded_bytes)s:%(progress.total_bytes,progress.total_bytes_estimate)s:%(progress.speed)s:%(progress.eta)s"
                    .into(),
            );
        }
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
            // Embed chapter markers + full metadata so VLC/players show chapters
            // (described/scoreboard) for tutorials & long videos.
            args.push("--embed-metadata".into());
            args.push("--embed-chapters".into());
            // Auto-embed one official/manual subtitle track so the video is self-contained (IDM-like).
            // Auto-generated captions are always excluded.
            if task.embed_subs && !task.sub_langs.trim().is_empty() {
                args.push("--sub-format".into());
                args.push("srt/vtt/best".into());
                args.push("--embed-subs".into());
                // Auto-generated captions only when explicitly asked (the
                // modal passes auto when the chosen track is an (auto) one).
                if task.auto_subs {
                    args.push("--write-auto-subs".into());
                } else {
                    args.push("--no-write-auto-subs".into());
                }
                args.push("--sub-langs".into());
                args.push(task.sub_langs.trim().to_string());
                args.push("--convert-subs".into());
                args.push("srt".into());
            }
        }
    }
    if task.embed_thumbnail && !is_subs {
        // IDM-style: mux the video thumbnail / album cover art into the file
        // (MP4/MKV cover track, MP3 ID3 cover). Requires ffmpeg (ensured above);
        // yt-dlp converts the webp poster to jpg for maximum compatibility.
        args.push("--embed-thumbnail".into());
        args.push("--convert-thumbnails".into());
        args.push("jpg".into());
    }
    // Per-site proxy override wins over the task's global proxy here too
    // ("DIRECT" drops the flag entirely).
    let eff_proxy = {
        let settings = crate::state::load_settings(&task.app);
        crate::state::proxy_for_url(&settings.per_site_proxies, &task.proxy, &task.url)
    };
    if !eff_proxy.trim().is_empty() {
        args.push("--proxy".into());
        args.push(eff_proxy.trim().to_string());
    }
    let settings = crate::state::load_settings(&task.app);
    args.extend(cookie_args(settings.use_cookies, &settings.cookies));
    // Keep temp/thumbnail artifacts out of the Downloads folder.
    args.push("--paths".into());
    args.push(format!("temp:{}", tmp_root.display()));
    args.push("--paths".into());
    args.push(format!("thumbnail:{}", tmp_root.display()));

    // Whole-playlist jobs use a `%(playlist_title)s` folder which must be
    // resolved to a real, sanitized name before spawning yt-dlp.
    let tmpl = if task.include_playlist {
        let raw = task.save_path.lock().unwrap().display().to_string();
        if raw.contains("%(playlist_title)s") {
            let info = dynamic_info(&task.app, bin, &task.url).await;
            let ptitle = info
                .playlist_title
                .filter(|s| !s.trim().is_empty())
                .map(|s| download::sanitize(&s))
                .unwrap_or_else(|| "Playlist".to_string());
            let base = task.save_base.lock().unwrap().join(&ptitle);
            *task.save_base.lock().unwrap() = base;
            if let Some(n) = info.playlist_count {
                task.playlist_total.store(n, Ordering::Relaxed);
                *task.filename.lock().unwrap() = format!("0 / {} videos", n);
            }
            raw.replace("%(playlist_title)s", &ptitle)
        } else {
            raw
        }
    } else {
        task.save_path.lock().unwrap().display().to_string()
    };
    // Route ALL yt-dlp output through the per-task staging dir: `-N` fragments,
    // `.part` files, subtitles and thumbnails would otherwise land next to the
    // final file in the user's Downloads folder (`--paths temp:` does NOT
    // relocate concurrent `-Frag` parts). Only finished files are moved out
    // at completion; the staging dir is deleted afterwards.
    let real_base = task.save_base.lock().unwrap().clone();
    let staged_tmpl = {
        let p = std::path::Path::new(&tmpl);
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| tmpl.clone());
        tmp_root.join(name).display().to_string()
    };
    let _ = std::fs::create_dir_all(&real_base);
    args.push("-o".into());
    args.push(staged_tmpl);
    args.push(task.url.clone());

    let mut child = crate::tools::silent(std::process::Command::new(bin))
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    // yt-dlp writes `[download]` progress to STDOUT but diagnostics
    // (extract, fragment, merge, ...) to STDERR. Both pipes are piped, so they
    // must be drained CONCURRENTLY: leaving stderr unread fills its pipe buffer
    // (~64KB) and stalls the process — which is what made the row hang at 0%.
    // Two threads forward each line as (is_stderr, line) into one channel.
    let (tx, rx) = std::sync::mpsc::sync_channel::<(bool, String)>(4096);
    let mut readers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    for stderr_stream in [false, true] {
        let stream: Box<dyn std::io::Read + Send> = if stderr_stream {
            match child.stderr.take() {
                Some(s) => Box::new(s),
                None => continue,
            }
        } else {
            match child.stdout.take() {
                Some(s) => Box::new(s),
                None => continue,
            }
        };
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stream);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if tx.send((stderr_stream, line)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);

    // The drain + join + wait below blocks for the whole job (minutes): run
    // it on the blocking pool so Tokio workers stay free for IPC/commands.
    // Everything inside is thread-safe (Arc task, atomics, channel, emits).
    let task_b = task.clone();
    let (stderr_tail, saw_progress, status) = tokio::task::spawn_blocking(move || -> Result<_, String> {
    let task = task_b;
    let mut stderr_tail: Vec<String> = Vec::new();
    let mut saw_progress = false;
    let mut last_prog_emit = Instant::now() - Duration::from_millis(1000);
    let mut last_prog_done = u64::MAX;
    // Cumulative-progress bookkeeping across the multiple files of one job
    // (e.g. separate video + audio streams that get merged).
    let mut prog_base: u64 = 0;
    let mut last_file_dl: u64 = 0;
    let mut last_file_total: u64 = 0;
    // Seen playlist video ids (see playlist_item_id).
    let mut pl_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in rx {
        if task.cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break;
        }
        let (is_err, line) = msg;
        // Rust's lines() strips `\n` and a trailing `\r` already; trim again
        // defensively so a bare `\r` (no `--newline`) never breaks the parse.
        let line = line.trim_end_matches('\r').to_string();

        if is_err {
            if stderr_tail.len() >= 64 {
                stderr_tail.remove(0);
            }
            stderr_tail.push(line.clone());
            eprintln!("[yt-dlp stderr] {line}");
        } else {
            println!("[yt-dlp raw stdout] {line}");
        }

        // ffmpeg merge / extract stage: no byte progress flows here, so flip the
        // chip to "merging" instead of leaving the bar frozen at its last value.
        if line.contains("[Merger]") || line.contains("[ExtractAudio]") {
            if *task.status.read().unwrap() == DlStatus::Downloading {
                task.set_status(DlStatus::Merging);
            }
        }

        // Playlist item counter: each new "[info] <id>: Downloading N
        // format(s)" line means yt-dlp moved to the next video — show
        // "k / N videos" so a 50-video job isn't a mystery bar.
        if task.include_playlist {
            // Authoritative "video k of n" line: also teaches us N when the
            // upfront probe never ran (no playlist_title template).
            if let Some((k, n)) = playlist_progress(&line) {
                task.playlist_total.store(n, Ordering::Relaxed);
                *task.filename.lock().unwrap() = format!("{k} / {n} videos");
                let _ = task.app.emit("downloads-changed", ());
            }
            if let Some(id) = playlist_item_id(&line) {
                let total = task.playlist_total.load(Ordering::Relaxed);
                if total > 0 && pl_seen.insert(id.to_string()) {
                    let k = (pl_seen.len() as u64).min(total);
                    *task.filename.lock().unwrap() = format!("{} / {} videos", k, total);
                    let _ = task.app.emit("downloads-changed", ());
                }
            }
        }

        if let Some(title) = line.strip_prefix("__VX_TITLE__:") {
            let t = title.trim().to_string();
            if !t.is_empty() {
                {
                    let mut cur = task.title.lock().unwrap();
                    if *cur != t {
                        *cur = t.clone();
                    }
                }
                *task.filename.lock().unwrap() = download::sanitize(&t);
                let _ = task.app.emit("downloads-changed", ());
            }
        } else if let Some(sz) = line.strip_prefix("__VX_SIZE__:") {
            if let Some(total) = parse_size_or_bytes(sz) {
                task.total.store(total, Ordering::Relaxed);
                let _ = task.app.emit("downloads-changed", ());
            }
        } else if let Some((dl, total, spd, eta)) = parse_progress_vx(&line) {
            if dl > 0 || total > 0 {
                saw_progress = true;
            }
            // Merged downloads (video+audio) report progress PER FILE with
            // `downloaded_bytes` resetting to ~0 when the next file starts.
            // Accumulate with a base offset so the bar climbs 0→100 across all
            // files instead of jumping to 100% on the first file and clamping
            // there (done > total) while the second file downloads.
            if dl + 1024 < last_file_dl && last_file_total > 0 {
                prog_base = prog_base.saturating_add(last_file_total);
            }
            last_file_dl = dl;
            if total > 0 {
                last_file_total = total;
                task.total.store(prog_base.saturating_add(total), Ordering::Relaxed);
            }
            task.done.store(
                prog_base.saturating_add(dl).max(task.done.load(Ordering::Relaxed)),
                Ordering::Relaxed,
            );
            if spd > 0 {
                task.speed.store(spd, Ordering::Relaxed);
            }
            // Emit live progress (max 4/s, only on movement + 1 s heartbeat)
            // so IPC never starves network/disk I/O on fast links.
            let d = task.done.load(Ordering::Relaxed);
            if d != last_prog_done
                || Instant::now().duration_since(last_prog_emit) >= Duration::from_secs(1)
            {
                if Instant::now().duration_since(last_prog_emit) >= Duration::from_millis(250) {
                    last_prog_emit = Instant::now();
                    last_prog_done = d;
                    let t = task.total.load(Ordering::Relaxed);
                    let _ = task.app.emit(
                        "download-progress",
                        serde_json::json!({
                            "id": task.id,
                            "downloaded": d,
                            "total_size": t,
                            "speed": task.speed.load(Ordering::Relaxed),
                            "progress": if t > 0 { (d as f64 / t as f64 * 100.0).min(100.0) } else { 0.0 },
                            "eta": eta,
                        }),
                    );
                }
            }
        } else if let Some((pct, total, speed)) = parse_progress(&line) {
            saw_progress = true;
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
    for h in readers {
        let _ = h.join();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    Ok((stderr_tail, saw_progress, status))
    })
    .await
    .map_err(|e| format!("yt-dlp monitor task failed: {e}"))??;
    watch.abort();

    if !saw_progress {
        println!("[yt-dlp] WARNING: no [download] progress lines were streamed on stdout/stderr");
    }

    if task.cancel.load(Ordering::Relaxed) {
        task.set_status(DlStatus::Cancelled);
        let _ = std::fs::remove_dir_all(&tmp_root);
        return Ok(());
    }

    if !status.success() {
        // Surface the real yt-dlp error instead of leaving the row in "Downloading".
        let first_err = stderr_tail.iter().find(|l| l.starts_with("ERROR:")).cloned().unwrap_or_default();
        let detail = if !first_err.is_empty() {
            first_err
        } else if let Some(last) = stderr_tail.last() {
            last.clone()
        } else {
            String::new()
        };
        *task.error.lock().unwrap() = Some(if detail.is_empty() {
            "Download failed".to_string()
        } else {
            format!("Download failed — {detail}")
        });
        task.set_status(DlStatus::Error);
        let _ = std::fs::remove_dir_all(&tmp_root);
        return Ok(());
    }

    if status.success() {
        if task.include_playlist {
            // Whole-playlist job: yt-dlp staged into tmp_root — move finished
            // videos into the real playlist folder first, then summarize.
            let base = task.save_base.lock().unwrap().clone();
            move_staged_tree(&tmp_root, &base);
            // Whole-playlist job: locate the videos inside a "NN - Title.ext"
            // folder (or any sub-folder) and summarize them on the row.
            let outputs = find_playlist_outputs(&base, started);
            if !outputs.is_empty() {
                // Remember every produced video for remove-with-delete.
                *task.produced.lock().unwrap() = outputs.clone();
                let folder = outputs[0].parent().map(|p| p.to_path_buf()).unwrap_or(base);
                let count = outputs.len();
                let sum: u64 = outputs.iter().filter_map(|p| p.metadata().ok()).map(|m| m.len()).sum();
                let folder_name = folder.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                *task.filename.lock().unwrap() = format!("{count} / {count} videos");
                *task.title.lock().unwrap() = folder_name;
                *task.save_path.lock().unwrap() = folder.clone();
                if sum > 0 {
                    task.total.store(sum, Ordering::Relaxed);
                    task.done.store(sum, Ordering::Relaxed);
                } else {
                    task.done.store(task.total.load(Ordering::Relaxed), Ordering::Relaxed);
                }
                let _ = task.app.emit("downloads-changed", ());
                // Auto-sub embed drops .srt sidecars next to the videos:
                // the subs live inside the files now, so sweep the strays.
                // (Subs-only jobs keep their product — never sweep those.)
                if task.auto_subs && !task.format_id.starts_with("subs:") {
                    cleanup_stray_subs(&folder, started);
                }
            } else {
                *task.error.lock().unwrap() = Some("Output file not found".into());
            }
        } else {
            // yt-dlp staged into tmp_root: pick finished files via the unique
            // prefix (skipping `.part`/`-Frag`/`.ytdl` intermediates) and move
            // them into the real destination folder with clean names.
            let dir = task.save_base.lock().unwrap().clone();
            let prefix = format!("vx_{}_", task.id);
            let outputs = find_outputs(&tmp_root, &prefix)
                .into_iter()
                .filter(|p| {
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .map(is_final_output)
                        .unwrap_or(false)
                })
                .collect::<Vec<_>>();
            if !outputs.is_empty() {
                // Strip the temporary prefix from final filename(s).
                let mut real_names: Vec<String> = Vec::new();
                let mut produced: Vec<std::path::PathBuf> = Vec::new();
                for real in &outputs {
                    let raw = real.file_name().map(|s| s.to_string_lossy()[prefix.len()..].to_string()).unwrap_or_default();
                    if !raw.is_empty() {
                        if let Some(fp) = finalize_file(real, &dir, &clean_output_name(&raw)) {
                            produced.push(fp.clone());
                            real_names.push(fp.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
                        }
                    }
                }
                *task.produced.lock().unwrap() = produced;
                let first = dir.join(real_names.first().cloned().unwrap_or_default());                if real_names.len() == 1 {
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
                if task.auto_subs && !task.format_id.starts_with("subs:") {
                    cleanup_stray_subs(&dir, started);
                }
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
        }
        task.set_status(DlStatus::Completed);
    } else {
        *task.error.lock().unwrap() = Some("Download failed".into());
        task.set_status(DlStatus::Error);
    }
    // Remove the per-task staging dir (fragments, .part, thumbnails, …).
    let _ = std::fs::remove_dir_all(&tmp_root);
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

/// Recursively find playlist outputs (files named "NN - <Title>.<ext>") under
/// `dir` that were created after `since`.
fn find_playlist_outputs(dir: &std::path::Path, since: std::time::SystemTime) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(find_playlist_outputs(&p, since));
            } else if is_playlist_output_name(&p) {
                let fresh = p
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| t >= since)
                    .unwrap_or(true);
                if fresh {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Remove stray subtitle sidecars (`*.srt|*.vtt|…`) that `--write-auto-subs`
/// drops next to an *embed* job's outputs. Only files created after `since`
/// qualify (mtime-scoped, so user files are never touched); unknown mtime
/// means keep. Subs-only jobs keep their product — callers gate on that.
fn cleanup_stray_subs(dir: &std::path::Path, since: std::time::SystemTime) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            continue;
        }
        let is_sub = p
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| matches!(x.to_ascii_lowercase().as_str(), "srt" | "vtt" | "ass" | "ssa" | "ttml" | "srv3" | "json3"))
            .unwrap_or(false);
        if !is_sub {
            continue;
        }
        let fresh = p
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| t >= since)
            .unwrap_or(false);
        if fresh {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// File name matches the "NN - Title.ext" playlist convention.
fn is_playlist_output_name(p: &std::path::Path) -> bool {
    p.file_name()
        .and_then(|s| s.to_str())
        .map(|s| {
            let b = s.as_bytes();
            b.len() > 4
                && b[0].is_ascii_digit()
                && b[1].is_ascii_digit()
                && b[2] == b' '
                && b[3] == b'-'
                && b[4] == b' '
        })
        .unwrap_or(false)
}

pub async fn progress_watch(task: Arc<YtTask>) {
    let mut last = Instant::now();
    let mut last_done = task.done.load(Ordering::Relaxed);
    // Same IPC throttle as the template path: max 4/s, movement + heartbeat.
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut sent_done = u64::MAX;
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let now = task.done.load(Ordering::Relaxed);
        let speed = ((now.saturating_sub(last_done)) as f64 / last.elapsed().as_secs_f64().max(0.05)) as u64;
        // Never overwrite a live template-reported speed with 0: when yt-dlp is
        // in a merge/extract stage `done` doesn't move, but zeroing the speed
        // is what froze the row at "0 B/s" between progress bursts.
        if speed > 0 {
            task.speed.store(speed, Ordering::Relaxed);
        }
        last = Instant::now();
        last_done = now;
        if now != sent_done || last_emit.elapsed() >= Duration::from_secs(1) {
            last_emit = Instant::now();
            sent_done = now;
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
        }
        let st = *task.status.read().unwrap();
        if st == DlStatus::Completed || st == DlStatus::Cancelled || st == DlStatus::Error {
            break;
        }
    }
}

#[cfg(test)]
mod vx_tests {
    use super::*;

    #[test]
    fn playlist_item_id_detects_markers() {
        assert_eq!(
            playlist_item_id("[info] dQw4w9WgXcQ: Downloading 1 format(s)"),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            playlist_item_id("[info] abc123XYZ_-: Downloading 2 format(s): 248+251"),
            Some("abc123XYZ_-")
        );
        // not markers
        assert_eq!(playlist_item_id("[download] 12.3% of 5MiB at 1MiB/s"), None);
        assert_eq!(playlist_item_id("[info] Downloading video 3 of 10"), None);
        assert_eq!(playlist_item_id("__VX_PROG__:1:2:3:4"), None);
        assert_eq!(playlist_item_id(""), None);
    }

    #[test]
    fn playlist_progress_parses_counter() {
        assert_eq!(
            playlist_progress("[download] Downloading video 3 of 12"),
            Some((3, 12))
        );
        assert_eq!(
            playlist_progress("[download] Downloading video 1 of 1"),
            Some((1, 1))
        );
        // junk / edge cases never yield numbers
        assert_eq!(playlist_progress("[download] 12.3% of 5MiB at 1MiB/s"), None);
        assert_eq!(playlist_progress("[download] Downloading video 0 of 12"), None);
        assert_eq!(playlist_progress("[download] Downloading video 13 of 12"), None);
        assert_eq!(playlist_progress("[info] abc: Downloading 1 format(s)"), None);
        assert_eq!(playlist_progress(""), None);
    }
}