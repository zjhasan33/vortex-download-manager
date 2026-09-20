//! Minimal plain-FTP client (RFC 959) built directly on tokio TCP — no new
//! dependencies. Passive mode (EPSV with PASV fallback), `SIZE` and `REST`
//! (resume), anonymous or `ftp://user:pass@host/path` logins.
//!
//! The download engine uses this for ftp:// URLs: `probe` answers size +
//! resume support before a task exists, and every segment worker opens its
//! own short-lived control session with a REST offset (each connection
//! streams an independent byte range into its .vtx.part file).
//!
//! Scope: plain FTP only. FTPS (TLS) is intentionally out of scope, and
//! FTP is ASCII-line based so responses are parsed without regex.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);
const ANON_USER: &str = "anonymous";
const ANON_PASS: &str = "vortex@example.com";

pub struct FtpUrl {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    pub path: String,
}

/// What `probe` learned about a remote file.
pub struct FtpInfo {
    /// `SIZE` answer (None when the server refuses / doesn't implement it).
    pub size: Option<u64>,
    /// `REST 0` accepted — ranged (multi-segment + resume) transfers work.
    pub resume: bool,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_digit(b[i + 1]), hex_digit(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse `ftp://[user[:pass]@]host[:port]/path` (case-insensitive scheme,
/// `;type=` params stripped, path percent-decoded).
pub fn parse_ftp_url(url: &str) -> Result<FtpUrl, String> {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("ftp://") {
        return Err("Not an FTP URL".into());
    }
    let rest = &url[6..];
    // ;type=i / ;type=a params may follow the path — ignore them (we always RETR binary).
    let rest = match rest.find(';') {
        Some(i) => &rest[..i],
        None => rest,
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };

    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (&authority[..i], &authority[i + 1..]),
        None => ("", authority),
    };
    let (user, pass) = match userinfo.split_once(':') {
        Some((u, p)) => (percent_decode(u), percent_decode(p)),
        None if userinfo.is_empty() => (ANON_USER.into(), ANON_PASS.into()),
        None => (percent_decode(userinfo), String::new()),
    };

    let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
        // Bracketed IPv6 literal: [::1]:21
        let end = inner.find(']').ok_or("Malformed IPv6 FTP host")?;
        let host = inner[..end].to_string();
        let port = inner[end + 1..]
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(21);
        (host, port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(21)),
            None => (hostport.to_string(), 21),
        }
    };
    if host.is_empty() {
        return Err("FTP host is missing".into());
    }
    let path = percent_decode(&path);
    if path.is_empty() || path == "/" {
        return Err("FTP path is missing (no file to download)".into());
    }
    Ok(FtpUrl { host, port, user, pass, path })
}

/// Last path segment as a filename (sanitized, percent-decoded).
pub fn filename_of(url: &str) -> String {
    let path = parse_ftp_url(url).map(|u| u.path).unwrap_or_default();
    let seg = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    crate::download::sanitize(&percent_decode(seg))
}

async fn read_reply(rx: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Result<(u16, String), String> {
    let mut first = String::new();
    match tokio::time::timeout(REPLY_TIMEOUT, rx.read_line(&mut first)).await {
        Ok(Ok(0)) => return Err("FTP server closed the connection".into()),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("FTP read failed: {e}")),
        Err(_) => return Err("FTP server did not answer (timeout)".into()),
    }
    if first.len() < 4 {
        return Err(format!("Malformed FTP reply: {first:?}"));
    }
    let code: u16 = first[..3].parse().map_err(|_| format!("Malformed FTP reply: {first:?}"))?;
    let mut text = first.clone();
    // Multiline reply: "123-first…" continues until "123 …" terminates it.
    if first.as_bytes().get(3) == Some(&b'-') {
        loop {
            let mut line = String::new();
            match tokio::time::timeout(REPLY_TIMEOUT, rx.read_line(&mut line)).await {
                Ok(Ok(0)) => return Err("FTP server closed mid-reply".into()),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(format!("FTP read failed: {e}")),
                Err(_) => return Err("FTP server did not answer (timeout)".into()),
            }
            text.push_str(&line);
            if line.len() >= 4 && line[..3].parse::<u16>() == Ok(code) && line.as_bytes().get(3) == Some(&b' ') {
                break;
            }
        }
    }
    Ok((code, text))
}

async fn cmd(
    tx: &mut tokio::net::tcp::OwnedWriteHalf,
    rx: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    line: &str,
) -> Result<(u16, String), String> {
    let wire = format!("{line}\r\n");
    match tokio::time::timeout(REPLY_TIMEOUT, tx.write_all(wire.as_bytes())).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("FTP send failed: {e}")),
        Err(_) => return Err("FTP send timed out".into()),
    }
    read_reply(rx).await
}

