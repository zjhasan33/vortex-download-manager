use std::fs::{self, File};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use futures_util::StreamExt;
use tauri::{AppHandle, Manager};

/// Prevent console windows from flashing for spawned console apps (yt-dlp, ffmpeg, reg, ...).
pub(crate) fn silent(mut cmd: Command) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = &cmd;
    cmd
}

pub const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
/// BtbN full static GPL build (single ffmpeg.exe under bin/, ~180 MB).
/// Previously Gyan essentials, but gyan.dev throttles to ~80 KB/s from some
/// regions (23+ min per fetch) while GitHub CDN saturates the line — and the
/// extractor only needs *some* ffmpeg.exe inside the archive either way.
pub const FFMPEG_URL: &str =
    "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/ffmpeg-n9.0-latest-win64-gpl-9.0.zip";
/// Some CDNs reject requests with a missing/empty User-Agent.
pub const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0 Safari/537.36";

/// Neutral tool UA used when a server answers a browser-like UA with an endless
/// redirect loop (mirror anti-hotlinking, e.g. mirrors.nju.edu.cn 302->itself).
pub const TOOL_UA: &str = "Wget/1.21.4";

pub fn tools_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("tools");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

pub fn ytdlp_path(app: &AppHandle) -> Option<PathBuf> {
    let p = tools_dir(app).ok()?.join("yt-dlp.exe");
    if p.exists() { Some(p) } else { None }
}

pub fn ffmpeg_path(app: &AppHandle) -> Option<PathBuf> {
    let p = tools_dir(app).ok()?.join("ffmpeg.exe");
    if p.exists() { Some(p) } else { None }
}

fn run_out(bin: &std::path::Path, args: &[&str]) -> Option<String> {
    silent(Command::new(bin))
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

pub fn ytdlp_version(app: &AppHandle) -> Option<String> {
    let bin = ytdlp_path(app)?;
    let out = run_out(&bin, &["--version"])?;
    out.trim().split_whitespace().next().map(|s| s.to_string())
}

pub fn ffmpeg_version(app: &AppHandle) -> Option<String> {
    let bin = ffmpeg_path(app)?;
    ffmpeg_version_of(&bin)
}

fn ffmpeg_version_of(bin: &std::path::Path) -> Option<String> {
    let out = run_out(bin, &["-version"])?;
    out.lines().next().map(|l| {
        l.trim()
            .split_whitespace()
            .nth(2)
            .unwrap_or("")
            .to_string()
    })
}

pub fn status(app: &AppHandle) -> (bool, bool, Option<String>, Option<String>) {
    let y = ytdlp_version(app);
    let f = ffmpeg_version(app);
    (y.is_some(), f.is_some(), y, f)
}

/// Ensure yt-dlp is present; download the portable exe if needed.
pub async fn ensure_ytdlp(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(p) = ytdlp_path(app) {
        return Ok(p);
    }
    let target = tools_dir(app).map_err(|e| e.to_string())?.join("yt-dlp.exe");
    download_to(&YTDLP_URL, &target).await.map_err(|e| format!("Failed to download yt-dlp: {e}"))?;
    Ok(target)
}

/// Ensure ffmpeg is present; download + extract the portable exe if needed.
pub async fn ensure_ffmpeg(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(p) = ffmpeg_path(app) {
        // Trust it only if it actually runs; a truncated/partial file must be replaced.
        if ffmpeg_version(app).is_some() {
            return Ok(p);
        }
        eprintln!("[vortex-tools] existing ffmpeg.exe is invalid — re-downloading");
        let _ = fs::remove_file(&p);
    }
    let dir = tools_dir(app).map_err(|e| e.to_string())?;
    let zip_path = dir.join("ffmpeg.zip");
    let _ = fs::remove_file(&zip_path); // drop any stale partial archive

    eprintln!("[vortex-tools] downloading ffmpeg from {FFMPEG_URL} -> {zip_path:?}");
    download_to(&FFMPEG_URL, &zip_path)
        .await
        .map_err(|e| {
            eprintln!("[vortex-tools] ffmpeg download FAILED: {e}");
            let _ = fs::remove_file(&zip_path);
            format!("Failed to download ffmpeg: {e}")
        })?;

    let zip_len = fs::metadata(&zip_path).map(|m| m.len()).unwrap_or(0);
    eprintln!("[vortex-tools] ffmpeg zip downloaded ({zip_len} bytes), extracting…");

    let extract = extract_ffmpeg(&zip_path, &dir);
    // Always remove the large temp archive, even if extraction failed.
    let _ = fs::remove_file(&zip_path);

    match extract {
        Ok(()) => {
            let exe = dir.join("ffmpeg.exe");
            eprintln!("[vortex-tools] ffmpeg ready at {exe:?}");
            Ok(exe)
        }
        Err(e) => {
            eprintln!("[vortex-tools] ffmpeg extraction FAILED: {e}");
            Err(e)
        }
    }
}

/// Find `ffmpeg.exe` anywhere inside the archive (e.g. `.../bin/ffmpeg.exe`)
/// and copy it to `dir/filename`.
fn extract_ffmpeg_as(
    zip_path: &std::path::Path,
    dir: &std::path::Path,
    filename: &str,
) -> Result<(), String> {
    let file = File::open(zip_path).map_err(|e| format!("Cannot open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Bad zip archive: {e}"))?;

    let target = dir.join(filename);
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Zip entry {i} error: {e}"))?;
        let name = entry.name().replace('\\', "/");
        let is_exe = name
            .rsplit('/')
            .next()
            .map(|f| f.eq_ignore_ascii_case("ffmpeg.exe"))
            .unwrap_or(false);
        if !is_exe {
            continue;
        }
        eprintln!("[vortex-tools] found '{name}' in archive, extracting");
        let mut out = File::create(&target).map_err(|e| format!("Cannot create {filename}: {e}"))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("Copy failed: {e}"))?;
        std::io::Write::flush(&mut out).ok();
        return Ok(());
    }
    Err("ffmpeg.exe not found inside archive".into())
}

