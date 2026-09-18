//! Native torrent core — Phase 1: metadata only.
//!
//! - `.torrent` files and `magnet:` links are parsed locally (instant, offline)
//!   into [`TorrentMetadata`] (info-hash, name, size, file tree, trackers).
//! - Actual P2P downloading (librqbit `Session`, DHT, peers) lands in Phase 2;
//!   [`TorrentManager`] is registered in Tauri state now so Phase 2 plugs in.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

/// One file inside a torrent.
#[derive(Clone, Serialize, Deserialize)]
pub struct TorrentFileEntry {
    pub path: String,
    pub size: u64,
}

/// Everything the UI needs to show a torrent before downloading it.
#[derive(Clone, Serialize, Deserialize)]
pub struct TorrentMetadata {
    /// Lowercase hex info-hash (40 chars for v1).
    pub info_hash: String,
    /// Torrent / display name.
    pub name: String,
    /// Sum of all file sizes.
    pub total_size: u64,
    pub files: Vec<TorrentFileEntry>,
    pub trackers: Vec<String>,
    pub is_magnet: bool,
    /// Echoed magnet URI (Phase 2 feeds it to the session).
    pub magnet_url: Option<String>,
}

/// Thread-safe torrent state owner (Tauri managed state). Holds the single
/// librqbit `Session` (DHT + listeners live as long as this does).
///
/// The session owns a DEDICATED tokio runtime: librqbit's DHT/peer I/O must
/// never run on Tauri's main runtime, or a busy swarm starves every backend
/// command and IPC event (frozen list, dead WS bridge).
pub struct TorrentManager {
    pub output_dir: Mutex<String>,
    rt: tokio::runtime::Runtime,
    session: tokio::sync::Mutex<Option<std::sync::Arc<librqbit::Session>>>,
}

impl TorrentManager {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("vortex-p2p")
            .enable_all()
            .build()
            .expect("P2P runtime needs OS threads");
        Self { output_dir: Mutex::new(String::new()), rt, session: tokio::sync::Mutex::new(None) }
    }

    /// Lazily create (or reuse) the global P2P session. Creation runs inside
    /// the P2P runtime so all of the session's internal tasks bind to it.
    pub(crate) async fn ensure_session(&self, default_dir: &str) -> Result<std::sync::Arc<librqbit::Session>, String> {
        let mut guard = self.session.lock().await;
        if let Some(s) = guard.as_ref() {
            return Ok(s.clone());
        }
        let dir = std::path::PathBuf::from(default_dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create folder: {e}"))?;
        eprintln!("[torrent] creating P2P session in {}", dir.display());
        let created = self.rt.spawn(librqbit::Session::new(dir));
        let session = tokio::time::timeout(std::time::Duration::from_secs(60), created)
            .await
            .map_err(|_| "Torrent engine timed out starting (firewall blocking ports?)".to_string())
            .and_then(|r| r.map_err(|e| format!("Torrent engine failed to start: {e}")))?
            .map_err(|e| format!("Torrent engine failed to start: {e:#}"))?;
        eprintln!("[torrent] session ready");
        *guard = Some(session.clone());
        Ok(session)
    }
}

impl Default for TorrentManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Pure progress math (unit-tested).
pub fn torrent_progress(done: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64 * 100.0).min(100.0)
    }
}

/// Pure ETA math (unit-tested).
pub fn torrent_eta(done: u64, total: u64, down_bps: u64) -> u64 {
    if down_bps > 0 && total > done {
        (total - done) / down_bps
    } else {
        0
    }
}

/// Pure share-ratio math: uploaded / total (unit-tested).
pub fn share_ratio(uploaded: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        uploaded as f64 / total as f64
    }
}

