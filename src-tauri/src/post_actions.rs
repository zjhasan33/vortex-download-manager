use std::path::PathBuf;
use std::process::Command;

fn regex_lite(hay: &str, pat: &str) -> bool {
    // Tiny regex for s\d+e\d+ and ep\d+ — no external crate, pure chars.
    if pat == r"s\d+e\d+" {
        let b = hay.as_bytes();
        for i in 0..b.len() {
            if b[i] == b's' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_digit() { j += 1; }
                if j < b.len() && b[j] == b'e' && j + 1 < b.len() && b[j + 1].is_ascii_digit() {
                    return true;
                }
            }
        }
        return false;
    }
    if pat == r"ep\d+" {
        let b = hay.as_bytes();
        for i in 0..b.len().saturating_sub(1) {
            if b[i] == b'e' && b[i + 1] == b'p' && i + 2 < b.len() && b[i + 2].is_ascii_digit() {
                return true;
            }
        }
        return false;
    }
    false
}

fn parse_scene_release(name: &str) -> Option<(String, i32, String)> {
    // Scene: Movie.Name.2024.1080p.BluRay.x264-YIFY or Movie Name 2024 1080p
    // Extract year (1900-2035) and resolution, clean title is before year.
    let lower = name.to_ascii_lowercase();
    let tokens: Vec<&str> = lower.split(|c: char| c == '.' || c == '_' || c == '-' || c == ' ').filter(|s| !s.is_empty()).collect();
    let mut year: Option<i32> = None;
    let mut year_idx: Option<usize> = None;
    let mut res = String::new();
    for (i, tok) in tokens.iter().enumerate() {
        if tok.len() == 4 {
            if let Ok(y) = tok.parse::<i32>() {
                if (1900..=2035).contains(&y) {
                    year = Some(y);
                    year_idx = Some(i);
                    break;
                }
            }
        }
        if ["2160p", "1080p", "720p", "480p", "4k"].contains(tok) && res.is_empty() {
            res = tok.to_string();
        }
    }
    // Also scan for resolution separately if not found before year.
    if res.is_empty() {
        for tok in &tokens {
            if ["2160p", "1080p", "720p", "480p", "4k"].contains(tok) {
                res = tok.to_string();
                break;
            }
        }
    }
    let y = year?;
    let idx = year_idx?;
    // Clean title is tokens before year, join with space, Title Case.
    let title_tokens = &tokens[..idx];
    if title_tokens.is_empty() {
        return None;
    }
    let clean = title_tokens
        .join(" ")
        .split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some((clean, y, res))
}

fn domain_folder(url: &str) -> Option<String> {
    let low = url.to_ascii_lowercase();
    let host = if let Some(p) = low.find("://") {
        let rest = &low[p + 3..];
        rest.split(['/', ':', '?', '#']).next().unwrap_or("")
    } else {
        low.split(['/', ':', '?', '#']).next().unwrap_or("")
    };
    if host.is_empty() {
        return None;
    }
    // Map known hosts to nice folder names.
    let mapped = if host.contains("youtube.com") || host.contains("youtu.be") || host.contains("youtube-nocookie") {
        "YouTube"
    } else if host.contains("tiktok.com") {
        "TikTok"
    } else if host.contains("instagram.com") {
        "Instagram"
    } else if host.contains("facebook.com") || host.contains("fb.watch") {
        "Facebook"
    } else if host.contains("twitter.com") || host.contains("x.com") {
        "X"
    } else if host.contains("vimeo.com") {
        "Vimeo"
    } else if host.contains("twitch.tv") {
        "Twitch"
    } else if host.contains("bilibili.com") {
        "Bilibili"
    } else if host.contains("dailymotion.com") {
        "Dailymotion"
    } else if host.contains("soundcloud.com") {
        "SoundCloud"
    } else {
        // Generic: take base domain, Title Case, e.g. "hianime.to" -> "Hianime"
        let base = host.split('.').rev().nth(1).unwrap_or(host);
        if base.len() < 2 {
            return None;
        }
        // Return owned string for generic case, but we need static for match above.
        // For generic, just return capitalized base.
        return Some({
            let mut c = base.chars();
            match c.next() {
                None => return None,
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
            }
        });
    };
    Some(mapped.to_string())
}

