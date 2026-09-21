use std::sync::Arc;

use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use tauri::{AppHandle, Emitter, Manager};

use crate::state::DlManager;
use crate::{download, grabber, tools, ytdlp};

pub const PORT: u16 = 17190;
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub fn ws_token(app: &AppHandle) -> String {
    // Persistent random token stored in app data dir, used to authenticate extension
    let dir = app.path().app_data_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join(".ws_token");
    if let Ok(t) = std::fs::read_to_string(&p) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            harden_token_file(&p);
            return t;
        }
    }
    let t = uuid::Uuid::new_v4().to_string();
    if std::fs::write(&p, &t).is_ok() {
        harden_token_file(&p);
    }
    t
}

/// Best-effort owner-only permissions on the token file (defense in depth;
/// the token is also dealt to frontend JS via get_ws_token by design).
fn harden_token_file(p: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    // Windows: %APPDATA% files already inherit user-private ACLs, and there
    // is no ACL API without extra deps — nothing to do.
    #[cfg(windows)]
    {
        let _ = p;
    }
}

pub async fn run(app: AppHandle) {
    let token = ws_token(&app);
    // Retry the bind briefly: a previous instance shutting down (or a slow
    // port release after a crash) must not leave the bridge silently dead —
    // without it the extension only sees ERR_CONNECTION_REFUSED.
    let mut listener = None;
    for attempt in 1..=5 {
        match TcpListener::bind(("127.0.0.1", PORT)).await {
            Ok(l) => {
                listener = Some(l);
                break;
            }
            Err(e) => {
                eprintln!("[vortex-ws] bind 127.0.0.1:{PORT} failed (attempt {attempt}/5): {e}");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
    let listener = match listener {
        Some(l) => l,
        None => {
            eprintln!("[vortex-ws] bridge disabled: port {PORT} busy (another instance already running?)");
            return;
        }
    };
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let app = app.clone();
        let tok = token.clone();
        tokio::spawn(async move {
            let _ = handle_conn(app, stream, tok).await;
        });
    }
}

async fn handle_conn(app: AppHandle, mut stream: TcpStream, token: String) -> std::io::Result<()> {
    // Slowloris guard: the whole handshake must finish within 5 s, otherwise
    // an idle connection parks a task + buffer forever.
    let (key, origin) = match tokio::time::timeout(std::time::Duration::from_secs(5), handshake(&mut stream)).await {
        Ok(Ok(v)) => v,
        _ => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
            return Ok(());
        }
    };

    // ---- Auth: require matching token via Sec-WebSocket-Protocol header ----
    // The extension must echo back the token it received at first connection.
    // For the initial handshake, the extension has no token yet; it uses "vortex-register".
    // After first connect, the desktop sends the token back and the extension must use it.

    // ---- Origin enforcement: browser extensions + local pages only ----
    // Parsed strictly (host-exact): the old prefix check accepted
    // `http://localhost.evil.com`. The message-level token gate still applies.
    let origin_ok = origin_allowed(origin.as_deref());
    if !origin_ok {
        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
        return Ok(());
    }

    let accept = accept_key(&key);
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept
    );
    stream.write_all(resp.as_bytes()).await?;

    // Reader/writer split: one task owns the read half and dispatches each
    // message to its own task; a single writer task owns the write half and
    // sends replies FIFO. A slow `grab_site`/`analyze` no longer stalls
    // ping/close/subsequent commands, and every reply still carries its `req`
    // id so the extension routes it (out-of-order across reqs is fine).
    let (mut rd, mut wr) = stream.into_split();
    let (wtx, mut wrx) = tokio::sync::mpsc::unbounded_channel::<(u8, Vec<u8>)>();
    let writer = tokio::spawn(async move {
        while let Some((opcode, payload)) = wrx.recv().await {
            if write_frame(&mut wr, opcode, &payload).await.is_err() {
                break;
            }
        }
    });

    let mut frag: Vec<u8> = Vec::new();
    // Fragmented-message bomb guard: continuation frames (fin=0) reassemble
    // into `frag` — cap the running total (per-frame cap alone is bypassable).
    const MAX_FRAG_TOTAL: usize = 5 * 1024 * 1024;
    let mut authenticated = false;
    loop {
        let (fin, opcode, payload) = match read_frame(&mut rd).await {
            Ok(f) => f,
            Err(_) => break,
        };
        // `send` fails only when the writer is gone (dead socket): stop.
        macro_rules! emit {
            ($op:expr, $bytes:expr) => {
                if wtx.send(($op, $bytes)).is_err() {
                    break;
                }
            };
        }
        match opcode {
            0x8 => {
                emit!(0x8, payload);
                break;
            }
            0x9 => {
                emit!(0xA, payload);
            }
            0x1 | 0x0 => {
                if frag.len().saturating_add(payload.len()) > MAX_FRAG_TOTAL {
                    break; // oversized fragmented message: drop the connection
                }
                frag.extend_from_slice(&payload);
                if !fin {
                    continue;
                }
                let text = String::from_utf8_lossy(&frag).into_owned();
                frag.clear();

                // Auth gate: first message must be {"type":"auth","token":"<token>"}
                if !authenticated {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("type").and_then(|t| t.as_str()) == Some("auth") {
                            let msg_token = v.get("token").and_then(|t| t.as_str()).unwrap_or("");
                            if msg_token == token {
                                authenticated = true;
                                emit!(0x1, br#"{"type":"auth","ok":true}"#.to_vec());
                                continue;
                            }
                        }
                    }
                    emit!(0x8, b"unauthorized".to_vec());
                    break;
                }

                // ---- Instant acknowledgment for long-running commands ----
                // No probe / async work happens before this ack, so the extension
                // popup (which races against a short timeout) always gets a reply
                // within microseconds. dispatch() below keeps working in the
                // background and emits events when the job really starts.
                // NOTE: `start_ytdl` is deliberately NOT acked here. Its reply
                // must be the real result: rpc() resolves on the FIRST frame
                // carrying its req, so an early ack would report success while
                // the background job fails ("Added ✓" with nothing in the app).
                // ytdlp::start() is pure construction (no network), so the real
                // reply below is fast anyway.
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let req = v.get("req").and_then(|r| r.as_str()).unwrap_or("");
                    if matches!(typ, "download" | "start_download" | "open_grabber") {
                        let mut ack = json!({"type":"ack","ok":true,"success":true,"action":"accepted"});
                        if !req.is_empty() {
                            ack["req"] = json!(req);
                        }
                        emit!(0x1, ack.to_string().into_bytes());
                    }
                }

                // Heavy commands (grab_site/analyze) run detached: the read
                // loop stays responsive and the reply still carries `req`.
                let app2 = app.clone();
                let wtx2 = wtx.clone();
                tokio::spawn(async move {
                    let reply = dispatch(&app2, &text).await;
                    let _ = wtx2.send((0x1, reply.into_bytes()));
                });
            }
            0x2 => { /* binary — ignore */ }
            _ => {}
        }
    }
    drop(wtx);
    // Flush queued replies (bounded wait so close stays prompt), then the
    // split halves drop and the socket closes.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), writer).await;
    Ok(())
}