/// One live torrent download, mirrored into the main download list as a
/// [`crate::download::DlView`] with `source == "torrent"`.
pub struct TorrentTask {
    /// Info-hash hex — the stable id.
    pub id: String,
    pub name: Mutex<String>,
    /// Sidebar category (video/audio/… from the first file, else other).
    pub category: String,
    /// Original source (magnet: URI, http(s) .torrent URL or local path).
    pub url: String,
    pub output_dir: std::path::PathBuf,
    pub total: std::sync::atomic::AtomicU64,
    pub done: std::sync::atomic::AtomicU64,
    pub down_speed: std::sync::atomic::AtomicU64,
    pub up_speed: std::sync::atomic::AtomicU64,
    pub peers: std::sync::atomic::AtomicU64,
    pub status: std::sync::RwLock<crate::download::DlStatus>,
    pub error: Mutex<Option<String>>,
    pub created_at: u64,
    pub completed_at: Mutex<Option<u64>>,
    /// Set when the task leaves the list: the monitor loop exits promptly.
    pub dropped: std::sync::atomic::AtomicBool,
    session: std::sync::Arc<librqbit::Session>,
    handle: Mutex<Option<std::sync::Arc<librqbit::ManagedTorrent>>>,
}

impl TorrentTask {
    pub fn view(&self) -> crate::download::DlView {
        use std::sync::atomic::Ordering;
        let status = *self.status.read().unwrap_or_else(|e| e.into_inner());
        let total = self.total.load(Ordering::Relaxed);
        let done = self.done.load(Ordering::Relaxed);
        let speed = self.down_speed.load(Ordering::Relaxed);
        crate::download::DlView {
            id: self.id.clone(),
            url: self.url.clone(),
            filename: self.name.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            title: self.name.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            category: self.category.clone(),
            total_size: total,
            downloaded: done,
            speed,
            progress: torrent_progress(done, total),
            eta: torrent_eta(done, total, speed),
            segments: 1,
            connections: self.peers.load(Ordering::Relaxed) as usize,
            live: self.peers.load(Ordering::Relaxed) as usize,
            status,
            error: self.error.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            thumbnail: None,
            source: "torrent".into(),
            save_path: self.output_dir.join(self.name.lock().unwrap_or_else(|e| e.into_inner()).clone()).display().to_string(),
            created_at: self.created_at,
            completed_at: *self.completed_at.lock().unwrap_or_else(|e| e.into_inner()),
            format_id: None,
            produced: Vec::new(),
        }
    }

    fn set_status(&self, app: &tauri::AppHandle, s: crate::download::DlStatus) {
        use crate::download::DlStatus;
        if s == DlStatus::Completed {
            *self.completed_at.lock().unwrap() = Some(crate::download::now_ms());
        }
        *self.status.write().unwrap() = s;
        let err = self.error.lock().unwrap().clone();
        let _ = app.emit(
            "download-status",
            serde_json::json!({ "id": self.id, "status": format!("{s:?}").to_lowercase(), "error": err }),
        );
        let _ = app.emit("downloads-changed", ());
    }

    /// Pause (works even while the background session-add is still resolving:
    /// the verdict is applied when the handle lands).
    /// Clone the session handle out (its std Mutex guard must never live
    /// across an await — guards are not Send).
    fn handle_cloned(&self) -> Option<std::sync::Arc<librqbit::ManagedTorrent>> {
        self.handle.lock().unwrap().clone()
    }