/// Isolated post-download actions — zero touch on download.rs core.
/// Each runs on a blocking thread so the UI/runtime never stalls.

/// Convert a completed video file to MP3 alongside the original (via bundled ffmpeg).
/// Returns the new .mp3 path on success.
pub async fn convert_to_mp3(app: &tauri::AppHandle, src: PathBuf) -> Result<PathBuf, String> {
    convert_with(app, src, "mp3", &["-vn", "-c:a", "libmp3lame", "-q:a", "2"]).await
}

/// Generic audio convert — isolated, spawn_blocking, no core touch.
/// `format` is the target extension (mp3/m4a/flac/wav/opus), `args` are ffmpeg audio args.
pub async fn convert_with(
    app: &tauri::AppHandle,
    src: PathBuf,
    format: &str,
    args: &[&str],
) -> Result<PathBuf, String> {
    if !src.is_file() {
        return Err("Source file not found".into());
    }
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == format.to_ascii_lowercase() {
        return Err(format!("File is already {format}"));
    }
    let ffmpeg = crate::tools::ffmpeg_path(app).ok_or("ffmpeg not found — run Update Tools")?;
    let dst = src.with_extension(format);
    if dst.exists() {
        return Err(format!("Already exists: {}", dst.display()));
    }
    let src_c = src.clone();
    let dst_c = dst.clone();
    let ffmpeg_c = ffmpeg.clone();
    let fargs: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let mut cmd = crate::tools::silent(Command::new(ffmpeg_c));
        cmd.args(["-y", "-i"]).arg(&src_c).args(&fargs).arg(&dst_c);
        let out = cmd.output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if msg.is_empty() { "ffmpeg failed".into() } else { msg });
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(dst)
}

/// Local keyword routing — pure regex/rules, no network/AI.
/// Weighted tokenized scoring + source URL hinting, 100% offline, spawn_blocking safe.
#[allow(dead_code)]
pub fn keyword_route(path: &PathBuf) -> Option<PathBuf> {
    keyword_route_with_url(path, None)
}