/// Legacy entry point used by `ensure_ffmpeg`; extracts straight to `ffmpeg.exe`.
fn extract_ffmpeg(zip_path: &std::path::Path, dir: &std::path::Path) -> Result<(), String> {
    extract_ffmpeg_as(zip_path, dir, "ffmpeg.exe")
}

/// Download with resume + retries: a truncated tool fetch (killed app, cut
/// connection — the classic 7 MB ffmpeg.zip) continues from its partial bytes
/// instead of restarting from zero and failing the same way again.
async fn download_to(url: &str, target: &std::path::Path) -> Result<(), String> {
    const MAX_ATTEMPTS: u64 = 5;
    let mut attempt = 0u64;
    loop {
        attempt += 1;
        match download_once(url, target).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("[vortex-tools] attempt {attempt} failed: {e}");
                if attempt >= MAX_ATTEMPTS {
                    return Err(format!("Gave up after {attempt} tries: {e}"));
                }
                let wait = 2u64.saturating_pow(attempt.min(5) as u32);
                eprintln!("[vortex-tools] retry {attempt}/{MAX_ATTEMPTS} in {wait}s: {e}");
                tokio::time::sleep(Duration::from_secs(wait)).await;
            }
        }
    }
}

async fn download_once(url: &str, target: &std::path::Path) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .user_agent(BROWSER_UA)
        // GitHub / CDN release URLs redirect several times to the asset.
        .redirect(reqwest::redirect::Policy::limited(10))
        // ffmpeg zip is ~80–120 MB; allow up to 10 minutes for the stream.
        .timeout(Duration::from_secs(600))
        .connect_timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    // Resume a partial file left by a killed/interrupted attempt.
    let have = std::fs::metadata(target).map(|m| m.len()).unwrap_or(0);
    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req.send().await.map_err(|e| {
        crate::download::log_net_err(&e, "tools download");
        format!("Network error: {e}")
    })?;
    let status = resp.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // Already fully fetched (or server disagrees) — accept if non-empty.
        if have > 0 {
            eprintln!("[vortex-tools] already complete: {have} bytes");
            return Ok(());
        }
        return Err("HTTP 416 for {url}".to_string());
    }
    if !status.is_success() {
        return Err(format!("HTTP {} for {url}", status.as_u16()));
    }
    // 206 = resume accepted (append); anything else = fresh body (truncate).
    // A server that drops Range on redirect answers 200 — restarting is the
    // only correct fallback then.
    let resumed = have > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
    if !resumed && have > 0 {
        eprintln!("[vortex-tools] server ignored Range — restarting from zero");
    }
    let remaining = resp.content_length().unwrap_or(0);
    let total = if resumed { have + remaining } else { remaining };
    eprintln!(
        "[vortex-tools] download started: {url} ({} bytes{})",
        if total > 0 { total.to_string() } else { "unknown".into() },
        if resumed { format!(", resuming at {have}") } else { String::new() },
    );

    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(target)
        .await
        .map_err(|e| format!("Cannot create file: {e}"))?;
    use tokio::io::AsyncWriteExt;
    let mut stream = resp.bytes_stream();
    let mut written = if resumed { have } else { 0 };
    let mut next_log = written;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            crate::download::log_net_err(&e, "tools stream");
            format!("Stream error after {written} bytes: {e}")
        })?;
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        written += chunk.len() as u64;
        if written >= next_log {
            let pct = if total > 0 { (written * 100 / total) as u32 } else { 0 };
            eprintln!("[vortex-tools]   {written} / {total} ({pct}%)");
            next_log = written + 2 * 1024 * 1024;
            out.flush().await.ok();
        }
    }
    out.flush().await.ok();
    drop(out);
    // A short stream with no error is still a failure (truncated zip).
    if total > 0 {
        let final_len = tokio::fs::metadata(target).await.map(|m| m.len()).unwrap_or(0);
        if final_len != total {
            return Err(format!("Incomplete download ({final_len}/{total} bytes)"));
        }
    }
    eprintln!("[vortex-tools] download finished: {written} bytes");
    Ok(())
}

