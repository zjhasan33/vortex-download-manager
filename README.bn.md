# Vortex — ডাউনলোড ম্যানেজার

[English](./README.md)

Vortex হলো **Rust (Tauri)** আর ব্রাউজার এক্সটেনশন দিয়ে বানানো একটি দ্রুত, আধুনিক,
IDM-স্টাইল ডাউনলোড ম্যানেজার। মাল্টি-কানেকশন সেগমেন্টেড ডাউনলোড, yt-dlp-ভিত্তিক
ইউটিউব/মিডিয়া টুলিং, HTTP Basic/Digest authentication, শিডিউলার, ট্রে, ক্লিপবোর্ড
মনিটর, আর পরিষ্কার ডার্ক UI — সবকিছু ফ্রি, প্রাইভেট, আর প্রথম রানে ডিপেন্ডেন্সি অটো-ম্যানেজ।

> **স্ট্রাটাস:** v1.0.1 — Windows (x64)। বিল্ডের পর NSIS installer পাওয়া যায়
> `src-tauri/target/release/bundle/nsis/` ফোল্ডারে। Installer-এ সাইন নেই, তাই
> SmartScreen warning আসবে — এটা স্বাভাবিক।

---

## ফিচারসমূহ

| ক্ষেত্র | যা পাবেন |
|---|---|
| **ডাউনলোড ইঞ্জিন** | মাল্টি-কানেকশন সেগমেন্টেড ডাউনলোড (১–৩২ সেগমেন্ট), ডায়নামিক ওয়ার্ক-স্টিলিং (`claim_chunk`), পার্মানেন্ট `.vtx.part` রিজ্যুম, async ১ MB বাফার্ড I/O, প্রতি-কানেকশন স্পিড লিমিট, অটো-রিট্রাই |
| **ইউটিউব ও মিডিয়া** | পূর্ণ yt-dlp সাপোর্ট: 4K/8K (যথাস্থানে), প্লেলিস্ট + `playlist_items` রেঞ্জ, **MP3 (128–320 kbps) / FLAC / WAV** অডিও কনভার্সন, অফিসিয়াল সাবটাইটেল এম্বেড (অটো-ক্যাপশন কখনোই ব্যবহার হয় না), আলাদা `.srt`/`.vtt` ডাউনলোড |
| **Authentication** | 401/407-এ HTTP **Basic** ও **Digest (RFC 2617)**, প্রতি-হোস্ট সেভড লগইন, `auth-required` ইভেন্ট দিয়ে অটো-লগইন, Settings-এর "Site logins" ম্যানেজার |
| **ব্রাউজার এক্সটেনশন** | MV3 (Chrome + Firefox), হোভার ডাউনলোড বার (কোয়ালিটি + সাবটাইটেল ড্রপডাউন), নয়েজ/টেলিমেট্রি ফিল্টারসহ ইন্টারসেপশন, **WS token + Origin allowlist** দিয়ে পেয়ারিং (`ws://127.0.0.1:17190`) |
| **ডেস্কটপ ইন্টিগ্রেশন** | সিস্টেম ট্রে + hide-to-tray, ক্লিপবোর্ড URL মনিটর (Win32 FFI), ডাউনলোড শেষে নোটিফিকেশন, প্রতি-ডাউনলোড `start-at` + গ্লোবাল `stop-at` শিডিউলার, কুই শেষে সাটডাউন/স্লিপ |
| **UI/UX** | ডার্ক থিম, রেসপনসিভ, Shift+Click বাল্ক অ্যাকশন, সাইট গ্র্যাবার, স্পিড চার্ট, অটো-ক্যাটাগরি ফোল্ডার, লগইন ম্যানেজার |
| **টুলস আপডেট** | এক ক্লিকে `↻ Update Tools` — দ্রুত yt-dlp সেলফ-আপডেট (১৫ সে. টাইমআউট, কখনোই হ্যাং নয়), ffmpeg শুধু missing/force করলেই রিফ্রেশ |

### রোডম্যাপ / পরিচিত ঘাটতি
- FTP/FTPS — **আপাতত নেই**
- RTMP / লাইভ-স্ট্রিম ক্যাপচার — নেই (yt-dlp যা কভার করে শুধু সেটুকু)
- প্রতি-সাইট প্রক্সি রুল (আজ গ্লোবাল একটাই প্রক্সি)

## টেক স্ট্যাক

- **Rust + Tauri 2** ব্যাকএন্ড (`src-tauri/`) — `tokio`, `reqwest`, `tauri-plugin-dialog`,
  `tauri-plugin-notification`
- **TypeScript + Vite** ফ্রন্টএন্ড (`ui/`) — ফ্রেমওয়ার্ক-বিহীন vanilla TS
- **ব্রাউজার এক্সটেনশন** (`browser-extension/`) — MV3, plain JS
- **কোনো external রানটাইম ডিপেন্ডেন্সি নেই**: হাতে লেখা `md5.rs` (RFC 2617 digest) — মোটেও
  unsafe transitive auth কোড নেই

## প্রজেক্ট লেআউট