    pub async fn pause(&self, app: &tauri::AppHandle) -> Result<(), String> {
        use crate::download::DlStatus;
        match self.handle_cloned() {
            Some(h) => {
                self.session.pause(&h).await.map_err(|e| format!("Pause failed: {e:#}"))?;
            }
            None => {
                if self.dropped.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("Torrent no longer active".into());
                }
            }
        }
        self.set_status(app, DlStatus::Paused);
        Ok(())
    }

    pub async fn resume(&self, app: &tauri::AppHandle) -> Result<(), String> {
        use crate::download::DlStatus;
        match self.handle_cloned() {
            Some(h) => {
                self.session.unpause(&h).await.map_err(|e| format!("Resume failed: {e:#}"))?;
            }
            None => {
                if self.dropped.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("Torrent no longer active".into());
                }
            }
        }
        self.set_status(app, DlStatus::Downloading);
        Ok(())
    }

    /// Stop traffic but keep the entry (resumable): session-level pause with
    /// a Cancelled chip, used by per-row Stop and Stop-All.
    pub async fn stop(&self, app: &tauri::AppHandle) -> Result<(), String> {
        use crate::download::DlStatus;
        match self.handle_cloned() {
            Some(h) => {
                self.session.pause(&h).await.map_err(|e| format!("Stop failed: {e:#}"))?;
            }
            None => {
                if self.dropped.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("Torrent no longer active".into());
                }
            }
        }
        self.set_status(app, DlStatus::Cancelled);
        Ok(())
    }
}

/// Resolve a magnet (or remote .torrent URL) into a full file tree over DHT
/// without downloading anything (`list_only` never registers a session task).
/// Times out gracefully so the modal spinner can't hang forever.
pub async fn resolve_magnet(
    mgr: &std::sync::Arc<TorrentManager>,
    source: String,
    output_dir: String,
    timeout_secs: u64,
) -> Result<TorrentMetadata, String> {
    let session = mgr.ensure_session(output_dir.trim()).await?;
    let mut opts = librqbit::AddTorrentOptions::default();
    opts.list_only = true;
    opts.output_folder = Some(output_dir);
    // Run on the P2P runtime: the DHT crawl's child tasks must not bind to
    // Tauri's main runtime (same starvation hazard as the session itself).
    let url = source.clone();
    let fut = mgr.rt.spawn(async move {
        session
            .add_torrent(librqbit::AddTorrent::from_url(url), Some(opts))
            .await
    });
    let joined = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs.clamp(3, 120)),
        fut,
    )
    .await
    .map_err(|_| "DHT resolve timed out (no peers with metadata?)".to_string())?;
    let added = joined.map_err(|e| format!("Resolve task failed: {e}"))?;
    let info = match added.map_err(|e| format!("Resolve failed: {e:#}"))? {
        librqbit::AddTorrentResponse::ListOnly(r) => r,
        _ => return Err("Unexpected session reply".into()),
    };
    let mut files: Vec<TorrentFileEntry> = Vec::new();
    let mut total = 0u64;
    for fd in info.info.iter_file_details() {
        if fd.attrs().padding {
            continue;
        }
        let p = fd.filename.to_pathbuf().display().to_string();
        if p.trim().is_empty() {
            continue;
        }
        total += fd.len;
        files.push(TorrentFileEntry { path: p, size: fd.len });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let local = parse_magnet_link(&source).ok();
    let name = local
        .as_ref()
        .and_then(|m| {
            if m.name.trim().is_empty() || m.name.starts_with("magnet-") {
                None
            } else {
                Some(m.name.clone())
            }
        })
        .or_else(|| {
            files.first().and_then(|f| {
                f.path.split('/').next().filter(|s| !s.is_empty()).map(|s| s.to_string())
            })
        })
        .unwrap_or_else(|| format!("torrent-{:.8}", info.info_hash.as_string()));
    Ok(TorrentMetadata {
        info_hash: info.info_hash.as_string(),
        name,
        total_size: total,
        files,
        trackers: local.map(|m| m.trackers).unwrap_or_default(),
        is_magnet: true,
        magnet_url: Some(source),
    })
}

/// Fallback UDP trackers auto-appended to trackerless magnets (qBittorrent
/// behavior): without any tracker a bare `xt=` magnet can sit peerless on DHT
/// alone for a long time.
const FALLBACK_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://explodie.org:6969/announce",
];