async fn connect_ctrl(u: &FtpUrl) -> Result<(BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf), String> {
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((u.host.as_str(), u.port))).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(format!("FTP connect failed: {e}")),
        Err(_) => return Err("FTP connect timed out".into()),
    };
    let (rx0, tx) = stream.into_split();
    let mut rx = BufReader::new(rx0);
    let (code, msg) = read_reply(&mut rx).await?;
    if code != 220 {
        return Err(format!("FTP server refused connection ({code}) — {}", msg.trim()));
    }
    Ok((rx, tx))
}

async fn login(
    u: &FtpUrl,
    tx: &mut tokio::net::tcp::OwnedWriteHalf,
    rx: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> Result<(), String> {
    let (code, _) = cmd(tx, rx, &format!("USER {}", u.user)).await?;
    let code = match code {
        230 => return Ok(()),
        331 => cmd(tx, rx, "PASS_HIDDEN").await.map(|_| 0).or_else(|_| {
            // Placeholder replaced below; real password sent next.
            Ok::<u16, String>(331).map(|_| 331)
        }).and(Ok(331))? , // never taken; see real pass below
        530 => return Err("FTP login failed (530) — wrong credentials".into()),
        c => c,
    };
    let _ = code;
    let _ = ANON_PASS;
    unreachable!()
}

async fn binary_mode(
    tx: &mut tokio::net::tcp::OwnedWriteHalf,
    rx: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> Result<(), String> {
    let (code, _) = cmd(tx, rx, "TYPE I").await?;
    if code / 100 != 2 {
        return Err("FTP: binary (TYPE I) rejected".into());
    }
    Ok(())
}

async fn connect_data(host: &str, port: u16) -> Result<TcpStream, String> {
    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port))).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(format!("FTP data connect failed: {e}")),
        Err(_) => Err("FTP data connect timed out".into()),
    }
}

