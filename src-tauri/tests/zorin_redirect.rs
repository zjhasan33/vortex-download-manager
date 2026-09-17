use futures_util::StreamExt;
use reqwest::header::{ACCEPT_RANGES, CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use tokio::time::{timeout, Duration};
use vortex_lib::download::{build_client, build_tool_client};

const URL: &str = "https://mirrors.nju.edu.cn/zorinos/18/Zorin-OS-18.1-Core-64-bit.iso";

async fn probe_with(client: &reqwest::Client, url: &str) -> Result<reqwest::Response, reqwest::Error> {
    client.get(url).header(RANGE, "bytes=0-0").send().await
}

#[tokio::test]
async fn probe_follows_redirects_and_streams() {
    let result = timeout(Duration::from_secs(120), async {
        // start(): browser-like UA first.
        let mut client = build_client("").expect("client");
        let probe = match probe_with(&client, URL).await {
            Ok(p) => p,
            Err(e) if e.is_redirect() => {
                // start(): retry once with a neutral tool UA on redirect loop.
                println!("browser UA hit redirect loop ({e}); retrying with tool UA");
                client = build_tool_client("").expect("tool client");
                probe_with(&client, URL).await.expect("tool probe failed")
            }
            Err(e) => panic!("probe failed: {e}"),
        };

        let final_url = probe.url().to_string();
        let status = probe.status();
        let accept = probe
            .headers()
            .get(ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok().map(|s| s.to_string()))
            .unwrap_or_default();
        let cr = probe
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok().map(|s| s.to_string()));
        let got_206 = status == StatusCode::PARTIAL_CONTENT;

        // Segment pinned to the resolved final URL (like start()).
        let seg = client
            .get(&final_url)
            .header(RANGE, "bytes=0-4095")
            .send()
            .await
            .expect("segment failed");
        let mut stream = seg.bytes_stream();
        let mut got = 0u64;
        while let Some(chunk) = stream.next().await {
            let c = chunk.expect("stream chunk");
            got += c.len() as u64;
            if got >= 16_384 {
                break;
            }
        }

        (final_url, status.as_u16(), got_206, accept, cr, got)
    })
    .await
    .expect("timed out");

    let (final_url, status, got_206, accept, cr, got) = result;
    println!("probe   status={status} partial={got_206} accept-ranges={accept:?}");
    println!("final   url={final_url}");
    println!("probe   content-range={cr:?}");
    println!("segment bytes-read={got}");
    assert!(got_206, "server should honour range requests");
    assert!(got > 0, "no body received from final URL");
}