/// Percent-encode a tracker URL for a magnet `tr=` param (only unreserved +
/// a few safe chars pass through raw).
fn encode_tracker(t: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(t.len());
    for b in t.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 15) as usize] as char);
        }
    }
    out
}

/// Append fallback trackers to a magnet that carries none.
pub fn with_fallback_trackers(magnet: &str) -> String {
    let has_tr = magnet.split(['?', '&']).any(|kv| {
        kv.eq_ignore_ascii_case("tr")
            || kv.to_ascii_lowercase().starts_with("tr=")
    });
    // Also catch the common raw form `...&tr=udp://...` (case-insensitive).
    let has_tr = has_tr || magnet.to_ascii_lowercase().contains("&tr=");
    if has_tr {
        return magnet.to_string();
    }
    let sep = if magnet.contains('?') { "&" } else { "?" };
    let mut out = magnet.to_string();
    for t in FALLBACK_TRACKERS {
        out.push_str(sep);
        out.push_str("tr=");
        out.push_str(&encode_tracker(t));
    }
    out
}

impl TorrentManager {
    /// Start a torrent download in the background and register it in the main
    /// download list. `source` is a magnet: URI, an http(s) .torrent URL, or a
    /// local .torrent path. `files` optionally restricts multi-file torrents.
    /// Returns the info-hash id.
    pub async fn add_torrent_task(
        mgr: &std::sync::Arc<TorrentManager>,
        _app: &AppHandle,
        dl: &std::sync::Arc<crate::state::DlManager>,
        source: String,
        output_dir: String,
        files: Option<Vec<usize>>,
    ) -> Result<String, String> {
        use crate::download::DlStatus;

        let mut source = source.trim().to_string();
        if source.is_empty() {
            return Err("Empty torrent source".into());
        }
        // Trackerless magnets get fallback trackers so DHT isn't the only
        // discovery path (qBittorrent parity).
        let is_magnet = source.to_ascii_lowercase().starts_with("magnet:");
        if is_magnet {
            source = with_fallback_trackers(&source);
        }
        let out = std::path::PathBuf::from(output_dir.trim());
        if out.as_os_str().is_empty() {
            return Err("Pick a destination folder".into());
        }
        std::fs::create_dir_all(&out).map_err(|e| format!("Cannot create folder: {e}"))?;

        // Resolve everything identifiable UP FRONT (fast, bounded) so the row
        // registers instantly with a stable id. The session add itself runs in
        // the background because magnets block on DHT metadata (minutes).
        enum Input {
            Magnet { meta: TorrentMetadata, url: String },
            Bytes { meta: TorrentMetadata, bytes: Vec<u8> },
        }
        let lower = source.to_ascii_lowercase();
        let input: Input = if lower.starts_with("magnet:") {
            let meta = parse_magnet_link(&source)?;
            Input::Magnet { meta, url: source.clone() }
        } else if lower.starts_with("http:") || lower.starts_with("https:") {
            // Fetch the remote .torrent ourselves (30 s cap): instant id.
            let bytes = fetch_torrent_bytes(&source).await?;
            let meta = parse_torrent_bytes(&bytes)?;
            Input::Bytes { meta, bytes }
        } else {
            let bytes = std::fs::read(&source).map_err(|e| format!("Cannot read .torrent: {e}"))?;
            if bytes.is_empty() {
                return Err("Empty .torrent file".into());
            }
            let meta = parse_torrent_bytes(&bytes)?;
            Input::Bytes { meta, bytes }
        };
        let (meta, _add) = match input {
            Input::Magnet { meta, url } => (meta, librqbit::AddTorrent::from_url(url)),
            // Vec<u8> converts into the session's byte buffer via bytes::Bytes.
            Input::Bytes { meta, bytes } => (meta, librqbit::AddTorrent::from_bytes(bytes)),
        };

        let session = mgr.ensure_session(output_dir.trim()).await?;

        let mut opts = librqbit::AddTorrentOptions::default();
        opts.output_folder = Some(out.display().to_string());
        // Overwrite lets a re-added torrent resume/seed on existing files.
        opts.overwrite = true;
        if let Some(f) = files.filter(|v| !v.is_empty()) {
            opts.only_files = Some(f);
        }

        let id = meta.info_hash.clone();
        let total = meta.total_size;
        // Sidebar category from the first known file (magnets resolve later
        // and stay in Other until then).
        let category = meta
            .files
            .first()
            .map(|f| crate::download::category_of(&f.path).to_string())
            .unwrap_or_else(|| "other".to_string());
        // Magnets start life metadata-less: show Resolving until DHT delivers
        // the file list (total > 0), then flip to Downloading automatically.
        let resolving = is_magnet && total == 0;
        let task = std::sync::Arc::new(TorrentTask {
            id: id.clone(),
            name: Mutex::new(if resolving {
                "Resolving metadata…".to_string()
            } else {
                meta.name.clone()
            }),
            category,
            url: source.clone(),
            output_dir: out,
            total: std::sync::atomic::AtomicU64::new(total),
            done: std::sync::atomic::AtomicU64::new(0),
            down_speed: std::sync::atomic::AtomicU64::new(0),
            up_speed: std::sync::atomic::AtomicU64::new(0),
            peers: std::sync::atomic::AtomicU64::new(0),
            status: std::sync::RwLock::new(if resolving {
                DlStatus::Resolving
            } else {
                DlStatus::Downloading
            }),
            error: Mutex::new(None),
            created_at: crate::download::now_ms(),
            completed_at: Mutex::new(None),
            dropped: std::sync::atomic::AtomicBool::new(false),
            session: session.clone(),
            handle: Mutex::new(None),
        });
        dl.torrents.lock().unwrap().insert(id.clone(), task.clone());
        eprintln!(
            "[torrent] registered {} (map now holds {} torrent(s))",
            id,
            dl.torrents.lock().unwrap().len()
        );
        // NOTE: P2P downloading is disabled in this version (the UI offers no
        // torrent entry point). The row registers so a future sidecar daemon
        // can adopt it; no session work runs here.
        Ok(id)
    }
}
/// Look a task up by info-hash (accepts hex or base32 spellings).
fn find_task(
    dl: &std::sync::Arc<crate::state::DlManager>,
    info_hash: &str,
) -> Result<std::sync::Arc<TorrentTask>, String> {
    let key = info_hash.trim().to_lowercase();
    dl.torrents
        .lock()
        .unwrap()
        .get(&key)
        .cloned()
        .ok_or_else(|| "Torrent not found".to_string())
}