async fn handshake(stream: &mut TcpStream) -> Result<(String, Option<String>), ()> {
    let mut buf = [0u8; 8192];
    let mut read = 0usize;
    loop {
        let n = stream.read(&mut buf[read..]).await.map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        read += n;
        if buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if read >= buf.len() {
            return Err(());
        }
    }
    let head = String::from_utf8_lossy(&buf[..read]);
    let mut lines = head.lines();
    let status = lines.next().unwrap_or("");
    if !status.contains("get") && !status.contains("GET") {
        return Err(());
    }
    let mut key = String::new();
    let mut upgrade = false;
    let mut origin: Option<String> = None;
    for l in lines {
        let (name, value) = match l.split_once(':') {
            Some((n, v)) => (n.trim(), v.trim()),
            None => continue,
        };
        match name.to_ascii_lowercase().as_str() {
            "sec-websocket-key" => key = value.to_string(),
            "upgrade" if value.eq_ignore_ascii_case("websocket") => upgrade = true,
            "origin" => origin = Some(value.to_string()),
            _ => {}
        }
    }
    if !upgrade || key.is_empty() {
        return Err(());
    }
    Ok((key, origin))
}

/// Strict origin check for the WS handshake: extension schemes (any id —
/// the token gate authorizes) plus local dev pages with an EXACT host match.
fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(o) = origin else { return false };
    if let Some(rest) = o
        .strip_prefix("chrome-extension://")
        .or_else(|| o.strip_prefix("moz-extension://"))
    {
        return !rest.is_empty();
    }
    if let Some(rest) = o.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or("");
        return host == "localhost" || host == "127.0.0.1";
    }
    false
}

