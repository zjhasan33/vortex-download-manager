# Vortex — Download Manager

[বাংলা](./README.bn.md)

Vortex is a fast, modern, IDM-style download manager built with **Rust (Tauri)** and a
browser extension. Segmented multi-connection downloads, YouTube/media tooling via
**yt-dlp**, HTTP Basic/Digest authentication, scheduler, tray, clipboard monitor, and a
clean dark UI — all free, private, and dependency-managed on first run.

> **Status:** v1.0.1 — Windows (x64). NSIS installer in
> `src-tauri/target/release/bundle/nsis/` after building. Installers are unsigned, so
> SmartScreen shows a warning.

---

## Features

| Area | What you get |
|---|---|
| **Download engine** | Multi-connection segmented download (1–32 segments), dynamic work-stealing (`claim_chunk`), permanent `.vtx.part` resume, async 1 MB buffered I/O, per-connection speed limit, auto-retry |
| **YouTube & media** | Full yt-dlp power: 4K/8K where published, playlists + `playlist_items` ranges, **MP3 (128–320 kbps) / FLAC / WAV** audio conversion, official-subtitle embedding (auto-captions never used), standalone `.srt`/`.vtt` output |
| **Authentication** | HTTP **Basic** and **Digest (RFC 2617)** at 401/407, per-host saved logins, auto-login callback via `auth-required` event + Settings "Site logins" manager |
| **Browser extension** | MV3 (Chrome + Firefox), hover download bar with quality + subtitle dropdowns, download interception with telemetry/noise filtering, pairing by **WS token + Origin allowlist** on `ws://127.0.0.1:17190` |
| **Desktop integration** | System tray + hide-to-tray, clipboard URL monitor (Win32 FFI), completion notifications, `start-at` per download + `stop-at` global scheduler, shutdown/sleep after queue |
| **UI/UX** | Dark theme, responsive, Shift+Click bulk actions, site grabber, speed chart, auto-categorize folders, built-in logins manager |
| **Tools self-update** | One-click `↻ Update Tools` — fast yt-dlp self-update (15 s timeout, no pipe deadlock), ffmpeg only refreshed when missing/forced |

### Roadmap / known gaps
- FTP/FTPS — **not implemented yet**
- RTMP / live-stream capture — not implemented (yt-dlp covers a subset)
- Per-site proxy rules (single global proxy today)

## Tech stack

- **Rust + Tauri 2** backend (`src-tauri/`) — `tokio`, `reqwest`, `tauri-plugin-dialog`,
  `tauri-plugin-notification`
- **TypeScript + Vite** frontend (`ui/`) — vanilla TS app, no UI framework
- **Browser extension** (`browser-extension/`) — MV3, plain JS
- **no external runtime deps**: a hand-rolled `md5.rs` (RFC 2617 digest) and a pure MD5
  mean zero transitive unsafe auth code

## Project layout

```
├── browser-extension/   MV3 extension source + dist (chrome/, firefox/)
├── scripts/             helper scripts (release.ps1, make-icon.mjs)
├── src-tauri/           Rust backend, Tauri config, NSIS bundling
│   └── src/             auth.rs, download.rs, state.rs, tools.rs, ws_server.rs, ytdlp.rs, ...
└── ui/                  TypeScript/Vite frontend (ember-free, framework-free)
```

## Building from source (Windows)

Prerequisites: **Rust (stable) + MSVC toolchain**, **Node.js 18+**, **Git**.

```powershell
# 1. Frontend first (mandatory order — backend embeds the built UI)
cd ui
npm install
npm run build
cd ..

# 2. Rust backend (release)
cd src-tauri
cargo build --release

# 3. Dev mode (changes hot-reload)
& ..\ui\node_modules\.bin\tauri.cmd dev
```

**Build & run:** the app binary lands at `src-tauri/target/release/vortex.exe`.
On first media download, yt-dlp + ffmpeg are auto-downloaded to
`%APPDATA%\com.vortex.downloader\tools` (GitHub release assets / Gyan essentials build).

**Browser extension:** `cd browser-extension && node build.mjs` writes `dist/chrome` and
`dist/firefox`; load the unpacked folder in the browser, then copy the app's pairing key
(Settings → Browser extension key) into the popup.

## Making a release installer (NSIS)

The whole flow is wrapped in [`scripts/release.ps1`](./scripts/release.ps1):

```powershell
powershell -ExecutionPolicy Bypass -File scripts/release.ps1 -Version 1.0.1
```

It will: bump `tauri.conf.json`/`Cargo.toml`/`package.json`, rebuild UI + release binary,
bundle the NSIS installer, print the installer path + SHA-256, and (optionally) tag the
release. Manual variant (what the script does):

```powershell
# kill any running vortex.exe first (locks target/release/vortex.exe)
Get-Process | Where-Object { $_.Path -like "*vortex.exe" } | Stop-Process -Force

cd ui; npm run build; cd ..\src-tauri          # build order matters
& ..\ui\node_modules\.bin\tauri.cmd build       # must run FROM src-tauri
# -> src-tauri\target\release\bundle\nsis\Vortex_<version>_x64-setup.exe
```

## Publishing a GitHub release

1. **Rename/confirm the repo** (if not done): GitHub → repo → Settings → General →
   Repository name (or `gh repo rename vortex-download-manager` after
   `winget install GitHub.cli` + `gh auth login`).
2. Push: `git push origin main` (origin is already set to the new name).
3. Create a tag + release notes:

```powershell
git tag v1.0.1
git push origin v1.0.1
```

4. GitHub → Releases → **Draft new release** → pick `v1.0.1`, attach the
   `Vortex_1.0.1_x64-setup.exe` from `src-tauri\target\release\bundle\nsis\`, paste the
   SHA-256, publish. Unsigned exe → SmartScreen warning is expected.

## Testing

- `cargo test` — MD5 vectors + auth (Basic/Digest/param parsing) + WS handshake tests
- Manual 401 flow: `node` test server scripts live in
  `%TEMP%\opencode\auth-test-server.js` (Basic on 8769, Digest on 8770)

## FAQ

**Why is the download folder different from the save path?** Downloads are auto-routed
into category folders (`Downloads\Video`, `Downloads\Other`, …) when
"Sort into folders by type" is enabled.

**Do I need ffmpeg for MP4 downloads?** Only for merging/audio conversion; otherwise no.

**Is yt-dlp bundled?** No — fetched at first use and updated through *Update Tools*.

## License

No license declared yet in this repository. Ask the owner before reusing.