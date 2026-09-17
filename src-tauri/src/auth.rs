//! HTTP authentication: stored credentials + Basic / Digest (RFC 2617) headers.
//! Digest is implemented manually (the crate has no MD5 dependency).

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::md5;

/// A saved login for a host (matching on `host[:port]`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cred {
    pub host: String,
    pub username: String,
    pub password: String,
}

/// `host[:port]` portion of a URL (strips scheme, path and userinfo).
pub fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let hostport = rest.split('/').next().unwrap_or("");
    hostport.split('@').next_back().unwrap_or(hostport).to_string()
}

/// Path + query of a URL, for the Digest `uri` directive.
fn uri_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let idx = rest.find('/').unwrap_or(rest.len());
    let mut pathq = if idx < rest.len() { &rest[idx..] } else { "/" };
    if let Some(h) = pathq.find('#') {
        pathq = &pathq[..h];
    }
    if pathq.is_empty() {
        return "/".to_string();
    }
    pathq.to_string()
}

/// First credential whose host matches the URL host.
pub fn find_cred(creds: &[Cred], url: &str) -> Option<Cred> {
    let h = host_of(url);
    if h.is_empty() {
        return None;
    }
    creds.iter().rev().find(|c| c.host == h).cloned()
}

/// `Authorization` header value for Basic auth.
pub fn basic_auth_value(username: &str, password: &str) -> String {
    let raw = format!("{username}:{password}");
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(raw.as_bytes()))
}

/// Build the `Authorization` value for a Digest challenge.
/// `challenge` is the raw `WWW-Authenticate: Digest ...` header value.
/// Returns `None` when the challenge is malformed or unsupported (non-MD5).
pub fn digest_auth_value(method: &str, url: &str, cred: &Cred, challenge: &str) -> Option<String> {
    let params = parse_digest_params(challenge)?;
    let realm = params.get("realm")?.to_string();
    let nonce = params.get("nonce")?.to_string();
    let qop = params.get("qop").cloned().unwrap_or_default().to_lowercase();
    let algorithm = params.get("algorithm").cloned().unwrap_or_else(|| "MD5".into());
    if !algorithm.eq_ignore_ascii_case("MD5") {
        return None;
    }
    let opaque = params.get("opaque").cloned().unwrap_or_default();
    let srv_uri = params.get("uri").cloned().unwrap_or_else(|| uri_of(url));

    let ha1 = md5::md5_hex(format!("{}:{}:{}", cred.username, realm, cred.password).as_bytes());
    let ha2 = md5::md5_hex(format!("{method}:{srv_uri}").as_bytes());

    let use_qop = qop.split(',').any(|q| q.trim() == "auth");
    let cnonce = format!(
        "{:08x}",
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0) as u64 ^ 0x5c2d9e31
    );
    let nc = "00000001";

    let response = if use_qop {
        md5::md5_hex(format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}").as_bytes())
    } else {
        md5::md5_hex(format!("{ha1}:{nonce}:{ha2}").as_bytes())
    };

    let mut out = String::from("Digest ");
    push_param(&mut out, "username", &cred.username);
    push_param(&mut out, "realm", &realm);
    push_param(&mut out, "nonce", &nonce);
    push_param(&mut out, "uri", &srv_uri);
    if use_qop {
        push_param(&mut out, "qop", "auth");
        push_param(&mut out, "nc", nc);
        push_param(&mut out, "cnonce", &cnonce);
    }
    if !opaque.is_empty() {
        push_param(&mut out, "opaque", &opaque);
    }
    push_param(&mut out, "response", &response);
    if !algorithm.eq_ignore_ascii_case("MD5") {
        push_param(&mut out, "algorithm", &algorithm);
    }
    Some(out)
}

/// Parse comma-separated key=value pairs from a `WWW-Authenticate` header.
/// Handles quoted and bare values; strips surrounding quotes and the leading
/// `Digest` scheme token.
fn parse_digest_params(challenge: &str) -> Option<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    let mut body = challenge.trim();
    // Drop the "Digest" scheme token if present: `Digest realm="r", nonce="n"`.
    for prefix in ["Digest", "digest", "Basic", "basic"] {
        if let Some(rest) = body.strip_prefix(prefix) {
            body = rest.trim_start();
            if body.starts_with(',') {
                body = body.trim_start_matches(',');
            }
            break;
        }
    }
    for part in body.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = match part.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let key = k.trim().to_string();
        let val = v.trim().trim_matches('"').to_string();
        map.insert(key, val);
    }
    Some(map)
}

fn push_param(out: &mut String, key: &str, value: &str) {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    out.push_str(key);
    out.push_str("=\"");
    out.push_str(&escaped);
    out.push_str("\", ");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parsing() {
        assert_eq!(host_of("https://www.example.com/file.bin"), "www.example.com");
        assert_eq!(host_of("http://user:pass@127.0.0.1:8080/path"), "127.0.0.1:8080");
        assert_eq!(host_of("ftp://example.net/"), "example.net");
    }

    #[test]
    fn basic_value() {
        assert_eq!(
            basic_auth_value("alice", "s3cret"),
            "Basic YWxpY2U6czNjcmV0"
        );
    }

    #[test]
    fn digest_challenge_parses_first_segment() {
        // Regressed: the first comma-segment (`realm=...`) used to be dropped.
        let challenge = "Digest realm=\"testrealm@host.com\", nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", qop=\"auth\", opaque=\"5ccc069c403ebaf9f0171e9517f40e41\"";
        let p = parse_digest_params(challenge).unwrap();
        assert_eq!(p.get("realm").map(|s| s.as_str()), Some("testrealm@host.com"));
        assert_eq!(p.get("nonce").map(|s| s.as_str()), Some("dcd98b7102dd2f0e8b11d0f600bfb0c093"));
        assert_eq!(p.get("qop").map(|s| s.as_str()), Some("auth"));
        assert_eq!(p.get("opaque").map(|s| s.as_str()), Some("5ccc069c403ebaf9f0171e9517f40e41"));
    }

    #[test]
    fn digest_header_fields() {
        let challenge = "Digest realm=\"testrealm@host.com\", nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", qop=\"auth\", opaque=\"5ccc069c403ebaf9f0171e9517f40e41\"";
        let cred = Cred { host: "example.com".into(), username: "Mufasa".into(), password: "Circle Of Life".into() };
        let h = digest_auth_value("GET", "http://example.com/dir/index.html", &cred, challenge).expect("digest header");
        assert!(h.starts_with("Digest "));
        assert!(h.contains("username=\"Mufasa\""));
        assert!(h.contains("realm=\"testrealm@host.com\""));
        assert!(h.contains("uri=\"/dir/index.html\""));
        assert!(h.contains("qop=\"auth\""));
        assert!(h.contains("nc=\"00000001\""));
        assert!(h.contains("cnonce=\""));
        assert!(h.contains("opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""));
        // algorithm omitted for MD5 by default; response is 32 hex chars.
        let resp = h.split("response=\"").nth(1).unwrap().split('"').next().unwrap();
        assert_eq!(resp.len(), 32);
        assert!(resp.chars().all(|c| c.is_ascii_hexdigit()));
    }
}