pub fn keyword_route_with_url(path: &PathBuf, url: Option<&str>) -> Option<PathBuf> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    let dir = path.parent()?;
    let url_low = url.unwrap_or("").to_ascii_lowercase();

    // Tokenize name for precise word boundaries (avoids "app" matching "happy").
    let tokens: Vec<&str> = name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|s| !s.is_empty()).collect();
    let has_token = |kw: &str| tokens.iter().any(|t| *t == kw) || name.contains(kw);

    // Weighted scores per category.
    let mut scores: std::collections::HashMap<String, i32> = std::collections::HashMap::new();
    // Helper to add score if token matches.
    let add_score = |cat: &str, kw: &str, w: i32, scores: &mut std::collections::HashMap<String, i32>| {
        if has_token(kw) {
            *scores.entry(cat.to_string()).or_insert(0) += w;
        }
    };
    // Movies — release tags weighted higher (2) to avoid "movie app" false positive.
    for k in ["bluray", "web-dl", "webdl", "x264", "x265", "hevc", "yify", "repack"] {
        add_score("Movies", k, 2, &mut scores);
    }
    for k in ["movie", "film", "cinema", "1080p", "720p", "2160p", "hdr"] {
        add_score("Movies", k, 1, &mut scores);
    }
    // TV / Anime — season/episode patterns.
    let re_se = regex_lite(&name, r"s\d+e\d+");
    let re_ep = regex_lite(&name, r"ep\d+");
    if re_se { *scores.entry("TV".to_string()).or_insert(0) += 3; }
    if re_ep { *scores.entry("TV".to_string()).or_insert(0) += 2; }
    for k in ["season", "episode", "anime"] {
        add_score("TV", k, 2, &mut scores);
    }
    // Courses — educational markers weighted higher.
    for k in ["course", "tutorial", "lecture", "guide", "class", "learn", "udemy", "coursera", "skillshare", "pluralsight"] {
        add_score("Courses", k, 2, &mut scores);
    }
    // Software — installer keywords.
    for k in ["setup", "installer", "portable", "patch", "update"] {
        add_score("Software", k, 2, &mut scores);
    }
    for k in ["software", "app", "exe", "msi", "dmg"] {
        add_score("Software", k, 1, &mut scores);
    }
    // Music
    for k in ["music", "song", "album"] {
        add_score("Music", k, 2, &mut scores);
    }
    for k in ["mp3", "flac", "wav", "opus", "m4a"] {
        add_score("Music", k, 1, &mut scores);
    }

    // Source URL hinting — strong signal (+5) from domain.
    if url_low.contains("udemy.com") || url_low.contains("coursera.org") || url_low.contains("skillshare.com") || url_low.contains("pluralsight.com") {
        *scores.entry("Courses".to_string()).or_insert(0) += 5;
    }
    if url_low.contains("github.com") || url_low.contains("gitlab.com") || url_low.contains("sourceforge.net") {
        *scores.entry("Software".to_string()).or_insert(0) += 5;
    }
    if url_low.contains("netflix.com") || url_low.contains("primevideo.com") || url_low.contains("hianime") {
        *scores.entry("Movies".to_string()).or_insert(0) += 5;
    }

    // Pick highest score, require at least 2 points to avoid single weak "app" triggering Software.
    let (best_cat, best_score) = scores.into_iter().max_by_key(|(_, v)| *v)?;
    if best_score < 2 {
        return None;
    }

    // Scene release parser (Plex/Kodi): Movie.Name.2024.1080p.BluRay.x264-YIFY
    // Extract clean title + year for Movies/TV to create <Title> (<Year>)/ subfolder.
    let scene = parse_scene_release(&name);
    // YouTube / domain subfolder: <Category>/<Channel_or_Domain>/
    let domain_folder = domain_folder(url.unwrap_or(""));

    let mut target = match best_cat.as_str() {
        "Movies" => {
            if let Some((clean, year, _)) = scene.as_ref().filter(|(_, y, _)| *y >= 1900 && *y <= 2035) {
                dir.join("Movies").join(format!("{} ({})", clean.clone(), year))
            } else {
                dir.join("Movies")
            }
        }
        "TV" => {
            if let Some((clean, year, _)) = scene.as_ref().filter(|(_, y, _)| *y >= 1900) {
                dir.join("TV Shows").join(format!("{} ({})", clean.clone(), year))
            } else {
                dir.join("TV Shows")
            }
        }
        "Courses" => dir.join("Courses"),
        "Software" => dir.join("Software"),
        "Music" => dir.join("Music"),
        _ => return None,
    };
    // For YouTube and other domains, nest one more level: <Category>/<Domain>/
    // e.g. Downloads/Video/YouTube/file.mp4 or Downloads/Video/TikTok/file.mp4
    if let Some(dom) = domain_folder {
        // Only for video/audio categories to avoid cluttering Courses/Software.
        if best_cat == "Movies" || best_cat == "TV" || best_cat == "Music" {
            // If scene already created a titled subfolder, nest domain inside it? No — keep flat: Movies/<Title> is enough.
            // For non-scene or generic, add domain layer.
            if scene.is_none() {
                target = target.join(dom);
            }
        }
    }
    if target == *dir {
        return None;
    }
    std::fs::create_dir_all(&target).ok()?;
    let dst = target.join(path.file_name()?);
    if dst.exists() {
        return None;
    }
    std::fs::rename(path, &dst).ok()?;
    Some(dst)
}
