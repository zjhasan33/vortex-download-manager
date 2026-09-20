# Vortex — Download Manager

[বাংলা](./README.bn.md)

<p align="center">
  <img src="./vortex.png" alt="Vortex" width="420" />
</p>

<p align="center">
  <b>Fast, IDM-grade download manager — free, private, and modern.</b><br/>
  Rust (Tauri) • TypeScript + Vite • Browser Extension (MV3)
</p>

<p align="center">
  <img alt="version" src="https://img.shields.io/badge/version-1.3.0-818cf8?style=flat-square" />
  <img alt="platform" src="https://img.shields.io/badge/platform-Windows%20x64-22d3ee?style=flat-square" />
  <img alt="license" src="https://img.shields.io/badge/license-TBD-6b7a9e?style=flat-square" />
</p>

> **Status:** v1.3.0 — Windows (x64). NSIS installer at `src-tauri/target/release/bundle/nsis/Vortex_1.3.0_x64-setup.exe` after building. Unsigned → SmartScreen warning is normal.

<p align="center">
  <img src="./vortex.png" alt="Vortex logo" width="160" style="border-radius:16px" />
</p>

---

## 📸 Screenshots

<p align="center">
  <img src="./vortex%20app.png" alt="Vortex App — Dashboard" width="860" />
  <br/><em>Dashboard — dark theme, categories, live transfer rate</em>
</p>

---

## ✨ Highlights

- **IDM parity where it matters** — segmented multi-connection, work-stealing, hover download bar, browser takeover, YouTube/playlist/media via yt-dlp, standalone *Download File Info* dialog (540×390, always-on-top), duplicate guard (Replace / Keep Both / Cancel).
- **Exotic streaming** — HLS `.m3u8` & DASH `.mpd` sniffed in the extension, forwarded with Referer/Cookies/UA to yt-dlp/ffmpeg → clean `.mp4`.
- **Scheduler & power** — per-download `Start at`, global `Stop at`, daily Start/Stop (02:00→06:00), and *On completion*: Shutdown (60s cancel window) / Sleep / Hibernate / Exit.
- **Polished UX** — taskbar progress fill, live intercept dialog (Pause/Cancel + Minimize to app), EMA-smoothed speed & graph, adaptive chunks (16–64 MB for >1 GB), AIMD 429/503 backoff, tray, clipboard monitor, speed chart.

---

## Features

| Area | What you get |
|---|---|
| **Engine** | 16→32 segments (adaptive), dynamic work-stealing, `.vtx.part` resume (kept on cancel/pause), 1 MB buffered I/O → 1 GB+ uses 16–64 MB chunks, per-connection throttle, auto-retry with 429/503 AIMD |
| **YouTube & media** | yt-dlp: 4K/8K, playlists + ranges, **MP3 128–320 / FLAC / WAV / Opus**, official subs embed (never auto-captions for “all”), **VTT/SRT** standalone, thumbnail/cover, chapters/metadata, temp-isolated fragments |
| **HLS / DASH** | `.m3u8` / `.mpd` master detection (chunk filtering), background → content `stream_detected` → hover bar `Download Video (HLS/DASH)`, all via yt-dlp |
| **Auth** | **Basic & Digest (RFC 2617)** with custom `md5.rs`, per-host saved logins, `auth-required` dialog |
| **Extension** | MV3 Chrome + Firefox 1.3.0, hover bar (draggable, opacity), `stream_detected` + `file_captured`, takeover, WS `ws://127.0.0.1:17190` with token + Origin allowlist |
| **Desktop** | System tray + hide-to-tray, clipboard monitor, notifications + sounds, scheduler, shutdown/sleep, drop box |
| **UI** | Dark theme, Shift+Click bulk, grabber (Stop + *Add to Queue Paused* / *Download Now*), emergency batch bar, sort, speed chart, categories |
| **Tools** | `Update Tools` with live `tools-progress` bars (sidebar + welcome modal), yt-dlp self-update (15 s cap), ffmpeg on-demand |

## Tech stack

- **Rust + Tauri 2** — `tokio`, `reqwest` (rustls), `tauri-plugin-dialog/notification/single-instance`
- **TypeScript + Vite** — vanilla TS, no framework
- **Extension** — MV3 plain JS, `webRequest` sniffing, `all_frames: true` + shadow-DOM piercing

```
├── browser-extension/   MV3 source + dist (chrome/, firefox/)
├── scripts/             release.ps1 helpers
├── src-tauri/           Rust backend + Tauri config + NSIS
│   └── src/             download.rs, ytdlp.rs, state.rs, tools.rs, ws_server.rs, ...
└── ui/                  TS/Vite frontend
```

## Building (Windows)

Prereqs: **Rust stable + MSVC**, **Node.js 18+**, **Git**.

```powershell
# 1. Frontend first (backend embeds the built UI)
cd ui; npm install; npm run build; cd ..

# 2. Backend (release)
cd src-tauri; cargo build --release

# 3. Dev (hot-reload)
..\ui\node_modules\.bin\tauri.cmd dev
# binary → src-tauri/target/release/vortex.exe
# first media download auto-fetches yt-dlp + ffmpeg to %APPDATA%\com.vortex.downloader\tools
```

**Extension:** `cd browser-extension && node build.mjs` → `dist/chrome` / `dist/firefox`; load unpacked, paste pairing key from *Settings → Browser extension key*.

## Release installer (NSIS)

```powershell
powershell -ExecutionPolicy Bypass -File scripts/release.ps1 -Version 1.3.0
# → src-tauri/target/release/bundle/nsis/Vortex_1.3.0_x64-setup.exe
# Manual:
Get-Process vortex -ErrorAction SilentlyContinue | Stop-Process -Force
cd ui; npm run build; cd ..\src-tauri; ..\ui\node_modules\.bin\tauri.cmd build
```

## GitHub release

```powershell
git tag v1.3.0; git push origin v1.3.0
# GitHub → Releases → Draft new release → pick v1.3.0 → attach Vortex_1.3.0_x64-setup.exe + SHA-256
```

## Testing

- `cargo test` — 19 unit + 1 integration (auth, WS, download steps, ytdlp, grabber)
- Manual 401 flow via `%TEMP%\opencode\auth-test-server.js`

## Credits

Built with **[opencode](https://opencode.ai)**.

- **Muse Spark 1.2** (`opencode/muse-spark-1.2-contributor-free`) — core implementation
- **Muse Spark 1.3** (big-pickle) — speed stabilization, HLS/DASH, scheduler polish
- **Free Buff** — community testing & feedback
- **glm 5.3** — UI/UX iteration

Thanks to **yt-dlp**, **ffmpeg (BtbN builds)**, and **Tauri**.

## License

No license declared yet — ask the owner before reusing.