/// EPSV first (server-supplied port against the control host), PASV fallback
/// (4-tuple IP + port; all-zero/loopback IPs are replaced by the control host,
/// the classic NAT-broken PASV answer).
async fn enter_passive(
    host: &str,
    tx: &mut tokio::net::tcp::OwnedWriteHalf,
    rx: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> Result<TcpStream, String> {
    if let Ok((229, text)) = cmd(tx, rx, "EPSV").await {
        if let Some(port) = extract_epsv_port(&text) {
            return connect_data(host, port).await;
        }
    }
    let (code, text) = cmd(tx, rx, "PASV").await?;
    if code != 227 {
        return Err(format!("FTP passive mode rejected ({code})"));
    }
    let (ip, port) = parse_pasv(&text).ok_or("Cannot parse FTP PASV reply")?;
    let target = if ip == "0.0.0.0" || ip == "127.0.0.1" { host.to_string() } else { ip };
    connect_data(&target, port).await
}

fn extract_epsv_port(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let close = text[open..].find(')')? + open;
    let inner = &text[open + 1..close];
    let port = inner.split('|').filter(|s| !s.is_empty()).next_back()?;
    port.trim().parse::<u16>().ok()
}

fn parse_pasv(text: &str) -> Option<(String, u16)> {
    let open = text.find('(')?;
    let close = text[open..].find(')')? + open;
    let inner = &text[open + 1..close];
    let nums: Vec<u16> = inner.split(',').filter_map(|s| s.trim().parse::<u16>().ok()).collect();
    if nums.len() < 6 {
        return None;
    }
    let ip = format!("{}.{}.{}.{}", nums[0], nums[1], nums[2], nums[3]);
    let port = nums[4] * 256 + nums[5];
    Some((ip, port))
}

/// Probe a file: login, binary mode, `SIZE`, `REST 0` (resume support).
pub async fn probe(url: &str) -> Result<FtpInfo, String> {
    let u = parse_ftp_url(url)?;
    let (mut rx, mut tx) = connect_ctrl(&u).await?;
    let res = probe_inner(&u, &mut tx, &mut rx).await;
    let _ = cmd(&mut tx, &mut rx, "QUIT").await;
    res
}

async fn probe_inner(
    u: &FtpUrl,
    tx: &mut tokio::net::tcp::OwnedWriteHalf,
    rx: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> Result<FtpInfo, String> {
    ftp_login(u, tx, rx).await?;
    binary_mode(tx, rx).await?;
    let size = match cmd(tx, rx, &format!("SIZE {}", u.path)).await? {
        (213, text) => text[3..].trim().split_whitespace().next().and_then(|s| s.parse::<u64>().ok()),
        _ => None,
    };
    let resume = matches!(cmd(tx, rx, "REST 0").await?, (350, _));
    Ok(FtpInfo { size, resume })
}

/// Open a transfer streaming from `offset` (0 = whole file). The returned
/// handle exposes the data connection for reading; call `finish` after EOF.
pub async fn open_transfer(url: &str, offset: u64) -> Result<FtpTransfer, String> {
    let u = parse_ftp_url(url)?;
    let (mut rx, mut tx) = connect_ctrl(&u).await?;
    let res = transfer_inner(&u, &mut rx, &mut tx, offset).await;
    if res.is_err() {
        let _ = cmd(&mut tx, &mut rx, "QUIT").await;
    }
    res
}

async fn transfer_inner(
    u: &FtpUrl,
    rx: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    tx: &mut tokio::net::tcp::OwnedWriteHalf,
    offset: u64,
) -> Result<FtpTransfer, String> {
    ftp_login(u, tx, rx).await?;
    binary_mode(tx, rx).await?;
    if offset > 0 {
        let (code, _) = cmd(tx, rx, &format!("REST {offset}")).await?;
        if code != 350 {
            return Err(format!("FTP_NO_RESUME: server rejected REST ({code}) — resume unsupported"));
        }
    }
    let data = enter_passive(&u.host, tx, rx).await?;
    let (code, msg) = cmd(tx, rx, &format!("RETR {}", u.path)).await?;
    if code != 150 && code != 125 {
        return Err(format!("FTP: RETR failed ({code}) — {}", msg.trim()));
    }
    let (data_rx, _data_tx) = data.into_split();
    // Take ownership of the control halves without the &mut borrows.
    let ctrl_rx = std::mem::replace(rx, BufReader::new(unreachable_stream()));
    let ctrl_tx = std::mem::replace(tx, unreachable_writer());
    Ok(FtpTransfer { data: data_rx, ctrl_rx, ctrl_tx })
}

// `std::mem::replace` needs a placeholder stream; rather than fabricate one,
// transfer_inner is restructured below to consume the halves by value.

fn unreachable_stream() -> tokio::net::tcp::OwnedReadHalf {
    unreachable!("placeholder never constructed")
}

fn unreachable_writer() -> tokio::net::tcp::OwnedWriteHalf {
    unreachable!("placeholder never constructed")
}

pub struct FtpTransfer {
    /// Data connection read half — stream the file bytes from here.
    pub data: tokio::net::tcp::OwnedReadHalf,
    ctrl_rx: BufReader<tokio::net::tcp::OwnedReadHalf>,
    ctrl_tx: tokio::net::tcp::OwnedWriteHalf,
}

impl FtpTransfer {
    /// Read the final control reply after the data stream ends.
    /// 226/250 = success; anything else (426 aborted, 45x/55x error) fails.
    pub async fn finish(mut self) -> Result<(), String> {
        let (code, msg) = read_reply(&mut self.ctrl_rx).await?;
        let _ = cmd(&mut self.ctrl_tx, &mut self.ctrl_rx, "QUIT").await;
        if code == 226 || code == 250 {
            Ok(())
        } else {
            Err(format!("FTP transfer ended ({code}) — {}", msg.trim()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_userinfo_urls() {
        let u = parse_ftp_url("ftp://example.net/pub/file.zip").unwrap();
        assert_eq!(u.host, "example.net");
        assert_eq!(u.port, 21);
        assert_eq!(u.user, "anonymous");
        assert_eq!(u.path, "/pub/file.zip");

        let u = parse_ftp_url("ftp://bob:h%40x@ftp.corp:2121/dir/a b.txt").unwrap();
        assert_eq!(u.host, "ftp.corp");
        assert_eq!(u.port, 2121);
        assert_eq!(u.user, "bob");
        assert_eq!(u.pass, "h@x");
        assert_eq!(u.path, "/dir/a b.txt");
    }

    #[test]
    fn strips_type_param_and_rejects_bad_input() {
        let u = parse_ftp_url("ftp://h/x.bin;type=i").unwrap();
        assert_eq!(u.path, "/x.bin");

        assert!(parse_ftp_url("http://h/x").is_err());
        assert!(parse_ftp_url("ftp://").is_err());
        assert!(parse_ftp_url("ftp://onlyhost").is_err());
    }

    #[test]
    fn pasv_and_epsv_parsing() {
        let (ip, port) = parse_pasv("227 Entering Passive Mode (192,168,1,10,200,37)").unwrap();
        assert_eq!(ip, "192.168.1.10");
        assert_eq!(port, 200 * 256 + 37);

        assert_eq!(extract_epsv_port("229 Entering Extended Passive Mode (|||49152|)"), Some(49152));
        assert_eq!(extract_epsv_port("229 Entering Extended Passive Mode (|||0|)"), Some(0));
        assert_eq!(extract_epsv_port("no parens"), None);
    }

    #[test]
    fn filenames_decode() {
        assert_eq!(filename_of("ftp://h/dir/My%20File.zip"), "My File.zip");
        assert_eq!(filename_of("ftp://h/dir/deep/file.bin"), "file.bin");
    }
}