/// Read a child process pipe fully into a string. Must run concurrently with
/// `Child::wait()` so the process can never deadlock on full pipe buffers.
async fn drain_pipe(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> String {
    let mut buf = String::new();
    if let Some(mut io) = pipe {
        use tokio::io::AsyncReadExt;
        let _ = io.read_to_string(&mut buf).await;
    }
    buf
}

/// Bring yt-dlp up to date via its built-in self updater. Runs with a hard
/// 15s timeout and drains stdout+stderr concurrently, so the subprocess can
/// never block on a full pipe buffer. If the updater fails or stays silent,
/// falls back to re-downloading the portable exe from GitHub.
pub async fn update_ytdlp(app: &AppHandle) -> Result<(), String> {
    use std::process::Stdio;

    let dir = tools_dir(app).map_err(|e| e.to_string())?;
    let target = dir.join("yt-dlp.exe");
    if !target.exists() {
        return ensure_ytdlp(app).await.map(|_| ());
    }

    let mut child = tokio::process::Command::new(&target)
        .arg("--update")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to start yt-dlp updater: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let wait = child.wait();

    let result = tokio::time::timeout(
        Duration::from_secs(15),
        async {
            let so = drain_pipe(stdout);
            let se = drain_pipe(stderr);
            let (status, out, err) = tokio::join!(wait, so, se);
            (status, out, err)
        },
    )
    .await;

    match result {
        // Dropping `child` here kills + reaps the process via kill_on_drop.
        Err(_) => Err("Update check timed out".into()),
        Ok((status, out, err)) => {
            let combined = format!("{out}{err}");
            eprintln!("[vortex-tools] yt-dlp --update => {combined}");
            if !status.map(|s| s.success()).unwrap_or(false) {
                eprintln!("[vortex-tools] yt-dlp updater failed — re-downloading");
                return download_to(&YTDLP_URL, &target)
                    .await
                    .map_err(|e| format!("yt-dlp update failed: {e}"));
            }
            Ok(())
        }
    }
}

/// Force-refresh ffmpeg from the latest Gyan essentials build. The existing copy
/// is only replaced after the new binary has been verified to run.
pub async fn update_ffmpeg(app: &AppHandle) -> Result<(), String> {
    let dir = tools_dir(app).map_err(|e| e.to_string())?;
    let zip_path = dir.join("ffmpeg.zip");
    let _ = fs::remove_file(&zip_path); // drop any stale partial archive

    eprintln!("[vortex-tools] force-refreshing ffmpeg from {FFMPEG_URL}");
    download_to(&FFMPEG_URL, &zip_path)
        .await
        .map_err(|e| {
            let _ = fs::remove_file(&zip_path);
            format!("ffmpeg update download failed: {e}")
        })?;

    let extract = extract_ffmpeg_as(&zip_path, &dir, "ffmpeg.exe.new");
    let _ = fs::remove_file(&zip_path); // always drop the large temp archive
    extract.map_err(|e| format!("ffmpeg update failed: {e}"))?;

    let new = dir.join("ffmpeg.exe.new");
    // Only swap if the freshly downloaded exe actually runs (truncated/bad file guard).
    if ffmpeg_version_of(&new).is_none() {
        let _ = fs::remove_file(&new);
        return Err("Downloaded ffmpeg failed to run — kept the existing copy".into());
    }
    let target = dir.join("ffmpeg.exe");
    if target.exists() {
        fs::remove_file(&target).map_err(|e| format!("Cannot replace ffmpeg.exe: {e}"))?;
    }
    fs::rename(&new, &target).map_err(|e| format!("Cannot finalize ffmpeg.exe: {e}"))?;
    eprintln!("[vortex-tools] ffmpeg force-refresh complete");
    Ok(())
}