```
├── browser-extension/   MV3 এক্সটেনশন সোর্স + dist (chrome/, firefox/)
├── scripts/             হেল্পার স্ক্রিপ্ট (release.ps1, make-icon.mjs)
├── src-tauri/           Rust ব্যাকএন্ড, Tauri কনফিগ, NSIS বান্ডলিং
│   └── src/             auth.rs, download.rs, state.rs, tools.rs, ws_server.rs, ytdlp.rs, ...
└── ui/                  TypeScript/Vite ফ্রন্টএন্ড
```

## সোর্স থেকে বিল্ড (Windows)

প্রয়োজন: **Rust (stable) + MSVC toolchain**, **Node.js 18+**, **Git**।

```powershell
# ১. আগে ফ্রন্টএন্ড (অর্ডার বাধ্যতামূলক — ব্যাকএন্ড বিল্ট UI এম্বেড করে)
cd ui
npm install
npm run build
cd ..

# ২. Rust ব্যাকএন্ড (release)
cd src-tauri
cargo build --release

# ৩. ডেভ মোড (হট-রিলোড)
& ..\ui\node_modules\.bin\tauri.cmd dev
```

**বিল্ড ও রান:** অ্যাপ বাইনারি আসে `src-tauri/target/release/vortex.exe`-এ। প্রথম মিডিয়া
ডাউনলোডে yt-dlp + ffmpeg অটো-ডাউনলোড হয় `%APPDATA%\com.vortex.downloader\tools`-এ।

**ব্রাউজার এক্সটেনশন:** `cd browser-extension && node build.mjs` → `dist/chrome` ও
`dist/firefox`; আনপ্যাকড ফোল্ডার লোড করুন, তারপর Settings → Browser extension key পপআপে
কপি করুন।

## রিলিজ ইন্সটলার বানানো (NSIS)

পুরো ফ্লো স্ক্রিপ্টে আছে: [`scripts/release.ps1`](./scripts/release.ps1)

```powershell
powershell -ExecutionPolicy Bypass -File scripts/release.ps1 -Version 1.0.1
```

এটা করবে: `tauri.conf.json`/`Cargo.toml`/`package.json`-এ version বাম্প, UI + release
বাইনারি রিবিল্ড, NSIS bundle, installer path + SHA-256 প্রিন্ট, (ঐচ্ছিক) tag। ম্যানুয়াল
ভার্সন (স্ক্রিপ্টটা যা করে):

```powershell
# আগে যেকোনো চলমান vortex.exe বন্ধ করুন (নইলে exe লক থাকবে)
Get-Process | Where-Object { $_.Path -like "*vortex.exe" } | Stop-Process -Force

cd ui; npm run build; cd ..\src-tauri          # বিল্ড অর্ডার গুরুত্বপূর্ণ
& ..\ui\node_modules\.bin\tauri.cmd build       # অবশ্যই src-tauri থেকে চালাতে হবে
# -> src-tauri\target\release\bundle\nsis\Vortex_<version>_x64-setup.exe
```

## GitHub রিলিজ পাবলিশ করা

1. **রিপো নাম নিশ্চিত/বদলান** (যদি করা না হয়ে থাকে): GitHub → repo → Settings →
   General → Repository name (`winget install GitHub.cli` + `gh auth login` এর পর
   `gh repo rename vortex-download-manager` চালিয়ে)
2. Push: `git push origin main` (origin ইতিমধ্যে নতুন নাম সেট করা আছে)
3. Tag + release notes:

```powershell
git tag v1.0.1
git push origin v1.0.1
```

4. GitHub → Releases → **Draft new release** → `v1.0.1` ট্যাগ বেছে নিন,
   `Vortex_1.0.1_x64-setup.exe` (থেকে `src-tauri\target\release\bundle\nsis\`) attach করুন,
   SHA-256 লিখুন, publish। Unsigned exe-তে SmartScreen warning আসবে — স্বাভাবিক।

## টেস্টিং

- `cargo test` — MD5 ভেক্টর + auth (Basic/Digest/param parsing) + WS handshake টেস্ট
- ম্যানুয়াল 401 ফ্লো: node টেস্ট সার্ভার স্ক্রিপ্টগুলো থাকে
  `%TEMP%\opencode\auth-test-server.js` (Basic 8769, Digest 8770 পোর্টে)

## FAQ

**সেভ পাথ থেকে ডাউনলোড আলাদা কেন?** "Sort into folders by type" চালু থাকলে ডাউনলোড
অটো-ক্যাটাগরি ফোল্ডারে যায় (`Downloads\Video`, `Downloads\Other`, …)।

**MP4 ডাউনলোডে কি ffmpeg দরকার?** শুধু merge/অডিও কনভার্সনে; নইলে দরকার নেই।

**yt-dlp কি বান্ডেল করা?** না — প্রথম ব্যবহারে নিজে নামায়, *Update Tools* দিয়ে আপডেট হয়।

## লাইসেন্স

এই রিপোজিটরিতে এখনো কোনো লাইসেন্স ঘোষণা নেই। ব্যবহারের আগে মালিকের কাছে জিজ্ঞেস করুন।