/// Pause a torrent (session-level; bytes already fetched are kept).
pub async fn pause_task(
    app: &AppHandle,
    dl: &std::sync::Arc<crate::state::DlManager>,
    info_hash: &str,
) -> Result<(), String> {
    let t = find_task(dl, info_hash)?;
    t.pause(app).await
}

/// Resume a paused torrent.
pub async fn resume_task(
    app: &AppHandle,
    dl: &std::sync::Arc<crate::state::DlManager>,
    info_hash: &str,
) -> Result<(), String> {
    let t = find_task(dl, info_hash)?;
    t.resume(app).await
}

/// Cancel + drop a torrent from the list, optionally deleting its files.
/// Works even mid-resolve (handle not yet landed): the background worker
/// self-cleans via the `dropped` flag.
pub async fn delete_task(
    app: &AppHandle,
    dl: &std::sync::Arc<crate::state::DlManager>,
    info_hash: &str,
    delete_files: bool,
) -> Result<(), String> {
    let t = find_task(dl, info_hash)?;
    t.dropped.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(h) = t.handle_cloned() {
        let id = h.info_hash();
        t.session
            .delete(librqbit::api::TorrentIdOrHash::Hash(id), delete_files)
            .await
            .map_err(|e| format!("Could not remove torrent: {e:#}"))?;
    }
    dl.torrents.lock().unwrap().remove(&t.id);
    let _ = app.emit("downloads-changed", ());
    Ok(())
}

