use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use futures_util::StreamExt;
use reqwest::Client;
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
pub const FFMPEG_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";

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
    let out = run_out(&bin, &["-version"])?;
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
        return Ok(p);
    }
    let dir = tools_dir(app).map_err(|e| e.to_string())?;
    let zip_path = dir.join("ffmpeg.zip");
    download_to(&FFMPEG_URL, &zip_path)
        .await
        .map_err(|e| format!("Failed to download ffmpeg: {e}"))?;

    let file = File::open(&zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut found = false;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        if name.ends_with("/ffmpeg.exe") {
            let mut out = File::create(dir.join("ffmpeg.exe")).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
            found = true;
            break;
        }
    }
    let _ = fs::remove_file(&zip_path);
    if !found {
        return Err("ffmpeg.exe not found inside archive".into());
    }
    Ok(dir.join("ffmpeg.exe"))
}

async fn download_to(url: &str, target: &std::path::Path) -> Result<(), String> {
    let client = Client::builder()
        .user_agent("Vortex/0.1 (download manager)")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    let total = resp.content_length().unwrap_or(0);
    let mut out = File::create(target).map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        out.write_all(&chunk).map_err(|e| e.to_string())?;
        written += chunk.len() as u64;
        if written % (1024 * 512) < chunk.len() as u64 {
            std::io::Write::flush(&mut out).ok();
        }
    }
    std::io::Write::flush(&mut out).ok();
    let _ = total;
    Ok(())
}