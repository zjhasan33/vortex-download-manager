# Vortex — ডাউনলোড ম্যানেজার

[English](./README.md)

<p align="center">
  <img src="./vortex.png" alt="Vortex" width="420" />
</p>

<p align="center">
  <b>দ্রুত, IDM-গ্রেড ডাউনলোড ম্যানেজার — ফ্রি, প্রাইভেট, আধুনিক।</b><br/>
  Rust (Tauri) • TypeScript + Vite • ব্রাউজার এক্সটেনশন (MV3)
</p>

> **স্ট্যাটাস:** v1.3.0 — Windows (x64)। NSIS installer: `src-tauri/target/release/bundle/nsis/Vortex_1.3.0_x64-setup.exe`। Unsigned → SmartScreen warning স্বাভাবিক।

---

## ✨ হাইলাইট

- **IDM parity** — মাল্টি-কানেকশন, work-stealing, hover বার, takeover, YouTube/playlist (yt-dlp), standalone *Download File Info* (540×390, always-on-top), duplicate guard (Replace / Keep Both / Cancel).
- **Exotic streaming** — HLS `.m3u8` & DASH `.mpd` sniff → Referer/Cookie/UA সহ yt-dlp/ffmpeg → `.mp4`.
- **Scheduler & power** — per-download `Start at`, global `Stop at`, daily Start/Stop, শেষে Shutdown (60s cancel) / Sleep / Hibernate / Exit।
- **Polished UX** — টাস্কবার progress, live intercept dialog (Pause/Cancel + Minimize), EMA-smoothed speed, 16–64 MB chunk (>1 GB), AIMD 429/503।

## ফিচার

| ক্ষেত্র | যা পাবেন |
|---|---|
| **ইঞ্জিন** | 16→32 adaptive, work-stealing, `.vtx.part` resume, 1 GB+ এ 16–64 MB chunk, throttle, AIMD |
| **YouTube & media** | 4K/8K, playlist, **MP3/FLAC/WAV/Opus**, official subs, VTT/SRT, thumbnail, chapter |
| **HLS / DASH** | master detection, hover `Download Video (HLS/DASH)` |
| **Auth** | Basic & Digest, per-host saved login |
| **Extension** | MV3 Chrome/Firefox 1.3.0, hover bar, takeover, WS `ws://127.0.0.1:17190` |
| **ডেস্কটপ** | Tray, clipboard, notification + sound, scheduler, shutdown, drop box |
| **UI** | Dark theme, grabber, batch bar, sort, chart |
| **Tools** | `tools-progress` live bar, yt-dlp/ffmpeg on-demand |

## বিল্ড (Windows)

```powershell
cd ui; npm install; npm run build; cd ..
cd src-tauri; cargo build --release
..\ui\node_modules\.bin\tauri.cmd dev
```

**Extension:** `cd browser-extension && node build.mjs`

## রিলিজ (NSIS)

```powershell
powershell -ExecutionPolicy Bypass -File scripts/release.ps1 -Version 1.3.0
```

## টেস্টিং

- `cargo test` — 19 unit + 1 integration

## কৃতজ্ঞতা

**[opencode](https://opencode.ai)** দিয়ে তৈরি।

- **Muse Spark 1.2** — core implementation
- **Muse Spark 1.3** (big-pickle) — speed & streaming polish
- **Free Buff** — community testing
- **glm 5.3** — UI/UX

## লাইসেন্স

এখনো কোনো লাইসেন্স ঘোষণা নেই — ব্যবহারের আগে মালিকের অনুমতি নিন।