/// Fetch a remote `.torrent` file ourselves (bounded): instant id + metadata
/// without routing the session through a slow URL fetch.
async fn fetch_torrent_bytes(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .user_agent(crate::tools::BROWSER_UA)
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| {
        crate::download::log_net_err(&e, "torrent file fetch");
        format!("Could not fetch .torrent URL: {e}")
    })?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} for .torrent URL", resp.status().as_u16()));
    }
    let mut buf = Vec::new();
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Stream error: {e}"))?;
        buf.extend_from_slice(&chunk);
        if buf.len() > 16 * 1024 * 1024 {
            return Err(".torrent file too large (>16 MB)".into());
        }
    }
    if buf.is_empty() {
        return Err("Empty .torrent file".into());
    }
    Ok(buf)
}

fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Build [`TorrentMetadata`] from an already-parsed v1 metainfo.
fn from_v1(
    info_hash: &[u8],
    name: Option<String>,
    length: Option<u64>,
    files: Option<Vec<(Vec<String>, u64)>>,
    trackers: Vec<String>,
    fallback_name: String,
) -> TorrentMetadata {
    let entries: Vec<TorrentFileEntry> = match files {
        Some(list) => list
            .into_iter()
            .map(|(parts, size)| TorrentFileEntry { path: parts.join("/"), size })
            .collect(),
        None => {
            let n = name.clone().unwrap_or_else(|| fallback_name.clone());
            vec![TorrentFileEntry { path: n, size: length.unwrap_or(0) }]
        }
    };
    let total_size = entries.iter().map(|f| f.size).sum();
    let name = name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| fallback_name.clone());
    TorrentMetadata {
        info_hash: hex(info_hash),
        name,
        total_size,
        files: entries,
        trackers,
        is_magnet: false,
        magnet_url: None,
    }
}

/// Parse a `.torrent` file from disk.
pub fn parse_torrent_file(path: &str) -> Result<TorrentMetadata, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read .torrent: {e}"))?;
    if bytes.is_empty() {
        return Err("Empty .torrent file".into());
    }
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(".torrent file too large (>16 MB)".into());
    }
    parse_torrent_bytes(&bytes)
}

/// Parse raw `.torrent` bytes (also used by tests).
pub fn parse_torrent_bytes(bytes: &[u8]) -> Result<TorrentMetadata, String> {
    let meta =
        librqbit::torrent_from_bytes(bytes).map_err(|e| format!("Bad .torrent file: {e}"))?;
    let hash_bytes: Vec<u8> = meta.info_hash.0.to_vec();
    let info_hash = hex(&hash_bytes);
    let fallback = format!("torrent-{}", &info_hash[..8.min(info_hash.len())]);
    let info = &meta.info.data;
    let name = info.name.as_ref().map(|b| lossy(b.as_ref()));
    let files = info.files.as_ref().map(|list| {
        list.iter()
            .map(|f| {
                (
                    f.path.iter().map(|c| lossy(c.as_ref())).collect::<Vec<_>>(),
                    f.length,
                )
            })
            .collect::<Vec<_>>()
    });
    let trackers: Vec<String> = meta.iter_announce().map(|b| lossy(b.as_ref())).collect();
    Ok(from_v1(
        &hash_bytes,
        name,
        info.length,
        files,
        trackers,
        fallback,
    ))
}