fn accept_key(key: &str) -> String {    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(GUID.as_bytes());
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &h.finalize())
}

async fn read_frame(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> std::io::Result<(bool, u8, Vec<u8>)> {
    let a = stream.read_u8().await?;
    let b = stream.read_u8().await?;
    let fin = a & 0x80 != 0;
    let opcode = a & 0x0f;
    let masked = b & 0x80 != 0;
    let mut len = (b & 0x7f) as u64;
    if len == 126 {
        len = stream.read_u16().await? as u64;
    } else if len == 127 {
        len = stream.read_u64().await?;
    }
    if len > 1024 * 1024 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut mask = [0u8; 4];
    if masked {
        stream.read_exact(&mut mask).await?;
    }
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;
    if masked {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i & 3];
        }
    }
    Ok((fin, opcode, payload))
}

async fn write_frame(stream: &mut (impl tokio::io::AsyncWrite + Unpin), opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut head = vec![0x80 | opcode];
    if payload.len() < 126 {
        head.push(payload.len() as u8);
    } else if payload.len() <= 0xFFFF {
        head.push(126);
        head.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        head.push(127);
        head.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    stream.write_all(&head).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}

async fn dispatch(app: &AppHandle, msg: &str) -> String {
    let v: serde_json::Result<Value> = serde_json::from_str(msg);
    let req = match &v {
        Ok(x) => x.get("req").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        Err(_) => String::new(),
    };
    let out: String = match v {
        Err(e) => err(&format!("invalid json: {e}")),
        Ok(v) => {
            let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let mgr = app.state::<Arc<DlManager>>().inner().clone();
            match typ {
                "intercept" => {
                    let p = &v["payload"];
                    if crate::state::load_settings(app).show_download_info {
                        let payload = serde_json::json!({
                            "url": p["url"].as_str().unwrap_or(""),
                            "filename": p["filename"].as_str().unwrap_or(""),
                            "referer": p["referer"].as_str().unwrap_or(""),
                            "cookies": p["cookies"].as_str().unwrap_or(""),
                            "ytdl": p.get("format_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|fid| serde_json::json!({"format_id": fid})),
                        });
                        crate::open_info_window(app, payload);
                        json!({"type":"ack","ok":true,"intercepted":true,"dialog":true}).to_string()
                    } else {
                        let _ = app.emit("download-intercept", json!({
                            "url": p["url"].as_str().unwrap_or(""),
                            "filename": p["filename"].as_str().unwrap_or(""),
                            "referer": p["referer"].as_str().unwrap_or(""),
                            "cookies": p["cookies"].as_str().unwrap_or(""),
                            "format_id": p["format_id"].as_str().unwrap_or(""),
                            "is_ytdl": p["is_ytdl"].as_bool().unwrap_or(false),
                        }));
                        json!({"type":"ack","ok":true,"intercepted":true}).to_string()
                    }
                }
                "ping" => json!({"type":"pong"}).to_string(),
                "download" => {
                    let p = &v["payload"];
                    let url = p["url"].as_str().unwrap_or("").to_string();
                    if url.is_empty() {
                        err("url required")
                    } else if crate::state::load_settings(app).show_download_info {
                        // Dialog mode: standalone File Info window instead of
                        // auto-starting (OFF keeps today's direct path below).
                        // The main dashboard is never raised for these prompts.
                        let payload = serde_json::json!({
                            "url": url,
                            "filename": p["filename"].as_str().unwrap_or(""),
                            "referer": p["referer"].as_str().unwrap_or(""),
                            "cookies": p["cookies"].as_str().unwrap_or(""),
                        });
                        crate::open_info_window(app, payload);
                        json!({"type":"ack","ok":true,"success":true,"action":"dialog_opened"}).to_string()
                    } else {
                        // Acknowledge immediately; the probe/yt-dlp work runs in the
                        // background so the extension popup never times out.
                        let app2 = app.clone();
                        let mgr2 = mgr.clone();
                        let payload = p.clone();
                        let url2 = url.clone();
                        tokio::spawn(async move {
                            // Acknowledge already sent; surface any background failure
                            // in the app UI so silent drop-ins never happen again.
                            let res = download_op(&app2, &mgr2, &payload).await;
                            let j: Value = serde_json::from_str(&res).unwrap_or(Value::Null);
                            if j.get("type").and_then(|t| t.as_str()) != Some("started") {
                                let _ = app2.emit(
                                    "dl-error",
                                    json!({ "via": "extension", "url": url2, "error": j.get("error").cloned().unwrap_or(json!(res)) }),
                                );
                            }
                        });
                        json!({"type":"ack","ok":true,"success":true,"action":"download_started"}).to_string()
                    }
                }
                "analyze" => {
                    let p = &v["payload"];
                    let url = p["url"].as_str().unwrap_or("").to_string();
                    if url.is_empty() {
                        err("url required")
                    } else {
                        let ctx = ytdlp::StreamCtx {
                            referer: p["referer"].as_str().map(|s| s.to_string()),
                            user_agent: p["user_agent"].as_str().map(|s| s.to_string()),
                            cookies: p["cookies"].as_str().map(|s| s.to_string()),
                        };
                        match ytdlp::fetch_info(app.clone(), url.clone(), ctx).await {
                            Ok(info) => json!({"type":"info","ok":true,"url":url,"info":info}).to_string(),
                            Err(e) => err(&e),
                        }
                    }
                }
                "start_ytdl" => {
                    let p = &v["payload"];
                    let url = p["url"].as_str().unwrap_or("").to_string();
                    let fid = p["format_id"].as_str().unwrap_or("").to_string();
                    if url.is_empty() || fid.is_empty() {
                        err("url and format_id required")
                    } else if crate::state::load_settings(app).show_download_info {
                        // Dialog mode: hover-bar clicks pop the standalone File
                        // Info window (title/size unknown here — the modal shows
                        // what the extension sent). OFF = direct start below.
                        let payload = serde_json::json!({
                            "url": url,
                            "filename": "",
                            "referer": p["referer"].as_str().unwrap_or(""),
                            "cookies": p["cookies"].as_str().unwrap_or(""),
                            "ytdl": {
                                "format_id": fid,
                                "playlist": p["playlist"].as_bool().unwrap_or(false),
                                "playlist_items": p["playlist_items"].as_str().unwrap_or(""),
                                "embed_subs": p["embed_subs"].as_bool().unwrap_or(true),
                                "sub_langs": p["sub_langs"].as_str().unwrap_or("all"),
                                "embed_thumbnail": p["embed_thumbnail"].as_bool().unwrap_or(true),
                                "auto_subs": p["auto_subs"].as_bool().unwrap_or(false),
                            },
                        });
                        crate::open_info_window(app, payload);
                        json!({"type":"ack","ok":true,"success":true,"action":"dialog_opened"}).to_string()
                    } else {
                        // Synchronous: ytdlp::start() does no network (tool
                        // fetching happens in launch()), so this returns the
                        // REAL result in the reply frame the extension waits
                        // for — never a premature "accepted".
                        let playlist = p["playlist"].as_bool().unwrap_or(false);
                        let playlist_items = p["playlist_items"].as_str().unwrap_or("").to_string();
                        let start_at = p["start_at"].as_u64();
                        let embed_subs = p["embed_subs"].as_bool();
                        let sub_langs = p["sub_langs"].as_str().map(|s| s.to_string());
                        let embed_thumbnail = p["embed_thumbnail"].as_bool();
                        let auto_subs = p["auto_subs"].as_bool();
                        let ctx = ytdlp::StreamCtx {
                            referer: p["referer"].as_str().map(|s| s.to_string()),
                            user_agent: p["user_agent"].as_str().map(|s| s.to_string()),
                            cookies: p["cookies"].as_str().map(|s| s.to_string()),
                        };
                        let sp = settings_path(app);
                        start_ytdl(app, &mgr, url, fid, sp, playlist, playlist_items, start_at, embed_subs, sub_langs, embed_thumbnail, auto_subs, ctx).await
                    }
                }
                "resume" => {
                    let p = &v["payload"];
                    let id = p["id"].as_str().unwrap_or("").to_string();
                    resume_http(&mgr, id)
                }
                "retry_all" => {
                    let n = mgr.retry_all();
                    json!({"type":"retried","ok":true,"count":n}).to_string()
                }
                "set_auth" => {
                    let p = &v["payload"];
                    let id = p["id"].as_str().unwrap_or("").to_string();
                    let host = p["host"].as_str().unwrap_or("").to_string();
                    let username = p["username"].as_str().unwrap_or("").to_string();
                    let password = p["password"].as_str().unwrap_or("").to_string();
                    let remember = p["remember"].as_bool().unwrap_or(true);
                    let n = mgr.apply_credentials(
                        &id,
                        crate::auth::Cred { host, username, password },
                        remember,
                        app,
                    );
                    json!({"type":"auth_updated","ok":true,"restarted":n}).to_string()
                }                "resume_all" => {
                    let n = mgr.resume_all();
                    json!({"type":"resumed","ok":true,"count":n}).to_string()
                }
                "grab_site" => {
                    let p = &v["payload"];
                    let url = p["url"].as_str().unwrap_or("").to_string();
                    let pages = p["max_pages"].as_u64().map(|n| n as usize).unwrap_or(10);
                    let kinds: Vec<String> = p["kinds"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
                    let cancel = std::sync::atomic::AtomicBool::new(false);
                    match grabber::grab_site(&url, pages, kinds, &cancel).await {
                        Ok(items) => json!({"type":"grabbed","ok":true,"items":items}).to_string(),
                        Err(e) => err(&e),
                    }
                }
                "open_grabber" => {
                    let p = &v["payload"];
                    let url = p["url"].as_str().unwrap_or("").to_string();
                    // Bring Vortex to the front and open the Site Grabber modal
                    // pre-filled with the active tab's URL.
                    let app2 = app.clone();
                    tokio::spawn(async move {
                        if let Some(win) = app2.get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.unminimize();
                            let _ = win.set_focus();
                        }
                        let _ = app2.emit("grabber-open", serde_json::json!({ "url": url }));
                    });
                    json!({"type":"ack","ok":true,"success":true,"action":"grabber_opened"}).to_string()
                }
                "remove" => {
                    let p = &v["payload"];
                    let id = p["id"].as_str().unwrap_or("").to_string();
                    let delete_file = p["delete_file"].as_bool().unwrap_or(false);
                    mgr.remove_with_file(&id, delete_file);
                    json!({"type":"removed","ok":true,"id":id}).to_string()
                }
                "list" => {
                    let mut items: Vec<serde_json::Value> = Vec::new();
                    {
                        let h = mgr.http.lock().unwrap();
                        items.extend(h.values().map(|t| {
                            let v = t.view();
                            serde_json::to_value(&v).unwrap_or_default()
                        }));
                    }
                    {
                        let y = mgr.yt.lock().unwrap();
                        items.extend(y.values().map(|t| {
                            let v = t.view();
                            serde_json::to_value(&v).unwrap_or_default()
                        }));
                    }
                    json!({"type":"list","ok":true,"items":items}).to_string()
                }
                "stats" => {
                    let st = mgr.stats();
                    let (y, f, yv, fv) = tools::status(app);
                    json!({"type":"stats","stats":st,"tools":{"ytdlp":y,"ffmpeg":f,"ytdlp_version":yv,"ffmpeg_version":fv}}).to_string()
                }
                _ => err("unknown command"),
            }
        }
    };
    if req.is_empty() {
        out
    } else if let Ok(mut j) = serde_json::from_str::<Value>(&out) {
        j["req"] = json!(req);
        j.to_string()
    } else {
        out
    }
}

async fn download_op(app: &AppHandle, mgr: &Arc<DlManager>, p: &Value) -> String {
    let url = p["url"].as_str().unwrap_or("").to_string();
    let filename = p["filename"].as_str().map(|s| s.to_string()).filter(|s| !s.trim().is_empty());
    let settings = crate::state::load_settings(app);
    let segments = p["segments"].as_u64().map(|n| n as usize).unwrap_or(settings.segments);
    let force_yt = p["via"].as_str() == Some("yt");
    if force_yt {
        let ctx = ytdlp::StreamCtx {
            referer: p["referer"].as_str().map(|s| s.to_string()),
            user_agent: p["user_agent"].as_str().map(|s| s.to_string()),
            cookies: p["cookies"].as_str().map(|s| s.to_string()),
        };
        let info = match ytdlp::fetch_info(app.clone(), url.clone(), ctx.clone()).await {
            Ok(i) => i,
            Err(e) => return err(&e),
        };
        let fmt = info
            .formats
            .iter()
            .find(|f| f.kind == "video" && f.has_video && f.has_audio && f.height == Some(720))
            .or_else(|| info.formats.iter().find(|f| f.has_video && f.has_audio))
            .or_else(|| info.formats.iter().find(|f| f.has_video));
        let Some(fmt) = fmt else { return err("no suitable format") };
        return start_ytdl(app, mgr, url, fmt.id.clone(), settings.path.clone(), false, "".into(), None, None, None, None, None, ctx).await;
    }
    let opts = download::StartOpts {
        segments,
        filename,
        categorize: settings.categorize_folders,
        start_at: p["start_at"].as_u64(),
        auto_retries: settings.auto_retries,
        proxy: settings.proxy.clone(),
        referer: p["referer"].as_str().map(|s| s.to_string()),
        cookies: p["cookies"].as_str().map(|s| s.to_string()),
        on_exists: None,
    };
    match download::start(app.clone(), url.clone(), settings.path.clone(), opts, mgr.limit.clone()).await {
        Ok(task) => {
            let id = task.id.clone();
            let view = task.view();
            mgr.add_http(task.clone());
            mgr.run_http(task);
            let _ = app.emit("downloads-changed", ());
            json!({"type":"started","ok":true,"id":id,"view":view}).to_string()
        }
        Err(e) => err(&e),
    }
}

fn settings_path(app: &AppHandle) -> String {
    crate::state::load_settings(app).path
}

async fn start_ytdl(
    app: &AppHandle,
    mgr: &Arc<DlManager>,
    url: String,
    format_id: String,
    save_path: String,
    playlist: bool,
    playlist_items: String,
    start_at: Option<u64>,
    embed_subs: Option<bool>,
    sub_langs: Option<String>,
    embed_thumbnail: Option<bool>,
    auto_subs: Option<bool>,
    ctx: ytdlp::StreamCtx,
) -> String {
    let settings = crate::state::load_settings(app);
    let embed = embed_subs.unwrap_or(settings.embed_subs);
    let langs = sub_langs.unwrap_or_else(|| settings.sub_langs.clone());
    let embed_thumb = embed_thumbnail.unwrap_or(settings.embed_thumbnail);
    match ytdlp::start(
        app.clone(),
        url,
        format_id,
        save_path,
        settings.categorize_folders,
        settings.proxy.clone(),
        playlist,
        playlist_items,
        start_at,
        embed,
        langs,
        embed_thumb,
        auto_subs.unwrap_or(false),
        ctx,
        None,
    )
    .await
    {
        Ok(task) => {
            let id = task.id.clone();
            let view = task.view();
            mgr.add_yt(task.clone());
            mgr.run_yt(task);
            let _ = app.emit("downloads-changed", ());
            json!({"type":"started","ok":true,"id":id,"view":view}).to_string()
        }
        Err(e) => err(&e),
    }
}

fn err(e: &str) -> String {
    json!({"type":"error","ok":false,"error":e}).to_string()
}

fn resume_http(mgr: &Arc<DlManager>, id: String) -> String {
    let m = mgr.http.lock().unwrap();
    match m.get(&id) {
        Some(t) => {
            t.paused.store(false, std::sync::atomic::Ordering::Relaxed);
            let c = t.clone();
            drop(m);
            mgr.run_http(c);
            json!({"type":"resumed","ok":true,"id":id}).to_string()
        }
        None => err("task not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accept_key() {
        let key = "ZGVicm91d2xva2V5aWc=";
        let expect = "3sAiWlP0M1H2vYqG0ZZaroeIfD8=";
        assert_eq!(accept_key(key), expect);
    }
}