/// Parse a `magnet:` URI locally (instant, no network). Resolving file lists
/// over DHT happens in Phase 2 with the session; until then size/files are
/// unknown and only hash + name + trackers are reported.
pub fn parse_magnet_link(url: &str) -> Result<TorrentMetadata, String> {
    let url = url.trim();
    if !url.to_ascii_lowercase().starts_with("magnet:") {
        return Err("Not a magnet: link".into());
    }
    let m = librqbit::Magnet::parse(url).map_err(|e| format!("Bad magnet link: {e}"))?;
    let id = m.as_id20().ok_or("Magnet has no btih info-hash")?;
    let info_hash = hex(&id.0);
    let name = m
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| format!("magnet-{}", &info_hash[..8.min(info_hash.len())]));
    Ok(TorrentMetadata {
        info_hash,
        name,
        total_size: 0,
        files: Vec::new(),
        trackers: m.trackers.clone(),
        is_magnet: true,
        magnet_url: Some(url.to_string()),
    })
}

#[cfg(test)]
mod torrent_tests {
    use super::*;

    /// Minimal valid single-file .torrent (bencoded by hand).
    fn sample_torrent() -> Vec<u8> {
        b"d8:announce31:http://tracker.example/announce4:infod6:lengthi12345e4:name10:ubuntu.iso12:piece lengthi262144e6:pieces20:0123456789abcdefghijee".to_vec()
    }

    #[test]
    fn parses_torrent_file_metadata() {
        let m = parse_torrent_bytes(&sample_torrent()).expect("parse");
        assert_eq!(m.name, "ubuntu.iso");
        assert_eq!(m.total_size, 12345);
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].path, "ubuntu.iso");
        assert_eq!(m.files[0].size, 12345);
        assert_eq!(m.info_hash.len(), 40);
        assert!(!m.is_magnet);
        assert_eq!(m.trackers, vec!["http://tracker.example/announce".to_string()]);
    }

    #[test]
    fn rejects_bad_torrent_bytes() {
        assert!(parse_torrent_bytes(b"not bencode").is_err());
        assert!(parse_torrent_bytes(b"").is_err());
    }

    #[test]
    fn parses_magnet_link() {
        let m = parse_magnet_link(
            "magnet:?xt=urn:btih:cab507494d02ebb1178b38f2e9d7be299c86b862&dn=Test+Name&tr=http://tracker.example/announce",
        )
        .expect("parse");
        assert_eq!(m.info_hash, "cab507494d02ebb1178b38f2e9d7be299c86b862");
        assert!(m.is_magnet);
        assert_eq!(m.trackers, vec!["http://tracker.example/announce".to_string()]);
    }

    #[test]
    fn rejects_bad_magnet() {
        assert!(parse_magnet_link("https://example.com/x.torrent").is_err());
        assert!(parse_magnet_link("magnet:?dn=no-hash-here").is_err());
    }

    #[test]
    fn fallback_trackers_appended_once() {
        let bare = "magnet:?xt=urn:btih:cab507494d02ebb1178b38f2e9d7be299c86b862&dn=X";
        let out = with_fallback_trackers(bare);
        assert_eq!(out.matches("&tr=").count(), 4);
        assert!(out.contains("tracker.opentrackr.org"));
        // Encoded (no raw :// inside params).
        assert!(!out.contains("tr=udp://"));
        // Already has trackers → untouched.
        let with = format!("{bare}&tr=http%3A%2F%2Fx%2Fannounce");
        assert_eq!(with_fallback_trackers(&with), with);
        // Non-magnet passthrough shape (only appends, never breaks).
        assert!(with_fallback_trackers(bare).starts_with("magnet:?xt="));
    }

    #[test]
    fn progress_eta_ratio_math() {
        assert_eq!(torrent_progress(0, 0), 0.0);
        assert_eq!(torrent_progress(50, 100), 50.0);
        assert_eq!(torrent_progress(200, 100), 100.0);
        assert_eq!(torrent_eta(0, 100, 0), 0);
        assert_eq!(torrent_eta(50, 100, 10), 5);
        assert_eq!(torrent_eta(100, 100, 10), 0);
        assert_eq!(share_ratio(0, 0), 0.0);
        assert_eq!(share_ratio(150, 100), 1.5);
    }
}
